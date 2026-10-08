/* C3:真实会议室(LiveKit)。
 *
 * 为什么这一端用 JS SDK,而 Agent 那端用 Rust SDK——不是不一致,是约束相反:
 * Agent 是无头进程、要在同进程里跑 muster_route(架构文档边界五),所以必须 Rust;
 * 桌面壳的 webview 本来就是浏览器,自带 WebRTC、getUserMedia 和 <video>,
 * 用 Rust SDK 反而要把音视频帧再喂回 webview。
 *
 * **能不能开麦不是前端判的**:它来自服务端入会票里的 can_publish,
 * 而那是 muster_identity::can() 的判定结果。前端只照着显示。
 *
 * **麦克风开没开也不是前端记的**:界面上的开关状态一律读 SDK
 * (`localParticipant.isMicrophoneEnabled`),不另存一份——系统拒绝了权限、
 * 设备被拔掉、SDK 自己静音了,界面都得跟着变,自己记一份就会说谎。
 *
 * 阶段 1(语音室做扎实,见 docs/会议线规划-2026-10-08.md):
 * 进会补历史纪要、行动项走 SSE 不再轮询、设备可选、权限预检、
 * 会议被结束时界面知道;共享屏幕只做**能力探测 + 最小接线**,焦点布局是阶段 2。
 *
 * 阶段 2(对标 Teams):字幕是纪要行叠在画面上,没有第二条转写链路;字幕语言各人自选,
 * 翻译由会议 Agent 做(模型调用在能跑 muster_route 的进程里),翻译行以 origin_id 指回原文;
 * 录制由服务端调自托管的 LiveKit Egress,能不能录服务端判(restricted 禁录),
 * **正在录制必须人人看见**;回顾由 Agent 散会时生成,在会议详情页看。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import {
  ConnectionState,
  DisconnectReason,
  Room,
  RoomEvent,
  Track,
  supportsAudioOutputSelection,
  type LocalTrack,
  type RemoteTrack,
} from "livekit-client";
import {
  ArrowLeft, Bot, Captions, Check, CircleDot, FileText, Film, Mic, MicOff, Monitor, MonitorOff, PhoneOff,
  Radio, Settings2, Square, Users, Video, VideoOff, X,
} from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { api, ActionItem, MeetingSummary, Recording, RemoteMeeting, TranscriptRow, fmtBytes, fmtTime } from "../api";

export interface TranscriptLine {
  speaker: string;
  text: string;
  ts: number;
  /** 服务端行号(0 = 系统行,没有行号) */
  id: number;
  lang: string;
  /** 翻译行指回的原文行号;原文行为 null */
  originId: number | null;
}

/** 服务端的纪要行 → 界面行 */
export function toLine(r: TranscriptRow): TranscriptLine {
  return { speaker: r.speaker_id, text: r.text, ts: r.ts_ms, id: r.id, lang: r.lang, originId: r.origin_id };
}

/** 原文语言:与 Agent 的转写语言一致(服务端 SOURCE_LANG) */
export const SOURCE_LANG = "zh";
/** 字幕可选语言。代码要能过服务端校验(两三个小写字母) */
export const CAPTION_LANGS: { code: string; name: string }[] = [
  { code: "source", name: "跟原文(中文)" },
  { code: "en", name: "English" },
  { code: "ja", name: "日本語" },
  { code: "ko", name: "한국어" },
  { code: "de", name: "Deutsch" },
  { code: "fr", name: "Français" },
  { code: "es", name: "Español" },
  { code: "pt", name: "Português" },
  { code: "ru", name: "Русский" },
  { code: "vi", name: "Tiếng Việt" },
];

/** 录制状态(服务端推送);started / stopping / ready / failed */
export interface RecordingState {
  state: string;
  by: string;
}

function readPref(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}
function writePref(key: string, v: string) {
  try {
    localStorage.setItem(key, v);
  } catch {
    /* 隐私模式等 */
  }
}

/** 字幕要显示的行:跟原文 → 原文行;选了语言 → 该语言的翻译行,没翻到的句子退回原文 */
export function captionLines(all: TranscriptLine[], lang: string): TranscriptLine[] {
  if (lang === "source" || lang === SOURCE_LANG) return all.filter((l) => l.originId === null);
  const translated = new Map<number, TranscriptLine>();
  for (const l of all) if (l.lang === lang && l.originId !== null) translated.set(l.originId, l);
  return all.filter((l) => l.originId === null).map((src) => translated.get(src.id) ?? src);
}

/** 纪要面板:原文行,各自挂着我这个语言的译文 */
export function groupTranslations(all: TranscriptLine[], lang: string): { line: TranscriptLine; translation?: string }[] {
  const translated = new Map<number, string>();
  if (lang !== "source" && lang !== SOURCE_LANG) {
    for (const l of all) if (l.lang === lang && l.originId !== null) translated.set(l.originId, l.text);
  }
  return all.filter((l) => l.originId === null).map((line) => ({ line, translation: translated.get(line.id) }));
}

/** 本机媒体状态——全部读自 SDK,见文件头 */
interface LocalMedia {
  mic: boolean;
  cam: boolean;
  screen: boolean;
}

interface Devices {
  mics: MediaDeviceInfo[];
  speakers: MediaDeviceInfo[];
  cams: MediaDeviceInfo[];
}

/** 浏览器能不能共享屏幕。WKWebView(Tauri on macOS)历史上不给 getDisplayMedia,
 *  这个探测的结果本身就是阶段 2 要先做的那个试验的答案。 */
const SCREEN_SHARE_SUPPORTED =
  typeof navigator !== "undefined" &&
  typeof navigator.mediaDevices?.getDisplayMedia === "function";

/** 把 getUserMedia 一类的失败翻成人能照着做的话。错误名是 WebRTC 标准的,不是猜的。 */
function mediaErrorZh(e: unknown, what: string): string {
  const name = (e as { name?: string })?.name ?? "";
  switch (name) {
    case "NotAllowedError":
    case "PermissionDeniedError":
      return `${what}被系统拒绝。macOS:系统设置 → 隐私与安全性 → ${what},放行本应用后重试。`;
    case "NotFoundError":
    case "DevicesNotFoundError":
      return `没有找到${what}设备。接上设备后在「设备」里选它。`;
    case "NotReadableError":
    case "TrackStartError":
      return `${what}被别的程序占用(常见是另一个会议软件),关掉它再试。`;
    case "OverconstrainedError":
      return `选中的${what}设备不可用,换一个试试。`;
    default:
      return `打不开${what}:${String((e as Error)?.message ?? e)}`;
  }
}

export function MeetingRoom({
  meeting,
  transcript,
  actions,
  ended,
  recording,
  onLeave,
}: {
  meeting: RemoteMeeting;
  /** 纪要行:进会时由上层补过历史,之后由 SSE 增量推来 */
  transcript: TranscriptLine[];
  /** 行动项:由 SSE 推来(提出 / 裁决)。这里进会再拉一次全量,两者按 id 合并 */
  actions: ActionItem[];
  /** 服务端推来「这场会已结束」(别人点了结束) */
  ended: boolean;
  /** 录制状态(服务端推送);没推来过就看 meeting.recording_active */
  recording?: RecordingState | null;
  onLeave: () => void;
}) {
  const [room] = useState(() => new Room({ adaptiveStream: true, dynacast: true }));
  const [state, setState] = useState<ConnectionState>(ConnectionState.Disconnected);
  const [err, setErr] = useState<string | null>(null);
  const [canPublish, setCanPublish] = useState(false);
  const [canRecord, setCanRecord] = useState(false);
  const [recBusy, setRecBusy] = useState(false);
  const [captionsOn, setCaptionsOn] = useState(() => readPref("muster.captions", "1") === "1");
  const [capLang, setCapLang] = useState(() => readPref("muster.captionLang", "source"));
  const [now, setNow] = useState(() => Date.now());
  const [local, setLocal] = useState<LocalMedia>({ mic: false, cam: false, screen: false });
  const [roster, setRoster] = useState<Seat[]>([]);
  const [screens, setScreens] = useState<string[]>([]);
  const [wantsAgent, setWantsAgent] = useState(meeting.wants_agent);
  const [agentBusy, setAgentBusy] = useState(false);
  const [pulled, setPulled] = useState<ActionItem[]>([]);
  const [taskErr, setTaskErr] = useState<string | null>(null);
  const [devOpen, setDevOpen] = useState(false);
  const [devices, setDevices] = useState<Devices>({ mics: [], speakers: [], cams: [] });
  const [active, setActive] = useState<{ mic?: string; speaker?: string; cam?: string }>({});
  const [dropped, setDropped] = useState<string | null>(null);
  /** 谁的画面。视频轨由 LiveKit 给的是 DOM 元素,只能命令式挂;
   *  席位本身仍归 React 管——两者放在不同节点上,互不打架。
   *  共享屏幕的轨另起一个键 `screen:<identity>`,挂在焦点区而不是席位里。 */
  const videoTracks = useRef(new Map<string, RemoteTrack | LocalTrack>());
  const hosts = useRef(new Map<string, HTMLDivElement>());
  const audioSink = useRef<HTMLDivElement>(null);
  const linesEnd = useRef<HTMLDivElement>(null);
  const speakerSelectable = useMemo(() => supportsAudioOutputSelection(), []);

  useEffect(() => {
    let cancelled = false;

    const refreshLocal = () => {
      const lp = room.localParticipant;
      setLocal({
        mic: lp.isMicrophoneEnabled,
        cam: lp.isCameraEnabled,
        screen: lp.isScreenShareEnabled,
      });
    };

    const refresh = () => {
      setRoster(
        [room.localParticipant, ...room.remoteParticipants.values()].map((p) => ({
          id: p.identity,
          name: p.name || p.identity,
          isLocal: p.isLocal,
          micOn: p.isMicrophoneEnabled,
          speaking: p.isSpeaking,
          hasVideo: videoTracks.current.has(p.identity),
        })),
      );
      setScreens(
        [...videoTracks.current.keys()].filter((k) => k.startsWith("screen:")).map((k) => k.slice(7)),
      );
      refreshLocal();
    };

    const place = (key: string) => {
      const [host, track] = [hosts.current.get(key), videoTracks.current.get(key)];
      if (host && track && !host.querySelector("video")) host.appendChild(track.attach());
    };

    const keyFor = (who: string, source: Track.Source | undefined) =>
      source === Track.Source.ScreenShare ? `screen:${who}` : who;

    const attach = (track: RemoteTrack | LocalTrack, who: string, source?: Track.Source) => {
      // 同一条轨只挂一次:重连时 TrackSubscribed 会再来一遍,
      // 挂两次就是两份声音同时播——听起来像回声,却会被误当成声学回授
      if (track.attachedElements?.length) return;
      if (track.kind === Track.Kind.Video) {
        const key = keyFor(who, source);
        videoTracks.current.set(key, track);
        place(key);
      } else if (audioSink.current) {
        // 音频元素不进布局:它只是让声音出来,不属于任何一个可见席位
        const el = track.attach();
        el.style.display = "none";
        audioSink.current.appendChild(el);
      }
      refresh();
    };

    const drop = (track: RemoteTrack | LocalTrack, who?: string, source?: Track.Source) => {
      track.detach().forEach((e) => e.remove());
      if (who) videoTracks.current.delete(keyFor(who, source));
      refresh();
    };

    const loadDevices = async () => {
      try {
        const [mics, speakers, cams] = await Promise.all([
          Room.getLocalDevices("audioinput", false),
          Room.getLocalDevices("audiooutput", false),
          Room.getLocalDevices("videoinput", false),
        ]);
        if (cancelled) return;
        setDevices({ mics, speakers, cams });
        setActive({
          mic: room.getActiveDevice("audioinput"),
          speaker: room.getActiveDevice("audiooutput"),
          cam: room.getActiveDevice("videoinput"),
        });
      } catch {
        /* 枚举失败不致命:还能用系统默认设备 */
      }
    };

    room
      .on(RoomEvent.ConnectionStateChanged, (s) => setState(s))
      .on(RoomEvent.TrackSubscribed, (track, pub, p) => attach(track, p.identity, pub.source))
      .on(RoomEvent.TrackUnsubscribed, (track, pub, p) => drop(track, p.identity, pub.source))
      .on(RoomEvent.LocalTrackPublished, (pub) => {
        // 自己的画面 LiveKit 不会"订阅"给自己
        if (pub.track) attach(pub.track, room.localParticipant.identity, pub.source);
      })
      .on(RoomEvent.LocalTrackUnpublished, (pub) => {
        if (pub.track) drop(pub.track, room.localParticipant.identity, pub.source);
      })
      // 说话人高亮的唯一数据来源。概念稿里那个绿框是写死的,这个是真的
      .on(RoomEvent.ActiveSpeakersChanged, refresh)
      .on(RoomEvent.TrackMuted, refresh)
      .on(RoomEvent.TrackUnmuted, refresh)
      .on(RoomEvent.ParticipantConnected, refresh)
      .on(RoomEvent.ParticipantDisconnected, refresh)
      // 设备被拔、被插、被系统换掉:重新枚举,别让下拉框停在一个不存在的设备上
      .on(RoomEvent.MediaDevicesChanged, () => void loadDevices())
      .on(RoomEvent.ActiveDeviceChanged, (kind, id) =>
        setActive((a) =>
          kind === "audioinput" ? { ...a, mic: id } : kind === "audiooutput" ? { ...a, speaker: id } : { ...a, cam: id },
        ),
      )
      .on(RoomEvent.MediaDevicesError, (e, kind) =>
        setErr(mediaErrorZh(e, kind === "videoinput" ? "摄像头" : "麦克风")),
      )
      .on(RoomEvent.Disconnected, (reason) => {
        setRoster([]);
        setScreens([]);
        // 自己点离开不算断线;别的原因都要说出来,不能让人对着一个安静的界面猜
        if (reason !== undefined && reason !== DisconnectReason.CLIENT_INITIATED) {
          setDropped(
            reason === DisconnectReason.DUPLICATE_IDENTITY
              ? "同一账号在别处进了这场会,这边被顶下线了。"
              : reason === DisconnectReason.ROOM_DELETED
                ? "房间已被关闭。"
                : `与会议服务器的连接断开了(${DisconnectReason[reason] ?? reason})。`,
          );
        }
      });

    api
      .remoteMeetingJoin(meeting.id)
      .then(async (info) => {
        if (cancelled) return;
        setCanPublish(info.can_publish);
        setCanRecord(info.can_record);
        await room.connect(info.url, info.token);
        if (cancelled) return;
        refresh();
        void loadDevices();
        // 进会就把自己的字幕语言告诉服务端:Agent 据此决定翻成哪些语言
        const lang = readPref("muster.captionLang", "source");
        if (lang !== "source") api.remoteCaptionLang(meeting.id, lang).catch(() => {});
      })
      .catch((e) => setErr(String(e)));

    return () => {
      cancelled = true;
      room.disconnect();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [meeting.id]);

  // 行动项:进会时拉一次全量,之后靠 SSE(上层 actions)。不再轮询。
  useEffect(() => {
    let stop = false;
    api
      .remoteActionItems(meeting.id)
      .then((v) => !stop && setPulled(v))
      .catch(() => {});
    return () => {
      stop = true;
    };
  }, [meeting.id]);

  // 会议被别人结束:断开房间,界面给出口。不自动跳走——人要看见"为什么没声音了"。
  useEffect(() => {
    if (ended) room.disconnect();
  }, [ended, room]);

  // 新纪要行滚到底
  useEffect(() => {
    linesEnd.current?.scrollIntoView({ block: "end" });
  }, [transcript.length]);

  // 字幕按时间淡出,需要一个走着的钟
  useEffect(() => {
    if (!captionsOn) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [captionsOn]);

  const chooseCaptionLang = (lang: string) => {
    setCapLang(lang);
    writePref("muster.captionLang", lang);
    api.remoteCaptionLang(meeting.id, lang).catch((e) => setErr(String(e)));
  };
  const toggleCaptions = () => {
    const next = !captionsOn;
    setCaptionsOn(next);
    writePref("muster.captions", next ? "1" : "0");
  };

  const recState = recording?.state ?? (meeting.recording_active ? "started" : "idle");
  const recOn = recState === "started" || recState === "stopping";
  const toggleRecording = async () => {
    setErr(null);
    setRecBusy(true);
    try {
      if (recOn) {
        await api.remoteRecordingStop(meeting.id);
      } else {
        // 此刻没人有画面就只录声音(语音室省掉合成画面的开销);中途开了摄像头也不会补录画面
        const anyVideo = roster.some((p) => p.hasVideo) || screens.length > 0;
        await api.remoteRecordingStart(meeting.id, !anyVideo);
      }
    } catch (e) {
      setErr(`录制:${String(e)}`);
    } finally {
      setRecBusy(false);
    }
  };

  /** 全量 ⊕ 推送,按 id 合并;推送来的是更新的状态,盖掉拉到的那份 */
  const tasks = useMemo(() => {
    const m = new Map<string, ActionItem>();
    for (const t of pulled) m.set(t.id, t);
    for (const t of actions) m.set(t.id, t);
    return [...m.values()].sort((a, b) => a.created_ms - b.created_ms);
  }, [pulled, actions]);

  const decide = async (id: string, confirm: boolean) => {
    setTaskErr(null);
    try {
      const updated = await api.remoteDecideAction(id, confirm);
      setPulled((v) => (v.some((t) => t.id === id) ? v.map((t) => (t.id === id ? updated : t)) : [...v, updated]));
    } catch (e) {
      // 多半是没有 CreateTask 权限。**照服务端说的显示**,不自己编理由
      setTaskErr(String(e));
    }
  };

  /** 权限预检:能问就先问,免得点了开麦等半天才弹系统拒绝。
   *  WKWebView 可能不支持 permissions.query——那就跳过预检,交给 getUserMedia 报错。 */
  const precheck = async (name: "microphone" | "camera", what: string): Promise<boolean> => {
    try {
      const st = await navigator.permissions?.query({ name: name as PermissionName });
      if (st?.state === "denied") {
        setErr(`${what}权限已被拒绝。macOS:系统设置 → 隐私与安全性 → ${what},放行本应用后重试。`);
        return false;
      }
    } catch {
      /* 不支持预检就直接试 */
    }
    return true;
  };

  const toggleMic = async () => {
    setErr(null);
    const next = !room.localParticipant.isMicrophoneEnabled;
    if (next && !(await precheck("microphone", "麦克风"))) return;
    try {
      // 关麦 = SDK 默认停掉底层音轨(stopMicTrackOnMute),系统的麦克风指示灯会灭——是真静音,不是只不发包
      await room.localParticipant.setMicrophoneEnabled(next);
    } catch (e) {
      setErr(mediaErrorZh(e, "麦克风"));
    }
    setLocal((l) => ({ ...l, mic: room.localParticipant.isMicrophoneEnabled }));
  };
  const toggleCam = async () => {
    setErr(null);
    const next = !room.localParticipant.isCameraEnabled;
    if (next && !(await precheck("camera", "摄像头"))) return;
    try {
      await room.localParticipant.setCameraEnabled(next);
    } catch (e) {
      setErr(mediaErrorZh(e, "摄像头"));
    }
    setLocal((l) => ({ ...l, cam: room.localParticipant.isCameraEnabled }));
  };
  const toggleScreen = async () => {
    setErr(null);
    const next = !room.localParticipant.isScreenShareEnabled;
    try {
      await room.localParticipant.setScreenShareEnabled(next, { audio: false });
    } catch (e) {
      setErr(mediaErrorZh(e, "屏幕共享"));
    }
    setLocal((l) => ({ ...l, screen: room.localParticipant.isScreenShareEnabled }));
  };

  const switchDevice = async (kind: MediaDeviceKind, id: string) => {
    setErr(null);
    try {
      const ok = await room.switchActiveDevice(kind, id, true);
      if (!ok) setErr("没能切换到这个设备。");
    } catch (e) {
      setErr(mediaErrorZh(e, kind === "videoinput" ? "摄像头" : kind === "audiooutput" ? "扬声器" : "麦克风"));
    }
  };

  /** 设备名在授权前是空的(浏览器规则)。这个按钮只为拿到设备名,拿到后立刻停掉轨。 */
  const grantFor = async (kind: "audioinput" | "videoinput") => {
    setErr(null);
    try {
      const list = await Room.getLocalDevices(kind, true);
      setDevices((d) => (kind === "audioinput" ? { ...d, mics: list } : { ...d, cams: list }));
      // 授权后扬声器名也会出现
      setDevices((d) => d);
      Room.getLocalDevices("audiooutput", false).then((speakers) => setDevices((d) => ({ ...d, speakers }))).catch(() => {});
    } catch (e) {
      setErr(mediaErrorZh(e, kind === "videoinput" ? "摄像头" : "麦克风"));
    }
  };

  const leave = () => {
    // 先告诉服务端(记 meeting.leave),再断房间。告诉不成也要走——离开不能被服务端卡住
    api.remoteMeetingLeave(meeting.id).catch(() => {});
    room.disconnect();
    onLeave();
  };

  /** 把某个席位 / 焦点区的画面容器登记下来,并在轨已就绪时立刻挂上。
   *
   *  ref 回调每次渲染都是新函数,React 会先用 null 调一次、再用元素调一次——
   *  所以必须靠"已经有 <video> 就不再挂"来防重复,不能假设只会调用一次。
   *  `track.attach()` 每调一次就新建一个元素,挂两次就是两份画面。 */
  const mountHost = (key: string, el: HTMLDivElement | null) => {
    if (!el) return;
    hosts.current.set(key, el);
    const t = videoTracks.current.get(key);
    if (t && !el.querySelector("video")) el.appendChild(t.attach());
  };

  const connected = state === ConnectionState.Connected;
  // Agent 是不是一个在场的参会者。名字来自它的账号 id / 显示名,
  // 两者都认——部署方可能改过显示名。
  const agentHere = roster.some((p) => p.name === "A-007" || p.name === "小七");
  const levelTone = meeting.level === "restricted" ? "red" : meeting.level === "internal" ? "amb" : undefined;
  const publishOk = connected && canPublish && !ended;
  const nameOf = (id: string) => roster.find((p) => p.id === id)?.name ?? id;
  const unnamed = (list: MediaDeviceInfo[]) => list.length > 0 && list.every((d) => !d.label);
  // 字幕:最近 10 秒内的两行,按我选的语言
  const captions = captionsOn
    ? captionLines(transcript, capLang).filter((l) => now - l.ts < 10_000).slice(-2)
    : [];
  const grouped = groupTranslations(transcript, capLang);

  return (
    <div className="px-7 pb-8 pt-2" style={{ display: "grid", gridTemplateColumns: "1.5fr 1fr", gap: 16 }}>
      <div className="flex flex-col gap-4">
        <Card className="p-5">
          <div className="flex items-center gap-2.5">
            <span className="w-2 h-2 rounded-full" style={{ background: connected ? T.green : T.faint }} />
            <b className="text-[15px]">{meeting.title}</b>
            <Tag tone={levelTone as never}>{meeting.level}</Tag>
            {meeting.level === "restricted" && (
              <span className="text-[10.5px]" style={{ color: T.red }}>
                高密级会议:禁止录制,转写只走本地模型
              </span>
            )}
            {recOn && (
              <span className="flex items-center gap-1 text-[10.5px] font-bold px-2 py-0.5 rounded-md"
                style={{ background: T.redSoft, color: T.red }}
                title={recording?.by ? `由 ${recording.by} 开始` : "正在录制"}>
                <CircleDot size={11} className="rec-dot" /> {recState === "stopping" ? "录制保存中…" : "正在录制"}
              </span>
            )}
            {recState === "ready" && (
              <span className="text-[10.5px] font-semibold px-2 py-0.5 rounded-md" style={{ background: T.greenSoft, color: T.green }}>
                录像已保存
              </span>
            )}
            {recState === "failed" && (
              <span className="text-[10.5px] font-semibold px-2 py-0.5 rounded-md" style={{ background: T.redSoft, color: T.red }}>
                录制失败
              </span>
            )}
            <span className="ml-auto flex items-center gap-1.5 text-[11px]" style={{ color: T.sub }}>
              <Users size={13} /> {roster.length} 人在场
            </span>
          </div>

          <div className="text-[11px] mt-1.5" style={{ color: T.faint }}>
            {connected ? `已入房间 ${meeting.room}` : state === ConnectionState.Connecting ? "连接中…" : "未连接"}
          </div>

          {ended && (
            <div className="mt-3 px-3 py-2.5 rounded-xl text-[11.5px] flex items-center gap-3"
              style={{ background: T.redSoft, color: T.red }}>
              <span className="flex-1">这场会已经结束(有人点了「结束」)。纪要与待批任务保留,下面仍可看。</span>
              <button onClick={leave} className="text-[11px] font-semibold px-3 py-1 rounded-lg" style={{ background: "#fff", color: T.red }}>
                回到大厅
              </button>
            </div>
          )}
          {!ended && dropped && (
            <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>
              {dropped}
            </div>
          )}
          {err && (
            <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>
              {err}
            </div>
          )}

          {/* 焦点区:有人共享屏幕时出现。只做最小接线——谁共享就放谁的,多人共享并排;
              真正的焦点布局(放大、切换、画中画)是阶段 2。 */}
          {screens.length > 0 && (
            <div className="mt-3.5 grid gap-3" style={{ gridTemplateColumns: `repeat(${screens.length}, 1fr)` }}>
              {screens.map((who) => (
                <div key={who} className="rounded-2xl overflow-hidden" style={{ background: "#111", border: `1px solid ${T.line}` }}>
                  <div className="relative" style={{ aspectRatio: "16 / 9" }}>
                    <div ref={(el) => mountHost(`screen:${who}`, el)} className="absolute inset-0 [&>video]:w-full [&>video]:h-full [&>video]:object-contain" />
                  </div>
                  <div className="flex items-center gap-1.5 px-2.5 py-1.5 text-[11px] font-semibold" style={{ background: "#fff", borderTop: `1px solid ${T.line}` }}>
                    <Monitor size={11} style={{ color: T.indigo }} /> {nameOf(who)} 正在共享屏幕
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* 席位。发言时描边变绿,数据来自 LiveKit 的 ActiveSpeakersChanged。
              放画面的那个 div **没有 React 子节点**——LiveKit 往里 appendChild,
              React 管别处,两边不会互相拆台。字幕叠在这一区的底部。 */}
          <div className="relative">
          <div className="mt-3.5 grid grid-cols-2 gap-3">
            {roster.map((p) => (
              <div
                key={p.id}
                className="rounded-2xl overflow-hidden"
                style={{
                  background: T.panel,
                  border: `2px solid ${p.speaking ? T.green : "transparent"}`,
                  boxShadow: p.speaking ? `0 0 0 3px ${T.green}22` : undefined,
                  transition: "border-color .12s, box-shadow .12s",
                }}
              >
                <div className="relative" style={{ aspectRatio: "4 / 3" }}>
                  <div ref={(el) => mountHost(p.id, el)} className="absolute inset-0 [&>video]:w-full [&>video]:h-full [&>video]:object-cover" />
                  {!p.hasVideo && (
                    <div className="absolute inset-0 flex items-center justify-center">
                      <div
                        className="w-11 h-11 rounded-full flex items-center justify-center text-[17px] font-bold text-white"
                        style={{ background: T.indigo }}
                      >
                        {[...p.name][0] ?? "?"}
                      </div>
                    </div>
                  )}
                </div>
                <div
                  className="flex items-center gap-1.5 px-2.5 py-1.5"
                  style={{ background: "#fff", borderTop: `1px solid ${T.line}` }}
                >
                  <span className="text-[11.5px] font-semibold truncate" title={p.name}>
                    {p.name}
                  </span>
                  {p.isLocal && <span className="text-[9.5px]" style={{ color: T.faint }}>(你)</span>}
                  <span className="ml-auto">
                    {p.micOn ? (
                      <Mic size={11} style={{ color: T.green }} />
                    ) : (
                      <MicOff size={11} style={{ color: T.faint }} />
                    )}
                  </span>
                </div>
              </div>
            ))}
          </div>
          {captionsOn && captions.length > 0 && (
            <div className="absolute left-2 right-2 bottom-2 rounded-xl px-3.5 py-2 flex flex-col gap-1 pointer-events-none"
              style={{ background: "rgba(17,18,28,.78)", color: "#fff", backdropFilter: "blur(4px)" }}>
              {captions.map((l) => (
                <div key={`${l.id}-${l.ts}`} className="text-[13px] leading-snug">
                  <b style={{ color: "#B9B9FF" }}>{l.speaker}</b>
                  <span className="mx-1.5" style={{ color: "#8f91a8" }}>·</span>
                  {l.text}
                </div>
              ))}
            </div>
          )}
          </div>
          {connected && !roster.some((p) => p.hasVideo) && screens.length === 0 && (
            <div className="text-[10.5px] mt-2" style={{ color: T.faint }}>
              没有人开摄像头。语音是通的——谁在说话,席位的描边会变绿。
            </div>
          )}

          <div className="flex items-center gap-2 mt-4 flex-wrap">
            <button
              onClick={toggleMic}
              disabled={!publishOk}
              title={canPublish ? (local.mic ? "关麦:底层音轨会停掉,系统麦克风指示灯会灭" : "开麦") : "你在这个频道没有发言权限(由服务端权限内核判定)"}
              className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{
                background: local.mic ? T.indigo : T.soft,
                color: local.mic ? "#fff" : T.sub,
                opacity: publishOk ? 1 : 0.45,
              }}
            >
              {local.mic ? <Mic size={13} /> : <MicOff size={13} />} {local.mic ? "麦克风开" : "开麦"}
            </button>
            <button
              onClick={toggleCam}
              disabled={!publishOk}
              className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{
                background: local.cam ? T.indigo : T.soft,
                color: local.cam ? "#fff" : T.sub,
                opacity: publishOk ? 1 : 0.45,
              }}
            >
              {local.cam ? <Video size={13} /> : <VideoOff size={13} />} {local.cam ? "摄像头开" : "开摄像头"}
            </button>
            <button
              onClick={toggleScreen}
              disabled={!publishOk || !SCREEN_SHARE_SUPPORTED}
              title={
                SCREEN_SHARE_SUPPORTED
                  ? "共享整个屏幕或某个窗口(不带系统声音)"
                  : "这个 webview 不提供 getDisplayMedia,共享屏幕在此环境不可用——阶段 2 的试验结论之一"
              }
              className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{
                background: local.screen ? T.indigo : T.soft,
                color: local.screen ? "#fff" : T.sub,
                opacity: publishOk && SCREEN_SHARE_SUPPORTED ? 1 : 0.45,
              }}
            >
              {local.screen ? <MonitorOff size={13} /> : <Monitor size={13} />} {local.screen ? "停止共享" : "共享屏幕"}
            </button>
            <button
              onClick={toggleCaptions}
              aria-pressed={captionsOn}
              title={agentHere ? "字幕 = 会议 Agent 的实时转写叠在画面上" : "字幕来自会议 Agent 的转写——它不在会中时不会有字幕"}
              className="flex items-center gap-1.5 text-xs font-semibold px-3 py-2 rounded-xl"
              style={{ background: captionsOn ? T.indigoSoft : T.soft, color: captionsOn ? T.indigoDeep : T.sub }}
            >
              <Captions size={13} /> 字幕
            </button>
            <select
              value={capLang}
              onChange={(e) => chooseCaptionLang(e.target.value)}
              disabled={!connected}
              title="字幕语言。选了别的语言,会议 Agent 会把每句话翻过来(经密级路由,高密级会只用本地模型)"
              className="text-xs font-semibold px-2.5 py-2 rounded-xl outline-none"
              style={{ background: T.soft, color: T.sub, opacity: connected ? 1 : 0.45 }}
            >
              {CAPTION_LANGS.map((l) => (
                <option key={l.code} value={l.code}>{l.name}</option>
              ))}
            </select>
            {canRecord && (
              <button
                onClick={toggleRecording}
                disabled={!connected || recBusy || ended || recState === "stopping"}
                title={recOn ? "停止录制:文件收口后在会议详情里回放" : "录制整场会(自托管 LiveKit Egress 写到服务器本地)。所有人都会看见「正在录制」"}
                className="flex items-center gap-1.5 text-xs font-semibold px-3 py-2 rounded-xl"
                style={{
                  background: recOn ? T.red : T.soft,
                  color: recOn ? "#fff" : T.sub,
                  opacity: !connected || recBusy || ended ? 0.45 : 1,
                }}
              >
                {recOn ? <Square size={12} /> : <CircleDot size={13} />} {recBusy ? "…" : recOn ? "停止录制" : "录制"}
              </button>
            )}
            <button
              onClick={() => setDevOpen((v) => !v)}
              disabled={!connected}
              aria-pressed={devOpen}
              title="选择麦克风 / 扬声器 / 摄像头"
              className="flex items-center gap-1.5 text-xs font-semibold px-3 py-2 rounded-xl"
              style={{ background: devOpen ? T.indigoSoft : T.soft, color: T.sub, opacity: connected ? 1 : 0.45 }}
            >
              <Settings2 size={13} /> 设备
            </button>
            <button
              onClick={leave}
              className="ml-auto flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{ background: T.redSoft, color: T.red }}
            >
              <PhoneOff size={13} /> 离开
            </button>
          </div>

          {devOpen && (
            <div className="mt-3 rounded-xl px-3.5 py-3 grid gap-2.5" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
              <DeviceRow
                label="麦克风"
                list={devices.mics}
                value={active.mic}
                onChange={(id) => switchDevice("audioinput", id)}
                note={unnamed(devices.mics) ? "授权后才看得到设备名" : undefined}
                onGrant={unnamed(devices.mics) ? () => grantFor("audioinput") : undefined}
              />
              <DeviceRow
                label="扬声器"
                list={devices.speakers}
                value={active.speaker}
                disabled={!speakerSelectable}
                onChange={(id) => switchDevice("audiooutput", id)}
                note={speakerSelectable ? undefined : "这个 webview 不支持选择输出设备,声音跟随系统默认输出"}
              />
              <DeviceRow
                label="摄像头"
                list={devices.cams}
                value={active.cam}
                onChange={(id) => switchDevice("videoinput", id)}
                note={unnamed(devices.cams) ? "授权后才看得到设备名" : undefined}
                onGrant={unnamed(devices.cams) ? () => grantFor("videoinput") : undefined}
              />
              <div className="text-[10px]" style={{ color: T.faint }}>
                共享屏幕:{SCREEN_SHARE_SUPPORTED ? "此环境支持" : "此环境不支持(getDisplayMedia 不存在)"} · 选择输出设备:{speakerSelectable ? "支持" : "不支持"}
              </div>
            </div>
          )}

          {!canPublish && connected && (
            <div className="text-[10.5px] mt-2" style={{ color: T.sub }}>
              你只能旁听:发言权限由服务端的权限内核判定,不是这个界面决定的。
            </div>
          )}
          {!canRecord && connected && meeting.level === "restricted" && (
            <div className="text-[10.5px] mt-2" style={{ color: T.faint }}>
              高密级会议禁止录制——这是密级决定的,不是权限。
            </div>
          )}
        </Card>

        {/* 音频元素要在 DOM 里才播,但它不属于任何一个可见席位 */}
        <div ref={audioSink} hidden />
      </div>

      {/* 实时纪要:由会议 Agent 转写后落库,再经 SSE 推来 */}
      <div className="flex flex-col gap-4" style={{ minWidth: 0 }}>
      {tasks.length > 0 && (
        <Card className="px-5 pt-4 pb-3">
          <div className="flex items-center gap-2">
            <b className="text-[13px]">待批任务</b>
            <span
              className="text-[10px] font-semibold px-2 py-0.5 rounded-md"
              style={{
                background: tasks.some((t) => t.status === "proposed") ? T.redSoft : T.soft,
                color: tasks.some((t) => t.status === "proposed") ? T.red : T.sub,
              }}
            >
              {tasks.filter((t) => t.status === "proposed").length || "无"} 条待批
            </span>
            <span className="ml-auto text-[10px]" style={{ color: T.faint }}>
              Agent 提案 · 需人确认 · 实时推送
            </span>
          </div>

          {taskErr && (
            <div
              className="mt-2 px-3 py-2 rounded-xl text-[11px]"
              style={{ background: T.redSoft, color: T.red }}
            >
              {taskErr}
            </div>
          )}

          <div className="mt-2.5 flex flex-col gap-2">
            {tasks.map((t) => {
              const pending = t.status === "proposed";
              return (
                <div
                  key={t.id}
                  className="rounded-xl px-3 py-2.5"
                  style={{ border: `1px solid ${T.line}`, opacity: pending ? 1 : 0.6 }}
                >
                  <div className="text-[12px] leading-relaxed">
                    {t.text}
                    {t.owner_hint && (
                      <span style={{ color: T.sub }}> — {t.owner_hint}</span>
                    )}
                  </div>
                  {/* 出处原话:人要能核对它是不是听岔了 */}
                  {t.source_quote && (
                    <div className="text-[10.5px] mt-1 leading-relaxed" style={{ color: T.faint }}>
                      原话:{t.source_quote}
                    </div>
                  )}
                  <div className="flex items-center gap-1.5 mt-2">
                    {pending ? (
                      <>
                        <button
                          onClick={() => decide(t.id, true)}
                          className="flex items-center gap-1 text-[11px] font-semibold px-3 py-1 rounded-lg"
                          style={{ background: T.green, color: "#fff" }}
                        >
                          <Check size={11} /> 批准
                        </button>
                        <button
                          onClick={() => decide(t.id, false)}
                          className="flex items-center gap-1 text-[11px] font-semibold px-3 py-1 rounded-lg"
                          style={{ background: T.soft, color: T.sub }}
                        >
                          <X size={11} /> 驳回
                        </button>
                      </>
                    ) : (
                      <span
                        className="text-[10.5px] font-semibold"
                        style={{ color: t.status === "confirmed" ? T.green : T.faint }}
                      >
                        {t.status === "confirmed" ? "已批准" : "已驳回"}
                        {t.decided_by ? ` · ${t.decided_by}` : ""}
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </Card>
      )}

      <Card className="px-5 pt-4 pb-2 flex flex-col" style={{ maxHeight: 560 }}>
        <div className="flex items-center gap-2">
          <Radio size={13} style={{ color: agentHere ? T.indigo : T.faint }} />
          <b className="text-[13px]">实时纪要</b>
          {/* **Agent 在不在必须看得见。** 它退出了而界面无声无息,
              人只会觉得"说了没反应"——真机上就撞过这一次。 */}
          <span
            className="text-[10px] font-semibold px-2 py-0.5 rounded-md"
            style={{
              background: agentHere ? T.greenSoft : T.redSoft,
              color: agentHere ? T.green : T.red,
            }}
          >
            {agentHere ? "Agent 在会中" : "Agent 不在会中"}
          </span>
          <span className="ml-auto text-[10px]" style={{ color: T.faint }}>
            本地转写 · {transcript.length} 行
          </span>
        </div>
        {!agentHere && !ended && (
          <div className="mt-2 px-3 py-2 rounded-xl text-[11px] leading-relaxed"
            style={{ background: wantsAgent ? T.soft : T.redSoft, color: wantsAgent ? T.sub : T.red }}>
            {wantsAgent ? (
              <>已请 Agent,等它进来(服务端会立刻推给常驻服务;若一直不来,检查服务器上的 agent-daemon 是否在跑)。</>
            ) : (
              <>会议 Agent 不在这场会里,<b>说话不会被转写</b>。</>
            )}
          </div>
        )}
        {!ended && (
          <button
            disabled={agentBusy}
            onClick={() => {
              const next = !wantsAgent;
              setAgentBusy(true);
              api
                .remoteMeetingAgent(meeting.id, next)
                .then(() => setWantsAgent(next))
                .catch((e) => setErr(String(e)))
                .finally(() => setAgentBusy(false));
            }}
            className="mt-2.5 w-full flex items-center justify-center gap-1.5 text-xs font-semibold py-2 rounded-xl"
            style={{
              background: wantsAgent ? T.soft : T.indigo,
              color: wantsAgent ? T.sub : "#fff",
              opacity: agentBusy ? 0.5 : 1,
            }}
            title="按钮只在服务端记一个意愿;真正入会的是服务器上常驻的 agent-daemon——桌面壳自己起一个的话,两个人开会就有两个 Agent 各转各的"
          >
            <Bot size={13} />
            {agentBusy ? "…" : wantsAgent ? "请 Agent 离开" : "请 Agent 来记录"}
          </button>
        )}
        <div className="mt-2 overflow-y-auto flex-1">
          {transcript.length === 0 ? (
            <div className="py-6 text-[11.5px] leading-relaxed" style={{ color: T.sub }}>
              还没有转写。
              <br />
              需要会议 Agent 在这场会里——它进房间后,谁说的话都会出现在这里。
              <br />
              <span style={{ color: T.faint }}>
                转写走本地 whisper,音频不出内网(演习期云端 STT 会被直接拒绝)。
              </span>
            </div>
          ) : (
            grouped.map(({ line: l, translation }, i) => (
              <div key={`${l.id}-${l.ts}-${i}`} className="py-1.5" style={{ borderTop: i ? `1px solid ${T.line}` : undefined }}>
                <div className="flex items-baseline gap-2">
                  <b className="text-[11.5px]" style={{ color: l.speaker === "系统" ? T.red : T.indigoDeep }}>
                    {l.speaker}
                  </b>
                  <span className="text-[9.5px]" style={{ color: T.faint }}>{fmtTime(l.ts)}</span>
                </div>
                <div className="text-[12px] mt-0.5 leading-relaxed">{l.text}</div>
                {translation && (
                  <div className="text-[11.5px] mt-0.5 leading-relaxed" style={{ color: T.sub }}>{translation}</div>
                )}
              </div>
            ))
          )}
          <div ref={linesEnd} />
        </div>
      </Card>
      </div>
    </div>
  );
}

function DeviceRow({
  label,
  list,
  value,
  disabled,
  note,
  onChange,
  onGrant,
}: {
  label: string;
  list: MediaDeviceInfo[];
  value?: string;
  disabled?: boolean;
  note?: string;
  onChange: (id: string) => void;
  onGrant?: () => void;
}) {
  return (
    <div className="grid items-center gap-2" style={{ gridTemplateColumns: "56px 1fr auto" }}>
      <span className="text-[11px] font-semibold" style={{ color: T.sub }}>{label}</span>
      <select
        value={value ?? ""}
        disabled={disabled || list.length === 0}
        onChange={(e) => e.target.value && onChange(e.target.value)}
        className="text-[11.5px] px-2.5 py-1.5 rounded-lg outline-none"
        style={{ background: "#fff", border: `1px solid ${T.line}`, opacity: disabled || list.length === 0 ? 0.6 : 1 }}
      >
        {list.length === 0 && <option value="">{disabled ? "跟随系统" : "没有找到设备"}</option>}
        {list.map((d, i) => (
          <option key={d.deviceId || i} value={d.deviceId}>
            {d.label || `${label} ${i + 1}`}
          </option>
        ))}
      </select>
      <span className="text-[10px]" style={{ color: T.faint }}>
        {onGrant ? (
          <button onClick={onGrant} className="font-semibold px-2 py-1 rounded-lg" style={{ background: T.soft, color: T.sub }}>
            授权以显示名称
          </button>
        ) : (
          note ?? ""
        )}
      </span>
    </div>
  );
}

interface Seat {
  id: string;
  name: string;
  isLocal: boolean;
  micOn: boolean;
  speaking: boolean;
  hasVideo: boolean;
}

/** 极简 Markdown:只认 `## 标题`、`- 条目`、`**粗体**`——回顾就这几样,不引库 */
export function Recap({ text }: { text: string }) {
  const bold = (s: string) =>
    s.split(/(\*\*[^*]+\*\*)/g).map((part, i) =>
      part.startsWith("**") && part.endsWith("**") ? <b key={i}>{part.slice(2, -2)}</b> : <span key={i}>{part}</span>,
    );
  return (
    <div className="text-[12px] leading-relaxed">
      {text.split("\n").map((raw, i) => {
        const line = raw.trimEnd();
        if (!line.trim()) return <div key={i} className="h-1.5" />;
        if (line.startsWith("## ")) return <div key={i} className="font-bold text-[12.5px] mt-2.5 mb-0.5" style={{ color: T.indigoDeep }}>{line.slice(3)}</div>;
        if (line.startsWith("# ")) return <div key={i} className="font-bold text-[13px] mt-2">{line.slice(2)}</div>;
        if (/^[-*•]\s/.test(line)) return <div key={i} className="flex gap-1.5 pl-1"><span style={{ color: T.faint }}>•</span><span>{bold(line.replace(/^[-*•]\s/, ""))}</span></div>;
        if (/^\d+\.\s/.test(line)) return <div key={i} className="pl-1">{bold(line)}</div>;
        return <div key={i}>{bold(line)}</div>;
      })}
    </div>
  );
}

/** 会议详情:回顾、行动项、录像、纪要。进行中的会也能看(回顾在散会后才有)。 */
export function MeetingDetail({
  meeting,
  version,
  onBack,
  onEnter,
}: {
  meeting: RemoteMeeting;
  /** 服务端推来总结 / 录制变化时递增,据此重拉 */
  version: number;
  onBack: () => void;
  onEnter?: (m: RemoteMeeting) => void;
}) {
  const [summaries, setSummaries] = useState<MeetingSummary[]>([]);
  const [lines, setLines] = useState<TranscriptLine[]>([]);
  const [tasks, setTasks] = useState<ActionItem[]>([]);
  const [recs, setRecs] = useState<Recording[]>([]);
  const [urls, setUrls] = useState<Record<string, string>>({});
  const [err, setErr] = useState<string | null>(null);
  const [lang] = useState(() => readPref("muster.captionLang", "source"));

  useEffect(() => {
    let stop = false;
    setErr(null);
    Promise.all([
      api.remoteSummaries(meeting.id).catch((e) => { setErr(String(e)); return [] as MeetingSummary[]; }),
      api.remoteTranscript(meeting.id).catch(() => [] as TranscriptRow[]),
      api.remoteActionItems(meeting.id).catch(() => [] as ActionItem[]),
      api.remoteRecordings(meeting.id).catch(() => [] as Recording[]),
    ]).then(async ([s, t, a, r]) => {
      if (stop) return;
      setSummaries(s);
      setLines(t.map(toLine));
      setTasks(a);
      setRecs(r);
      const u: Record<string, string> = {};
      for (const rec of r) if (rec.status === "ready") u[rec.id] = await api.remoteRecordingUrl(rec.id).catch(() => "");
      if (!stop) setUrls(u);
    });
    return () => { stop = true; };
  }, [meeting.id, version]);

  const live = !meeting.ended_ms;
  const recaps = summaries.filter((s) => s.kind === "recap");
  const recap = recaps.length ? recaps[recaps.length - 1] : undefined;
  const interims = summaries.filter((s) => s.kind !== "recap");
  const grouped = groupTranslations(lines, lang);
  const levelTone = meeting.level === "restricted" ? "red" : meeting.level === "internal" ? "amb" : undefined;
  const dur = (ms: number | null) => (ms == null ? "" : `${Math.floor(ms / 60000)} 分 ${Math.floor((ms % 60000) / 1000)} 秒`);

  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      <Card className="p-5">
        <div className="flex items-center gap-2.5">
          <button onClick={onBack} className="flex items-center gap-1 text-xs font-semibold px-2.5 py-1.5 rounded-lg" style={{ background: T.soft, color: T.sub }}>
            <ArrowLeft size={13} /> 返回
          </button>
          <b className="text-[15px] ml-1">{meeting.title}</b>
          <Tag tone={levelTone as never}>{meeting.level}</Tag>
          <span className="text-[11px]" style={{ color: T.faint }}>
            {fmtTime(meeting.started_ms)} 开始{meeting.ended_ms ? ` · ${fmtTime(meeting.ended_ms)} 结束` : " · 进行中"}
          </span>
          {live && onEnter && (
            <button onClick={() => onEnter(meeting)} className="ml-auto text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.indigo, color: "#fff" }}>
              进入会议
            </button>
          )}
        </div>
        {err && <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>{err}</div>}
      </Card>

      <div style={{ display: "grid", gridTemplateColumns: "1.3fr 1fr", gap: 16 }}>
        <div className="flex flex-col gap-4" style={{ minWidth: 0 }}>
          <Card className="px-5 pt-4 pb-4">
            <div className="flex items-center gap-2">
              <FileText size={13} style={{ color: T.indigo }} />
              <b className="text-[13px]">会议回顾</b>
              {recap && (
                <span className="ml-auto text-[10px]" style={{ color: T.faint }}>
                  {recap.author_id} 整理 · {recap.model ?? "模型未知"} · {fmtTime(recap.created_ms)}
                </span>
              )}
            </div>
            {recap ? (
              <div className="mt-2"><Recap text={recap.text} /></div>
            ) : (
              <div className="py-4 text-[11.5px] leading-relaxed" style={{ color: T.sub }}>
                {live
                  ? "回顾在散会时由会议 Agent 依据转写生成;会议还在进行中。"
                  : "这场会没有回顾:散会时 Agent 不在会中,或者没有任何转写可依据。"}
              </div>
            )}
            {interims.length > 0 && (
              <div className="mt-3 flex flex-col gap-2">
                {interims.map((s) => (
                  <div key={s.id} className="rounded-xl px-3 py-2" style={{ border: `1px solid ${T.line}` }}>
                    <div className="text-[10px] mb-1" style={{ color: T.faint }}>会中小结 · {fmtTime(s.created_ms)}</div>
                    <Recap text={s.text} />
                  </div>
                ))}
              </div>
            )}
          </Card>

          <Card className="px-5 pt-4 pb-4">
            <div className="flex items-center gap-2">
              <Film size={13} style={{ color: T.indigo }} />
              <b className="text-[13px]">录像</b>
              <span className="ml-auto text-[10px]" style={{ color: T.faint }}>服务器本地文件 · 按权限回放</span>
            </div>
            {recs.length === 0 ? (
              <div className="py-4 text-[11.5px]" style={{ color: T.sub }}>这场会没有录制。</div>
            ) : (
              <div className="mt-2 flex flex-col gap-3">
                {recs.map((r) => (
                  <div key={r.id} className="rounded-xl px-3 py-2.5" style={{ border: `1px solid ${T.line}` }}>
                    <div className="flex items-center gap-2 text-[11px]" style={{ color: T.sub }}>
                      <span className="font-semibold" style={{ color: r.status === "ready" ? T.green : r.status === "failed" ? T.red : T.sub }}>
                        {r.status === "ready" ? "可回放" : r.status === "failed" ? "失败" : r.status === "stopping" ? "保存中…" : "录制中"}
                      </span>
                      <span>{r.audio_only ? "音频" : "音视频"}</span>
                      <span>{fmtTime(r.started_ms)} 由 {r.started_by} 开始</span>
                      {r.duration_ms != null && r.duration_ms > 0 && <span>{dur(r.duration_ms)}</span>}
                      {r.size_bytes != null && r.size_bytes > 0 && <span>{fmtBytes(r.size_bytes)}</span>}
                    </div>
                    {r.error && <div className="text-[10.5px] mt-1" style={{ color: T.red }}>{r.error}</div>}
                    {r.status === "ready" && urls[r.id] && (
                      r.audio_only ? (
                        <audio controls preload="metadata" src={urls[r.id]} className="w-full mt-2" />
                      ) : (
                        <video controls preload="metadata" src={urls[r.id]} className="w-full mt-2 rounded-lg" style={{ background: "#000", maxHeight: 360 }} />
                      )
                    )}
                    {r.content_hash && (
                      <div className="text-[9.5px] mt-1 font-mono truncate" style={{ color: T.faint }} title="审计链上记的就是这个哈希:事后可验录像有没有被改过">
                        {r.content_hash}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </Card>

          {tasks.length > 0 && (
            <Card className="px-5 pt-4 pb-4">
              <div className="flex items-center gap-2">
                <b className="text-[13px]">行动项</b>
                <span className="ml-auto text-[10px]" style={{ color: T.faint }}>Agent 提案 · 人裁决</span>
              </div>
              <div className="mt-2 flex flex-col gap-2">
                {tasks.map((t) => (
                  <div key={t.id} className="rounded-xl px-3 py-2" style={{ border: `1px solid ${T.line}`, opacity: t.status === "rejected" ? 0.55 : 1 }}>
                    <div className="text-[12px] leading-relaxed">{t.text}{t.owner_hint && <span style={{ color: T.sub }}> — {t.owner_hint}</span>}</div>
                    <div className="text-[10.5px] mt-1 font-semibold" style={{ color: t.status === "confirmed" ? T.green : t.status === "rejected" ? T.faint : T.red }}>
                      {t.status === "confirmed" ? "已批准" : t.status === "rejected" ? "已驳回" : "待批"}{t.decided_by ? ` · ${t.decided_by}` : ""}
                    </div>
                  </div>
                ))}
              </div>
            </Card>
          )}
        </div>

        <Card className="px-5 pt-4 pb-2 flex flex-col" style={{ maxHeight: 720, minWidth: 0 }}>
          <div className="flex items-center gap-2">
            <Radio size={13} style={{ color: T.indigo }} />
            <b className="text-[13px]">纪要</b>
            <span className="ml-auto text-[10px]" style={{ color: T.faint }}>{grouped.length} 行{lang !== "source" ? ` · 附 ${CAPTION_LANGS.find((l) => l.code === lang)?.name ?? lang} 译文` : ""}</span>
          </div>
          <div className="mt-2 overflow-y-auto flex-1">
            {grouped.length === 0 ? (
              <div className="py-6 text-[11.5px]" style={{ color: T.sub }}>没有转写。</div>
            ) : (
              grouped.map(({ line: l, translation }, i) => (
                <div key={`${l.id}-${i}`} className="py-1.5" style={{ borderTop: i ? `1px solid ${T.line}` : undefined }}>
                  <div className="flex items-baseline gap-2">
                    <b className="text-[11.5px]" style={{ color: l.speaker === "系统" ? T.red : T.indigoDeep }}>{l.speaker}</b>
                    <span className="text-[9.5px]" style={{ color: T.faint }}>{fmtTime(l.ts)}</span>
                  </div>
                  <div className="text-[12px] mt-0.5 leading-relaxed">{l.text}</div>
                  {translation && <div className="text-[11.5px] mt-0.5 leading-relaxed" style={{ color: T.sub }}>{translation}</div>}
                </div>
              ))
            )}
          </div>
        </Card>
      </div>
    </div>
  );
}
