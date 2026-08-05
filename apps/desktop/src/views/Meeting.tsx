/* C3:真实会议室(LiveKit)。
 *
 * 为什么这一端用 JS SDK,而 Agent 那端用 Rust SDK——不是不一致,是约束相反:
 * Agent 是无头进程、要在同进程里跑 muster_route(架构文档边界五),所以必须 Rust;
 * 桌面壳的 webview 本来就是浏览器,自带 WebRTC、getUserMedia 和 <video>,
 * 用 Rust SDK 反而要把音视频帧再喂回 webview。
 *
 * **能不能开麦不是前端判的**:它来自服务端入会票里的 can_publish,
 * 而那是 muster_identity::can() 的判定结果。前端只照着显示。
 */
import { useEffect, useRef, useState } from "react";
import {
  ConnectionState,
  Room,
  RoomEvent,
  Track,
  type LocalTrack,
  type RemoteTrack,
} from "livekit-client";
import { Bot, Mic, MicOff, PhoneOff, Radio, Users, Video, VideoOff } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { api, RemoteMeeting, fmtTime } from "../api";

export interface TranscriptLine {
  speaker: string;
  text: string;
  ts: number;
}

export function MeetingRoom({
  meeting,
  transcript,
  onLeave,
}: {
  meeting: RemoteMeeting;
  /** 由 SSE 推来的转写行(Agent 落库后广播) */
  transcript: TranscriptLine[];
  onLeave: () => void;
}) {
  const [room] = useState(() => new Room({ adaptiveStream: true, dynacast: true }));
  const [state, setState] = useState<ConnectionState>(ConnectionState.Disconnected);
  const [err, setErr] = useState<string | null>(null);
  const [canPublish, setCanPublish] = useState(false);
  const [micOn, setMicOn] = useState(false);
  const [camOn, setCamOn] = useState(false);
  const [roster, setRoster] = useState<Seat[]>([]);
  const [wantsAgent, setWantsAgent] = useState(meeting.wants_agent);
  const [agentBusy, setAgentBusy] = useState(false);
  /** 谁的画面。视频轨由 LiveKit 给的是 DOM 元素,只能命令式挂;
   *  席位本身仍归 React 管——两者放在不同节点上,互不打架。 */
  const videoTracks = useRef(new Map<string, RemoteTrack | LocalTrack>());
  const hosts = useRef(new Map<string, HTMLDivElement>());
  const audioSink = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;

    const refresh = () =>
      setRoster(
        [room.localParticipant, ...room.remoteParticipants.values()].map((p) => ({
          id: p.identity,
          name: p.name || p.identity,
          isLocal: p.isLocal,
          micOn: p.isMicrophoneEnabled,
          speaking: p.isSpeaking,
          hasVideo: videoTracks.current.has(p.identity),
        }))
      );

    const place = (id: string) => {
      const [host, track] = [hosts.current.get(id), videoTracks.current.get(id)];
      if (host && track && !host.querySelector("video")) host.appendChild(track.attach());
    };

    const attach = (track: RemoteTrack | LocalTrack, who: string) => {
      // 同一条轨只挂一次:重连时 TrackSubscribed 会再来一遍,
      // 挂两次就是两份声音同时播——听起来像回声,却会被误当成声学回授
      if (track.attachedElements?.length) return;
      if (track.kind === Track.Kind.Video) {
        videoTracks.current.set(who, track);
        place(who);
      } else if (audioSink.current) {
        // 音频元素不进布局:它只是让声音出来,不属于任何一个可见席位
        const el = track.attach();
        el.style.display = "none";
        audioSink.current.appendChild(el);
      }
      refresh();
    };

    const drop = (track: RemoteTrack | LocalTrack, who?: string) => {
      track.detach().forEach((e) => e.remove());
      if (who) videoTracks.current.delete(who);
      refresh();
    };

    room
      .on(RoomEvent.ConnectionStateChanged, (s) => setState(s))
      .on(RoomEvent.TrackSubscribed, (track, _pub, p) => attach(track, p.identity))
      .on(RoomEvent.TrackUnsubscribed, (track, _pub, p) => drop(track, p.identity))
      .on(RoomEvent.LocalTrackPublished, (pub) => {
        // 自己的画面 LiveKit 不会"订阅"给自己
        if (pub.track) attach(pub.track, room.localParticipant.identity);
      })
      .on(RoomEvent.LocalTrackUnpublished, (pub) => {
        if (pub.track) drop(pub.track, room.localParticipant.identity);
      })
      // 说话人高亮的唯一数据来源。概念稿里那个绿框是写死的,这个是真的
      .on(RoomEvent.ActiveSpeakersChanged, refresh)
      .on(RoomEvent.TrackMuted, refresh)
      .on(RoomEvent.TrackUnmuted, refresh)
      .on(RoomEvent.ParticipantConnected, refresh)
      .on(RoomEvent.ParticipantDisconnected, refresh)
      .on(RoomEvent.Disconnected, () => setRoster([]));

    api
      .remoteMeetingJoin(meeting.id)
      .then(async (info) => {
        if (cancelled) return;
        setCanPublish(info.can_publish);
        await room.connect(info.url, info.token);
        if (cancelled) return;
        refresh();
      })
      .catch((e) => setErr(String(e)));

    return () => {
      cancelled = true;
      room.disconnect();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [meeting.id]);

  const toggleMic = async () => {
    try {
      await room.localParticipant.setMicrophoneEnabled(!micOn);
      setMicOn(!micOn);
    } catch (e) {
      // 麦克风被系统拒绝是最常见的失败,要说清楚而不是静默
      setErr(`打不开麦克风:${e}。macOS 需要在「系统设置 → 隐私与安全性 → 麦克风」里放行。`);
    }
  };
  const toggleCam = async () => {
    try {
      await room.localParticipant.setCameraEnabled(!camOn);
      setCamOn(!camOn);
    } catch (e) {
      setErr(`打不开摄像头:${e}`);
    }
  };

  /** 把某个席位的画面容器登记下来,并在轨已就绪时立刻挂上。
   *
   *  ref 回调每次渲染都是新函数,React 会先用 null 调一次、再用元素调一次——
   *  所以必须靠"已经有 <video> 就不再挂"来防重复,不能假设只会调用一次。
   *  `track.attach()` 每调一次就新建一个元素,挂两次就是两份画面。 */
  const mountHost = (id: string, el: HTMLDivElement | null) => {
    if (!el) return;
    hosts.current.set(id, el);
    const t = videoTracks.current.get(id);
    if (t && !el.querySelector("video")) el.appendChild(t.attach());
  };

  const connected = state === ConnectionState.Connected;
  // Agent 是不是一个在场的参会者。名字来自它的账号 id / 显示名,
  // 两者都认——部署方可能改过显示名。
  const agentHere = roster.some((p) => p.name === "A-007" || p.name === "小七");
  const levelTone = meeting.level === "restricted" ? "red" : meeting.level === "internal" ? "amb" : undefined;

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
            <span className="ml-auto flex items-center gap-1.5 text-[11px]" style={{ color: T.sub }}>
              <Users size={13} /> {roster.length} 人在场
            </span>
          </div>

          <div className="text-[11px] mt-1.5" style={{ color: T.faint }}>
            {connected ? `已入房间 ${meeting.room}` : state === ConnectionState.Connecting ? "连接中…" : "未连接"}
          </div>

          {err && (
            <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>
              {err}
            </div>
          )}

          {/* 席位。发言时描边变绿,数据来自 LiveKit 的 ActiveSpeakersChanged。
              放画面的那个 div **没有 React 子节点**——LiveKit 往里 appendChild,
              React 管别处,两边不会互相拆台。 */}
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
                  <div ref={(el) => mountHost(p.id, el)} className="absolute inset-0" />
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
          {connected && !roster.some((p) => p.hasVideo) && (
            <div className="text-[10.5px] mt-2" style={{ color: T.faint }}>
              没有人开摄像头。语音是通的——谁在说话,席位的描边会变绿。
            </div>
          )}

          <div className="flex items-center gap-2 mt-4">
            <button
              onClick={toggleMic}
              disabled={!connected || !canPublish}
              title={canPublish ? "" : "你在这个频道没有发言权限(由服务端权限内核判定)"}
              className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{
                background: micOn ? T.indigo : T.soft,
                color: micOn ? "#fff" : T.sub,
                opacity: connected && canPublish ? 1 : 0.45,
              }}
            >
              {micOn ? <Mic size={13} /> : <MicOff size={13} />} {micOn ? "麦克风开" : "开麦"}
            </button>
            <button
              onClick={toggleCam}
              disabled={!connected || !canPublish}
              className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{
                background: camOn ? T.indigo : T.soft,
                color: camOn ? "#fff" : T.sub,
                opacity: connected && canPublish ? 1 : 0.45,
              }}
            >
              {camOn ? <Video size={13} /> : <VideoOff size={13} />} {camOn ? "摄像头开" : "开摄像头"}
            </button>
            <button
              onClick={() => {
                room.disconnect();
                onLeave();
              }}
              className="ml-auto flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
              style={{ background: T.redSoft, color: T.red }}
            >
              <PhoneOff size={13} /> 离开
            </button>
          </div>

          {!canPublish && connected && (
            <div className="text-[10.5px] mt-2" style={{ color: T.sub }}>
              你只能旁听:发言权限由服务端的权限内核判定,不是这个界面决定的。
            </div>
          )}
        </Card>

        {/* 音频元素要在 DOM 里才播,但它不属于任何一个可见席位 */}
        <div ref={audioSink} hidden />
      </div>

      {/* 实时纪要:由会议 Agent 转写后落库,再经 SSE 推来 */}
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
            本地转写
          </span>
        </div>
        {!agentHere && (
          <div className="mt-2 px-3 py-2 rounded-xl text-[11px] leading-relaxed"
            style={{ background: wantsAgent ? T.soft : T.redSoft, color: wantsAgent ? T.sub : T.red }}>
            {wantsAgent ? (
              <>已请 Agent,等它进来(常驻服务每几秒认领一次)。若一直不来,检查服务器上的 agent-daemon 是否在跑。</>
            ) : (
              <>会议 Agent 不在这场会里,<b>说话不会被转写</b>。</>
            )}
          </div>
        )}
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
            transcript.map((l, i) => (
              <div key={i} className="py-1.5" style={{ borderTop: i ? `1px solid ${T.line}` : undefined }}>
                <div className="flex items-baseline gap-2">
                  <b className="text-[11.5px]" style={{ color: l.speaker === "系统" ? T.red : T.indigoDeep }}>
                    {l.speaker}
                  </b>
                  <span className="text-[9.5px]" style={{ color: T.faint }}>{fmtTime(l.ts)}</span>
                </div>
                <div className="text-[12px] mt-0.5 leading-relaxed">{l.text}</div>
              </div>
            ))
          )}
        </div>
      </Card>
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
