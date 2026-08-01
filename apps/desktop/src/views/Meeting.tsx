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
  RemoteTrack,
  Room,
  RoomEvent,
  Track,
  type RemoteParticipant,
} from "livekit-client";
import { Mic, MicOff, PhoneOff, Radio, Users, Video, VideoOff } from "lucide-react";
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
  const [peers, setPeers] = useState<string[]>([]);
  const mediaRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;

    const attach = (track: RemoteTrack, who: string) => {
      if (!mediaRef.current) return;
      const el = track.attach();
      el.dataset.who = who;
      if (track.kind === Track.Kind.Video) {
        (el as HTMLVideoElement).className = "rounded-xl w-full";
        mediaRef.current.appendChild(el);
      } else {
        // 音频元素不进布局:它只是让声音出来
        el.style.display = "none";
        mediaRef.current.appendChild(el);
      }
    };

    room
      .on(RoomEvent.ConnectionStateChanged, (s) => setState(s))
      .on(RoomEvent.TrackSubscribed, (track, _pub, p) => attach(track, p.identity))
      .on(RoomEvent.TrackUnsubscribed, (track) => track.detach().forEach((e) => e.remove()))
      .on(RoomEvent.ParticipantConnected, () => setPeers(names(room)))
      .on(RoomEvent.ParticipantDisconnected, () => setPeers(names(room)))
      .on(RoomEvent.Disconnected, () => setPeers([]));

    api
      .remoteMeetingJoin(meeting.id)
      .then(async (info) => {
        if (cancelled) return;
        setCanPublish(info.can_publish);
        await room.connect(info.url, info.token);
        if (cancelled) return;
        setPeers(names(room));
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

  const connected = state === ConnectionState.Connected;
  // Agent 是不是一个在场的参会者。名字来自它的账号 id / 显示名,
  // 两者都认——部署方可能改过显示名。
  const agentHere = peers.some((p) => p === "A-007" || p === "小七");
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
              <Users size={13} /> {peers.length + 1} 人在场
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

          {/* 视频区:没人开摄像头时不留一片空白,如实说明 */}
          <div ref={mediaRef} className="mt-3.5 grid grid-cols-2 gap-3 min-h-[80px]" />
          {connected && (
            <div className="text-[10.5px] mt-2" style={{ color: T.faint }}>
              视频画面在上方;没人开摄像头时这里是空的——语音仍然是通的。
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

        {peers.length > 0 && (
          <Card className="px-5 pt-4 pb-3">
            <b className="text-[13px]">在场</b>
            <div className="flex flex-wrap gap-1.5 mt-2">
              {peers.map((p) => (
                <span key={p} className="text-[11px] px-2.5 py-1 rounded-lg" style={{ background: T.soft, color: T.sub }}>
                  {p}
                </span>
              ))}
            </div>
          </Card>
        )}
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
            style={{ background: T.redSoft, color: T.red }}>
            会议 Agent 不在这场会里,<b>说话不会被转写</b>。
            需要在服务器上把它拉起来并指向本会议。
          </div>
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

function names(room: Room): string[] {
  return Array.from(room.remoteParticipants.values()).map(
    (p: RemoteParticipant) => p.name || p.identity
  );
}
