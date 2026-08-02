/* 网页参会端。
 *
 * 为什么有它:**参会者不需要桌面壳。** 桌面壳里 Tauri 特有的东西是本地 Runner、
 * 本地审计链、本地正文库——那些是"在自己机器上跑任务"才要的。开会只需要浏览器:
 * LiveKit 走 WebRTC、纪要走 SSE、消息走 HTTP。
 *
 * 于是第二台机器(Windows / Linux / 谁都行)开个浏览器就能进会,什么都不用装。
 *
 * 打包成单个自包含 JS 由服务端托管:**内网连不上 CDN**,依赖必须随包走。
 *
 * ## 它做什么、不做什么
 *
 * 做:登录、看会议、进会、开麦、看实时纪要、发消息。
 * 不做:跑任务、审批、能力库——那些要本地 Runner 与本地审计链,
 * 浏览器里做不了,**也不该假装能做**。
 */
import { Room, RoomEvent, Track, ConnectionState } from "livekit-client";

const $ = (id) => document.getElementById(id);
const state = { base: location.origin, token: null, me: null, room: null, meeting: null, es: null };

function show(view) {
  for (const v of ["login", "lobby", "meeting"]) $(v).hidden = v !== view;
}
function toast(msg, bad = false) {
  const t = $("toast");
  t.textContent = msg;
  t.className = bad ? "toast bad" : "toast";
  t.hidden = false;
  setTimeout(() => (t.hidden = true), 5000);
}

async function api(path, opts = {}) {
  const r = await fetch(state.base + path, {
    ...opts,
    headers: {
      "content-type": "application/json",
      ...(state.token ? { authorization: "Bearer " + state.token } : {}),
      ...(opts.headers || {}),
    },
  });
  const text = await r.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    /* 非 JSON 就当空 */
  }
  if (!r.ok) throw new Error(body?.error || `HTTP ${r.status}`);
  return body;
}

// ---------------------------------------------------------------- 登录

$("loginForm").onsubmit = async (e) => {
  e.preventDefault();
  state.base = $("server").value.trim().replace(/\/$/, "") || location.origin;
  try {
    const r = await api("/auth/login", {
      method: "POST",
      body: JSON.stringify({ id: $("account").value.trim(), password: $("password").value }),
    });
    state.token = r.token;
    state.me = r.display_name;
    localStorage.setItem("muster.session", JSON.stringify({ base: state.base, token: r.token, me: r.display_name }));
    await enterLobby();
  } catch (err) {
    toast(String(err.message || err), true);
  }
};

async function enterLobby() {
  $("who").textContent = state.me;
  show("lobby");
  await refreshChannels();
}

async function refreshChannels() {
  const chans = await api("/channels");
  const sel = $("channel");
  sel.innerHTML = "";
  for (const c of chans) {
    const o = document.createElement("option");
    o.value = c.id;
    o.textContent = `#${c.name} · ${c.level}`;
    sel.appendChild(o);
  }
  if (chans.length) await refreshMeetings();
}

async function refreshMeetings() {
  const cid = $("channel").value;
  if (!cid) return;
  const list = await api(`/channels/${cid}/meetings`);
  const box = $("meetings");
  box.innerHTML = "";
  const live = list.filter((m) => !m.ended_ms);
  if (!live.length) {
    box.innerHTML = '<div class="dim">当前没有进行中的会议。</div>';
    return;
  }
  for (const m of live) {
    const b = document.createElement("button");
    b.className = "row";
    b.innerHTML = `<b>${m.title}</b><span class="tag ${m.level}">${m.level}</span>` +
      (m.wants_agent ? '<span class="dim">已请 Agent</span>' : "");
    b.onclick = () => joinMeeting(m);
    box.appendChild(b);
  }
}
$("channel").onchange = refreshMeetings;
$("refresh").onclick = () => refreshMeetings().catch((e) => toast(String(e), true));

$("startMeeting").onclick = async () => {
  const title = $("title").value.trim();
  if (!title) return toast("先给会议起个名字", true);
  try {
    const m = await api(`/channels/${$("channel").value}/meetings`, {
      method: "POST",
      body: JSON.stringify({ title }),
    });
    $("title").value = "";
    await joinMeeting(m);
  } catch (e) {
    toast(String(e.message || e), true);
  }
};

// ---------------------------------------------------------------- 会议

async function joinMeeting(m) {
  // **先把上一个房间断干净。** 不断的话旧 Room 的音频元素还挂在页面上继续播,
  // 两份声音叠在一起——听起来就是回声,而人会以为是声学回授去找耳机。
  if (state.room) {
    try {
      await state.room.disconnect();
    } catch {
      /* 已经断了就算了 */
    }
    state.room = null;
  }
  $("videos").innerHTML = "";
  state.meeting = m;
  $("mTitle").textContent = m.title;
  $("mLevel").textContent = m.level;
  $("mLevel").className = "tag " + m.level;
  $("lines").innerHTML = "";
  $("agentBtn").textContent = m.wants_agent ? "请 Agent 离开" : "请 Agent 来记录";
  show("meeting");

  let info;
  try {
    info = await api(`/meetings/${m.id}/join`, { method: "POST", body: "{}" });
  } catch (e) {
    toast("入会被拒:" + (e.message || e), true);
    return show("lobby");
  }
  // 能不能开麦由服务端 can() 决定,前端只照着显示
  $("micBtn").disabled = !info.can_publish;
  $("micBtn").title = info.can_publish ? "" : "你在这个频道没有发言权限(服务端权限内核判定)";

  const room = new Room({ adaptiveStream: true });
  state.room = room;
  room
    .on(RoomEvent.ConnectionStateChanged, (s) => {
      $("mState").textContent = s === ConnectionState.Connected ? "已入房间" : String(s);
    })
    .on(RoomEvent.TrackSubscribed, (track) => {
      // 同一条轨只挂一次:重连时 TrackSubscribed 会再来一遍,
      // 挂两次就是两份声音同时播
      if (track.attachedElements?.length) return;
      const el = track.attach();
      if (track.kind === Track.Kind.Video) {
        el.className = "video";
        $("videos").appendChild(el);
      } else {
        el.style.display = "none";
        $("videos").appendChild(el);
      }
    })
    .on(RoomEvent.TrackUnsubscribed, (t) => t.detach().forEach((e) => e.remove()))
    .on(RoomEvent.ParticipantConnected, renderPeers)
    .on(RoomEvent.ParticipantDisconnected, renderPeers);

  try {
    await room.connect(info.url, info.token);
    renderPeers();
  } catch (e) {
    // **信令失败和媒体失败是两回事,报错要分开说。**
    // 之前统一归咎于 node_ip,结果信令层的问题被指到媒体配置上,查错了方向。
    const msg = String(e);
    const hint = /signal|Failed to fetch|WebSocket/i.test(msg)
      ? `连不上 LiveKit 的信令(WebSocket)。多半是服务端发下来的地址 ${info.url} ` +
        `这台机器到不了 —— 如果它是 localhost,那在你这台机器上指的是你自己。` +
        `让管理员检查服务端的 LIVEKIT_URL。`
      : `信令通了但媒体连不上。多半是 LiveKit 广播的 ICE 候选这台机器到不了 —— ` +
        `让管理员检查服务端的 rtc.node_ip 和 UDP 7882 是否可达。`;
    toast(`进不去房间:${msg}\n${hint}`, true);
  }

  await loadTranscript(m.id);
  openStream();
}

function renderPeers() {
  const room = state.room;
  if (!room) return;
  const names = [...room.remoteParticipants.values()].map((p) => p.name || p.identity);
  $("peers").textContent = `${names.length + 1} 人在场` + (names.length ? `:${names.join("、")}` : "");
  // Agent 在不在必须看得见——它不在时说话不会被转写
  const here = names.some((n) => n === "A-007" || n === "小七");
  $("agentState").textContent = here ? "Agent 在会中" : "Agent 不在会中";
  $("agentState").className = "badge " + (here ? "on" : "off");
}

async function loadTranscript(mid) {
  try {
    const rows = await api(`/meetings/${mid}/transcript`);
    for (const r of rows) addLine(r.speaker_id, r.text, r.ts_ms);
  } catch {
    /* 拉不到就从现在开始记 */
  }
}

function addLine(speaker, text, ts) {
  const d = document.createElement("div");
  d.className = "line";
  const t = new Date(ts).toTimeString().slice(0, 8);
  d.innerHTML = `<b class="${speaker === "系统" ? "sys" : ""}">${speaker}</b> <span class="dim">${t}</span><div>${escapeHtml(text)}</div>`;
  $("lines").appendChild(d);
  $("lines").scrollTop = $("lines").scrollHeight;
}
const escapeHtml = (s) =>
  s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

/** SSE:断线重连与 Last-Event-ID 是浏览器白送的,不自己写补拉。 */
function openStream() {
  state.es?.close();
  const es = new EventSource(`${state.base}/events?token=${encodeURIComponent(state.token)}`);
  es.onmessage = (e) => {
    try {
      const ev = JSON.parse(e.data);
      if (ev.type === "transcript" && ev.meeting_id === state.meeting?.id) {
        addLine(ev.speaker_id, ev.text, ev.ts_ms);
      }
    } catch {
      /* 单条坏了不该拖垮整条流 */
    }
  };
  state.es = es;
}

$("micBtn").onclick = async () => {
  const on = state.room?.localParticipant.isMicrophoneEnabled;
  try {
    await state.room.localParticipant.setMicrophoneEnabled(!on);
    $("micBtn").textContent = !on ? "麦克风开" : "开麦";
    $("micBtn").classList.toggle("active", !on);
  } catch (e) {
    toast(`打不开麦克风:${e}。浏览器需要在地址栏放行麦克风权限。`, true);
  }
};

$("agentBtn").onclick = async () => {
  const want = $("agentBtn").textContent.includes("来记录");
  try {
    await api(`/meetings/${state.meeting.id}/agent`, {
      method: "POST",
      body: JSON.stringify({ want }),
    });
    $("agentBtn").textContent = want ? "请 Agent 离开" : "请 Agent 来记录";
    toast(want ? "已请 Agent,等它进来(常驻服务每几秒认领一次)" : "已请 Agent 离开");
  } catch (e) {
    toast(String(e.message || e), true);
  }
};

$("leaveBtn").onclick = () => {
  state.room?.disconnect();
  state.es?.close();
  state.room = null;
  $("videos").innerHTML = "";
  show("lobby");
  refreshMeetings().catch(() => {});
};

// ---------------------------------------------------------------- 启动

(function boot() {
  $("server").value = location.origin;
  const raw = localStorage.getItem("muster.session");
  if (!raw) return show("login");
  try {
    const s = JSON.parse(raw);
    Object.assign(state, s, { me: s.me });
    // 探一次再说"已登录":令牌 12 小时过期,显示登录了却什么都拉不到更糟
    api("/channels")
      .then(() => enterLobby())
      .catch(() => {
        localStorage.removeItem("muster.session");
        state.token = null;
        show("login");
      });
  } catch {
    show("login");
  }
})();
