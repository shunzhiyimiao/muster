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
// 字幕:开关与语言记在本机;语言告诉服务端,Agent 据此决定翻成哪些语言
const caps = {
  on: localStorage.getItem("muster.captions") !== "0",
  lang: localStorage.getItem("muster.captionLang") || "source",
  recent: [], // {id, speaker, text, at}
};

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
    loginNotice("");
    await enterLobby();
  } catch (err) {
    const msg = String(err.message || err);
    if (msg.includes("批准")) loginNotice(msg, true);
    else toast(msg, true);
  }
};

/* ---------------------------------------------------------------- 自助注册(2026-11-03)
 *
 * 入口只在服务端说开放(GET /auth/registration 不是 closed)时显示。approval 模式下注册完
 * 不发令牌,要等管理员批准;open 模式直接给令牌,当场进大厅。
 */
const serverBase = () => $("server").value.trim().replace(/\/$/, "") || location.origin;

let regMode = "closed";
async function syncRegisterEntry() {
  try {
    const r = await fetch(serverBase() + "/auth/registration");
    const j = r.ok ? await r.json() : null;
    regMode = j?.mode ?? "closed";
  } catch {
    regMode = "closed";
  }
  $("toRegister").hidden = regMode === "closed";
  // 只有「登录」一个页签时整条页签都不显示
  $("toLogin").parentElement.classList.toggle("single", regMode === "closed");
  $("regModeHint").textContent =
    regMode === "open" ? "注册即可登录。" : "注册后需要管理员批准才能登录。";
  if (regMode === "closed") authTab("login");
}
$("server").addEventListener("change", syncRegisterEntry);

/** 登录 / 注册页签。 */
function authTab(which) {
  const reg = which === "register";
  authForms(reg ? "register" : "login");
  $("toRegister").classList.toggle("on", reg);
  $("toLogin").classList.toggle("on", !reg);
  (reg ? $("regId") : $("account")).focus();
}
function loginNotice(text, warn = false) {
  const n = $("loginNotice");
  n.textContent = text || "";
  n.className = "notice " + (warn ? "warn" : "ok");
  n.hidden = !text;
}
$("toRegister").onclick = () => authTab("register");
$("toLogin").onclick = () => authTab("login");
$("registerForm").onsubmit = async (e) => {
  e.preventDefault();
  if ($("regPw").value !== $("regPw2").value) return toast("两次口令不一致", true);
  state.base = serverBase();
  try {
    const r = await api("/auth/register", {
      method: "POST",
      body: JSON.stringify({
        id: $("regId").value.trim(),
        password: $("regPw").value,
        display_name: $("regName").value.trim(),
        email: $("regEmail").value.trim() || null,
        department: $("regDept").value.trim() || null,
        title: $("regTitle").value.trim() || null,
      }),
    });
    if (r.pending) {
      $("account").value = r.id;
      $("password").value = "";
      authTab("login");
      loginNotice(`已提交注册 ${r.id}。管理员批准后,用这个账号和口令登录。`);
      return;
    }
    state.token = r.token;
    state.me = r.display_name;
    localStorage.setItem("muster.session", JSON.stringify({ base: state.base, token: r.token, me: r.display_name }));
    loginNotice("");
    await enterLobby();
  } catch (err) {
    toast(String(err.message || err), true);
  }
};

/* ---------------------------------------------------------------- 找回口令(2026-11-03,服务端 profile.rs)
 *
 * 凭邮箱申请永远回 200(不泄露邮箱存不存在);邮件通道未接时服务端回 delivery:"none",
 * 如实告诉人去找管理员。带 ?reset=令牌 打开页面就显示重置表单。
 */
function authForms(which) {
  for (const [id, on] of [["loginForm", which === "login"], ["registerForm", which === "register"], ["recoverForm", which === "recover"], ["resetForm", which === "reset"]]) {
    $(id).hidden = !on;
  }
  $("toLogin").parentElement.hidden = which === "recover" || which === "reset";
}
$("toRecover").onclick = () => {
  authForms("recover");
  $("recEmail").focus();
};
$("recBack").onclick = () => authForms("login");
$("recoverForm").onsubmit = async (e) => {
  e.preventDefault();
  state.base = serverBase();
  try {
    const r = await api("/auth/recover", { method: "POST", body: JSON.stringify({ email: $("recEmail").value.trim() }) });
    authForms("login");
    loginNotice(
      r.delivery === "none"
        ? "已记录申请,但这个服务器还没接邮件通道:请找组织管理员在「编制」页为你签发重置链接。"
        : "如果邮箱存在,重置链接已发到邮箱,30 分钟内有效。",
    );
  } catch (err) {
    toast(String(err.message || err), true);
  }
};
$("resetForm").onsubmit = async (e) => {
  e.preventDefault();
  if ($("rstPw").value !== $("rstPw2").value) return toast("两次口令不一致", true);
  const token = new URLSearchParams(location.search).get("reset") || "";
  state.base = serverBase();
  try {
    const r = await api("/auth/reset", { method: "POST", body: JSON.stringify({ token, new_password: $("rstPw").value }) });
    history.replaceState(null, "", location.pathname);
    authForms("login");
    $("account").value = r.id;
    loginNotice(`口令已重置,用 ${r.id} 和新口令登录。`);
  } catch (err) {
    toast(String(err.message || err), true);
  }
};

/* ---------------------------------------------------------------- 个人资料 */
let providerOptions = [];
async function loadProfile() {
  try {
    const p = await api("/me/profile");
    $("pfName").value = p.display_name || "";
    $("pfEmail").value = p.email || "";
    $("pfDept").value = p.department || "";
    $("pfTitle").value = p.title || "";
    $("pfBio").value = p.bio || "";
    const last = p.last_login_ms ? `最近登录 ${new Date(p.last_login_ms).toLocaleString()}${p.last_login_ip ? " · " + p.last_login_ip : ""}` : "";
    $("profileMeta").textContent = [p.id, last].filter(Boolean).join(" · ");
    $("avatarInitial").textContent = [...(p.display_name || p.id)][0] || "?";
    if (p.avatar_url) {
      // 头像接口要令牌,<img src> 带不了 Authorization,拉成 blob 再显示
      const r = await fetch(state.base + p.avatar_url, { headers: { authorization: "Bearer " + state.token } });
      if (r.ok) {
        $("avatarImg").src = URL.createObjectURL(await r.blob());
        $("avatarImg").style.display = "";
        $("avatarInitial").style.display = "none";
      }
    } else {
      $("avatarImg").style.display = "none";
      $("avatarInitial").style.display = "flex";
    }
    try {
      const cat = await api("/providers/catalog");
      providerOptions = Object.entries(cat.providers || {}).map(([id, c]) => ({ id, name: c.display_name || id, locality: c.locality }));
    } catch {
      providerOptions = [];
    }
    const sel = $("pfProvider");
    sel.innerHTML = '<option value="">跟组织默认</option>' + providerOptions.map((o) => `<option value="${escapeHtml(o.id)}">${escapeHtml(o.name)}${o.locality === "cloud" ? "(云端)" : "(本地)"}</option>`).join("");
    sel.value = p.preferred_provider || "";
  } catch (err) {
    toast(String(err.message || err), true);
  }
}
$("profileBtn").onclick = async () => {
  $("profileCard").hidden = !$("profileCard").hidden;
  if (!$("profileCard").hidden) await loadProfile();
};
$("profileForm").onsubmit = async (e) => {
  e.preventDefault();
  try {
    const p = await api("/me/profile", {
      method: "PUT",
      body: JSON.stringify({
        display_name: $("pfName").value,
        email: $("pfEmail").value,
        department: $("pfDept").value,
        title: $("pfTitle").value,
        bio: $("pfBio").value,
        preferred_provider: $("pfProvider").value,
      }),
    });
    state.me = p.display_name;
    $("who").textContent = p.display_name;
    $("profileSaved").textContent = "已保存";
    setTimeout(() => ($("profileSaved").textContent = ""), 3000);
  } catch (err) {
    toast(String(err.message || err), true);
  }
};
$("avatarFile").onchange = async () => {
  const f = $("avatarFile").files[0];
  if (!f) return;
  if (f.size > 512 * 1024) return toast("头像最大 512 KiB", true);
  try {
    const r = await fetch(state.base + "/me/avatar", { method: "POST", headers: { authorization: "Bearer " + state.token, "content-type": f.type || "application/octet-stream" }, body: f });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
    toast("头像已更新");
    await loadProfile();
  } catch (err) {
    toast(String(err.message || err), true);
  } finally {
    $("avatarFile").value = "";
  }
};

/* 改口令:要原口令。拿着没过期的令牌不等于知道口令,不能凭令牌换锁(服务端 signup.rs)。 */
$("pwBtn").onclick = () => {
  $("pwForm").hidden = !$("pwForm").hidden;
  if (!$("pwForm").hidden) $("pwOld").focus();
};
$("pwCancel").onclick = () => {
  $("pwForm").hidden = true;
};
$("pwForm").onsubmit = async (e) => {
  e.preventDefault();
  try {
    await api("/me/password", {
      method: "POST",
      body: JSON.stringify({ old_password: $("pwOld").value, new_password: $("pwNew").value }),
    });
    toast("口令已改");
    $("pwOld").value = "";
    $("pwNew").value = "";
    $("pwForm").hidden = true;
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
      (m.wants_agent ? '<span class="dim">已请 Agent</span>' : "") +
      (m.recording_active ? '<span class="badge rec">● 录制中</span>' : "");
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
  clearStage();
  state.meeting = m;
  $("mTitle").textContent = m.title;
  $("mLevel").textContent = m.level;
  $("mLevel").className = "tag " + m.level;
  $("lines").innerHTML = "";
  $("tasks").innerHTML = "";
  $("tasksCard").hidden = true;
  $("agentBtn").textContent = m.wants_agent ? "请 Agent 离开" : "请 Agent 来记录";
  $("recBadge").hidden = !m.recording_active;
  caps.recent = [];
  renderCaptions();
  $("capLang").value = caps.lang;
  $("capBtn").classList.toggle("active", caps.on);
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
  $("screenBtn").disabled = !info.can_publish || !SCREEN_SHARE_SUPPORTED;
  if (!SCREEN_SHARE_SUPPORTED) $("screenBtn").title = "这个浏览器没有 getDisplayMedia,不能共享屏幕";
  syncScreenBtn();

  const room = new Room({ adaptiveStream: true });
  state.room = room;
  room
    .on(RoomEvent.ConnectionStateChanged, (s) => {
      $("mState").textContent = s === ConnectionState.Connected ? "已入房间" : String(s);
    })
    .on(RoomEvent.TrackSubscribed, (track, pub, who) => {
      // 同一条轨只挂一次:重连时 TrackSubscribed 会再来一遍,
      // 挂两次就是两份声音同时播
      if (track.attachedElements?.length) return;
      if (track.kind === Track.Kind.Video) {
        seatFor(who);
        addVideo(track, who.identity, pub.source);
      } else {
        // 音频要在 DOM 里才播,但它不属于任何一个可见格子(共享屏幕的声音也走这里)
        const el = track.attach();
        el.style.display = "none";
        $("audioSink").appendChild(el);
      }
      syncStage();
    })
    .on(RoomEvent.TrackUnsubscribed, (t, pub, who) => {
      if (t.kind === Track.Kind.Video) removeVideo(keyOf(who.identity, pub.source));
      else t.detach().forEach((e) => e.remove());
      syncStage();
    })
    // 对方停止共享 / 关摄像头时 Unpublished 往往比 Unsubscribed 先到(甚至只到这一个):
    // 按它摘画面,格子立刻回到首字母,不留黑屏
    .on(RoomEvent.TrackUnpublished, (pub, who) => {
      if (pub.kind === Track.Kind.Video) removeVideo(keyOf(who.identity, pub.source));
      syncStage();
    })
    // 自己的画面 LiveKit 不会"订阅"给自己,得单独接
    .on(RoomEvent.LocalTrackPublished, (pub) => {
      if (pub.track?.kind === Track.Kind.Video) addVideo(pub.track, room.localParticipant.identity, pub.source);
      syncScreenBtn();
      syncStage();
    })
    .on(RoomEvent.LocalTrackUnpublished, (pub) => {
      if (pub.track?.kind === Track.Kind.Video) removeVideo(keyOf(room.localParticipant.identity, pub.source));
      else pub.track?.detach().forEach((e) => e.remove());
      syncScreenBtn();
      syncStage();
    })
    // 浏览器的自动播放限制:入会后几秒远端音轨才到,这时 play() 可能被拦,表现是
    // "点了开麦才听得见别人"(开麦拿到录音权限,浏览器顺带放开了播放)。LiveKit 把这件事
    // 暴露成 canPlaybackAudio;被拦就亮一个按钮,让人亲手点一下 startAudio()
    .on(RoomEvent.AudioPlaybackStatusChanged, syncAudioGate)
    .on(RoomEvent.ActiveSpeakersChanged, syncSpeaking)
    // 关摄像头是 mute 不是 unpublish:画面停了但轨还在,不藏起来就是一块黑格子
    .on(RoomEvent.TrackMuted, (pub, who) => {
      if (pub.kind === Track.Kind.Video) setVideoHidden(keyOf(who.identity, pub.source), true);
      syncStage();
    })
    .on(RoomEvent.TrackUnmuted, (pub, who) => {
      if (pub.kind === Track.Kind.Video) setVideoHidden(keyOf(who.identity, pub.source), false);
      syncStage();
    })
    .on(RoomEvent.ParticipantConnected, syncStage)
    .on(RoomEvent.ParticipantDisconnected, syncStage);

  try {
    await room.connect(info.url, info.token);
    syncStage();
    // 还在"点了加入"这个手势的链条里,先试着解锁播放;不行就靠上面的闸
    room.startAudio().catch(() => {});
    syncAudioGate();
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
  await loadTasks(m.id);
  openStream();
  // 进会就把自己的字幕语言告诉服务端
  if (caps.lang !== "source") pushCaptionLang().catch(() => {});
}

async function pushCaptionLang() {
  if (!state.meeting) return;
  await api(`/meetings/${state.meeting.id}/captions`, { method: "POST", body: JSON.stringify({ lang: caps.lang }) });
}

/* ---------------------------------------------------------------- 共享屏幕(2026-10-30)
 *
 * 带声音:`audio: true` 让 Chrome / Edge 在共享标签页时出现「分享标签页音频」的勾;Windows 上
 * 共享整个屏幕还能带系统声音。Safari、Firefox、WKWebView 的 getDisplayMedia 不支持音频——
 * 有的忽略、有的报错,所以报错就退回只共享画面,并把这件事说出来。
 */
const SCREEN_SHARE_SUPPORTED = typeof navigator.mediaDevices?.getDisplayMedia === "function";

function syncScreenBtn() {
  const on = !!state.room?.localParticipant?.isScreenShareEnabled;
  $("screenBtn").textContent = on ? "停止共享" : "共享屏幕";
  $("screenBtn").classList.toggle("active", on);
}
$("screenBtn").onclick = async () => {
  const lp = state.room?.localParticipant;
  if (!lp) return;
  const next = !lp.isScreenShareEnabled;
  try {
    try {
      await lp.setScreenShareEnabled(next, { audio: true });
    } catch (e) {
      // 用户点了取消 ⇒ NotAllowedError,别再弹第二次;其余多半是这个浏览器不认 audio
      if (next && e?.name !== "NotAllowedError") {
        await lp.setScreenShareEnabled(true, { audio: false });
        toast("这个浏览器不支持带声音共享,已改为只共享画面");
      } else if (e?.name !== "NotAllowedError") {
        throw e;
      }
    }
    if (next && lp.isScreenShareEnabled && !lp.getTrackPublication(Track.Source.ScreenShareAudio)) {
      toast("画面已共享,但没带声音:Chrome / Edge 共享「标签页」并勾选「分享标签页音频」才有声音");
    }
  } catch (e) {
    toast(`共享失败:${e.message || e}`, true);
  }
  syncScreenBtn();
};

/** 播放被浏览器拦住时亮出「开启声音」;放开了就藏起来。 */
function syncAudioGate() {
  const blocked = !!state.room && !state.room.canPlaybackAudio;
  $("soundBtn").hidden = !blocked;
}
$("soundBtn").onclick = async () => {
  try {
    await state.room?.startAudio();
  } catch (e) {
    toast(`浏览器仍不允许播放:${e}`, true);
  }
  syncAudioGate();
};

/** 字幕:最近 10 秒的两句,按我选的语言。 */
function renderCaptions() {
  const now = Date.now();
  caps.recent = caps.recent.filter((c) => now - c.at < 10_000).slice(-2);
  const box = $("captions");
  if (!caps.on || caps.recent.length === 0) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  box.innerHTML = caps.recent.map((c) => `<div><b>${escapeHtml(c.speaker)}</b>${escapeHtml(c.text)}</div>`).join("");
}
setInterval(renderCaptions, 1000);
$("capBtn").onclick = () => {
  caps.on = !caps.on;
  localStorage.setItem("muster.captions", caps.on ? "1" : "0");
  $("capBtn").classList.toggle("active", caps.on);
  renderCaptions();
};
$("capLang").onchange = () => {
  caps.lang = $("capLang").value;
  localStorage.setItem("muster.captionLang", caps.lang);
  pushCaptionLang().catch((e) => toast(String(e.message || e), true));
};

/* ---------------------------------------------------------------- 席位
 *
 * 每个参会者一个**持久**的格子,按事件打补丁而不是整体重建——
 * 重建会把已经挂上去的 <video> 从 DOM 上扯下来,画面就断了。
 *
 * 发言时描边变绿,数据来自 LiveKit 的 ActiveSpeakersChanged。
 * 这一点值得说清楚:概念稿里那个绿框是写死的 `<Seat speaking />`,
 * 这里的是真的——**界面上的东西要么是真的,要么明说自己不是。**
 */
const seats = new Map(); // identity → 格子元素

const nameOf = (p) => p.name || p.identity;
const everyone = () =>
  state.room ? [state.room.localParticipant, ...state.room.remoteParticipants.values()] : [];

function clearStage() {
  seats.clear();
  for (const v of videos.values()) v.el.remove();
  videos.clear();
  focus.key = null;
  focus.pinned = false;
  focus.dismissed = null;
  $("focus").hidden = true;
  $("stage").innerHTML = "";
  $("audioSink").innerHTML = "";
  $("stageHint").textContent = "";
}

function seatFor(p) {
  let el = seats.get(p.identity);
  if (el) return el;
  el = document.createElement("div");
  el.className = "seat";
  el.innerHTML =
    '<div class="media"><div class="initial"></div></div>' +
    '<div class="bar2"><span class="nm"></span><span class="you"></span><span class="mic"></span></div>';
  // 点席位里的画面 ⇒ 钉到焦点区
  el.querySelector(".media").onclick = () => {
    const key = [...videos.keys()].find((k) => k.endsWith(":" + p.identity) && videos.get(k).el.parentElement === el.querySelector(".media"));
    if (!key) return;
    focus.key = key;
    focus.pinned = true;
    placeVideos();
  };
  seats.set(p.identity, el);
  $("stage").appendChild(el);
  return el;
}

/* ---------------------------------------------------------------- 画面与焦点区(2026-10-30)
 *
 * 每条视频轨按「来源:谁」记一个键(cam:alice / screen:alice),摄像头和共享屏幕**不再塞进同一个格子**。
 * 画面放哪由 placeVideos 决定:焦点区放一路,其余回各自的席位。有人共享屏幕就自动聚焦它
 * (Teams 式);点席位里的画面可以把摄像头钉上来;「收起」后同一路共享不再自动弹回,直到有新的共享。
 *
 * 停止共享后格子变黑,之前的原因有两个:摄像头和屏幕两条轨都在同一个 .media 里,摘错或没摘;
 * 以及轨结束了但 TrackUnsubscribed 晚到。现在按键摘,并且 Unpublished / 底层 ended 都触发摘除。
 */
const videos = new Map(); // key → { el, track, who, source, hidden }
const focus = { key: null, pinned: false, dismissed: null };
const keyOf = (who, source) => (source === Track.Source.ScreenShare ? "screen:" : "cam:") + who;

function addVideo(track, who, source) {
  const key = keyOf(who, source);
  removeVideo(key, true);
  const el = track.attach();
  videos.set(key, { el, track, who, source, hidden: false });
  // 共享者点浏览器自己的「停止共享」:底层轨先 ended,LiveKit 的事件随后才到(或不到)
  track.mediaStreamTrack?.addEventListener?.("ended", () => {
    if (videos.get(key)?.track === track) removeVideo(key);
  });
  if (source === Track.Source.ScreenShare) {
    focus.dismissed = null; // 新的共享来了,之前收起的不算数
    if (!focus.pinned) focus.key = key;
  }
  placeVideos();
}

function removeVideo(key, quiet = false) {
  const v = videos.get(key);
  if (!v) return;
  videos.delete(key);
  v.el.remove();
  try {
    v.track.detach(v.el);
  } catch {
    /* 轨已经没了 */
  }
  if (focus.key === key) {
    focus.key = null;
    focus.pinned = false;
  }
  if (!quiet) placeVideos();
}

function setVideoHidden(key, hidden) {
  const v = videos.get(key);
  if (!v) return;
  v.hidden = hidden;
  v.el.style.display = hidden ? "none" : "";
  if (hidden && focus.key === key && !focus.pinned) focus.key = null;
  placeVideos();
}

function placeVideos() {
  // 没人钉住时:有共享就聚焦最新的那路(收起过的除外)
  if (!focus.pinned && !focus.key) {
    const s = [...videos.keys()].filter((k) => k.startsWith("screen:") && k !== focus.dismissed && !videos.get(k).hidden).pop();
    if (s) focus.key = s;
  }
  if (focus.key && !videos.has(focus.key)) {
    focus.key = null;
    focus.pinned = false;
  }
  for (const [key, v] of videos) {
    const seat = seats.get(v.who);
    const host = key === focus.key ? $("focusHost") : seat?.querySelector(".media");
    if (!host) {
      v.el.remove();
      continue;
    }
    if (v.el.parentElement !== host) host.appendChild(v.el);
  }
  // 同一个席位里既有摄像头又有(没被聚焦的)共享:共享更有信息量,摄像头先让位
  for (const v of videos.values()) {
    if (v.source === Track.Source.ScreenShare || v.hidden) continue;
    const screenHere = [...videos.values()].some((o) => o.who === v.who && o.source === Track.Source.ScreenShare && o.el.parentElement === v.el.parentElement);
    v.el.style.display = screenHere ? "none" : "";
  }
  const fv = focus.key ? videos.get(focus.key) : null;
  $("focus").hidden = !fv;
  if (fv) {
    const p = everyone().find((x) => x.identity === fv.who);
    const n = p ? nameOf(p) : fv.who;
    $("focusLabel").textContent = fv.source === Track.Source.ScreenShare ? `${n} 正在共享屏幕` : `${n} 的画面`;
  } else if (document.fullscreenElement === $("focus")) {
    document.exitFullscreen?.().catch(() => {});
  }
  for (const [id, el] of seats) {
    const media = el.querySelector(".media");
    media.classList.toggle("clickable", [...videos.values()].some((v) => v.who === id && !v.hidden && v.el.parentElement === media));
  }
}

const toggleFullscreen = () => {
  if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
  else $("focus").requestFullscreen?.().catch((e) => toast(`全屏被拒:${e.message || e}`, true));
};
$("fsBtn").onclick = toggleFullscreen;
$("focusHost").ondblclick = toggleFullscreen;
$("unfocusBtn").onclick = () => {
  focus.dismissed = focus.key;
  focus.key = null;
  focus.pinned = false;
  placeVideos();
};
document.addEventListener("fullscreenchange", () => {
  $("fsBtn").textContent = document.fullscreenElement ? "退出全屏" : "全屏";
});

function updateSeat(p) {
  const el = seatFor(p);
  const name = nameOf(p);
  el.querySelector(".nm").textContent = name;
  el.querySelector(".nm").title = name;
  el.querySelector(".you").textContent = p.isLocal ? "(你)" : "";
  el.querySelector(".initial").textContent = [...name][0] || "?";
  // 有(可见的)画面就不显示首字母占位;关了摄像头的轨虽然还挂着,但 hidden
  const media = el.querySelector(".media");
  const showing = [...videos.values()].some((v) => v.who === p.identity && !v.hidden && v.el.parentElement === media && v.el.style.display !== "none");
  el.querySelector(".initial").style.display = showing ? "none" : "";

  const on = p.isMicrophoneEnabled;
  const mic = el.querySelector(".mic");
  mic.textContent = on ? "麦克风开" : "静音";
  mic.className = on ? "mic" : "mic off";
  el.classList.toggle("speaking", p.isSpeaking);
}

/** 只改绿框。说话状态变化很频繁,不必为它重排整个席位。 */
function syncSpeaking() {
  for (const p of everyone()) {
    seats.get(p.identity)?.classList.toggle("speaking", p.isSpeaking);
  }
}

function syncStage() {
  const all = everyone();
  const live = new Set(all.map((p) => p.identity));
  for (const [id, el] of seats) {
    if (!live.has(id)) {
      el.remove();
      seats.delete(id);
    }
  }
  all.forEach(updateSeat);

  $("peers").textContent = `${all.length} 人在场`;
  // 没人开摄像头时别留一片空白格子而不解释
  const anyVideo = [...videos.values()].some((v) => !v.hidden);
  $("stageHint").textContent = !state.room
    ? ""
    : anyVideo
      ? ""
      : "没有人开摄像头。语音是通的——谁在说话,格子的描边会变绿。";

  // Agent 在不在必须看得见——它不在时说话不会被转写
  const here = all.some((p) => nameOf(p) === "A-007" || nameOf(p) === "小七");
  $("agentState").textContent = here ? "Agent 在会中" : "Agent 不在会中";
  $("agentState").className = "badge " + (here ? "on" : "off");
}

async function loadTranscript(mid) {
  try {
    const rows = await api(`/meetings/${mid}/transcript`);
    for (const r of rows) addLine(r.speaker_id, r.text, r.ts_ms, r.lang, r.origin_id, r.id, true);
  } catch {
    /* 拉不到就从现在开始记 */
  }
}

/** 一行纪要。翻译行(origin_id 非空)只在它是我选的语言时显示:挂在原文下面、进字幕。 */
function addLine(speaker, text, ts, lang = "zh", originId = null, id = 0, history = false) {
  if (originId !== null && originId !== undefined) {
    if (lang !== caps.lang) return;
    const src = $("lines").querySelector(`.line[data-id="${originId}"]`);
    if (src && !src.querySelector(".tr")) {
      const tr = document.createElement("div");
      tr.className = "tr";
      tr.textContent = text;
      src.appendChild(tr);
    }
    if (!history) {
      caps.recent.push({ id: originId, speaker, text, at: Date.now() });
      // 译文到了就替换掉同一句的原文字幕
      caps.recent = caps.recent.filter((c, i, a) => !(c.id === originId && i !== a.length - 1));
      renderCaptions();
    }
    return;
  }
  if (id && $("lines").querySelector(`.line[data-id="${id}"]`)) return; // 历史与推送重叠
  const d = document.createElement("div");
  d.className = "line";
  if (id) d.dataset.id = String(id);
  const t = new Date(ts).toTimeString().slice(0, 8);
  d.innerHTML = `<b class="${speaker === "系统" ? "sys" : ""}">${escapeHtml(speaker)}</b> <span class="dim">${t}</span><div>${escapeHtml(text)}</div>`;
  $("lines").appendChild(d);
  $("lines").scrollTop = $("lines").scrollHeight;
  // 跟原文的字幕;选了别的语言时原文先上,译文到了再换
  if (!history) {
    caps.recent.push({ id, speaker, text, at: Date.now() });
    renderCaptions();
  }
}
const escapeHtml = (s) =>
  s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

/** SSE。凭据是一次性订阅票:浏览器自动重连会带着用过的票撞 401,
 *  所以断了自己换票再连,并把最后收到的消息号带上,服务端据此补发漏掉的消息。 */
let streamGen = 0;
let streamLastId = null;
let streamDelay = 1000;
async function openStream() {
  state.es?.close();
  state.es = null;
  const gen = ++streamGen;
  let ticket;
  try {
    ticket = (await api("/events/ticket", { method: "POST", body: "{}" })).ticket;
  } catch (e) {
    if (gen !== streamGen) return;
    setTimeout(openStream, streamDelay);
    streamDelay = Math.min(streamDelay * 2, 30000);
    return;
  }
  if (gen !== streamGen) return;
  const qs = `ticket=${encodeURIComponent(ticket)}` + (streamLastId ? `&last_event_id=${encodeURIComponent(streamLastId)}` : "");
  const es = new EventSource(`${state.base}/events?${qs}`);
  es.onopen = () => {
    streamDelay = 1000;
  };
  es.onerror = () => {
    if (gen !== streamGen) return;
    es.close();
    setTimeout(openStream, streamDelay);
    streamDelay = Math.min(streamDelay * 2, 30000);
  };
  es.onmessage = (e) => {
    // 只记消息的序号(推送类事件的 id 是 0,带上它会让服务端从头补发)
    if (Number(e.lastEventId) > 0) streamLastId = e.lastEventId;
    try {
      const ev = JSON.parse(e.data);
      if (ev.type === "transcript" && ev.meeting_id === state.meeting?.id) {
        addLine(ev.speaker_id, ev.text, ev.ts_ms, ev.lang || "zh", ev.origin_id ?? null, ev.id || 0);
      } else if (ev.type === "recording" && ev.meeting_id === state.meeting?.id) {
        // 录制开始 / 保存中 / 已保存 / 失败:人人可见
        const on = ev.state === "started" || ev.state === "stopping";
        $("recBadge").hidden = !on;
        $("recBadge").textContent = ev.state === "stopping" ? "● 录制保存中…" : "● 正在录制";
        if (ev.state === "ready") toast("录像已保存");
        if (ev.state === "failed") toast("录制失败", true);
      } else if (ev.type === "action_item" && ev.meeting_id === state.meeting?.id) {
        upsertTask(ev);
      } else if (ev.type === "meeting") {
        // 会议开始 / 结束 / 请了 Agent:列表重拉;自己在的那场被结束了要说出来
        refreshMeetings().catch(() => {});
        if (ev.event === "ended" && ev.meeting_id === state.meeting?.id) {
          toast("这场会已被结束,纪要与待批任务保留");
        }
      }
    } catch {
      /* 单条坏了不该拖垮整条流 */
    }
  };
  state.es = es;
}

/* ---------------------------------------------------------------- 待批任务
 *
 * Agent 提,人批。**这个中间步骤不是流程负担,是授权边界**——
 * 服务端那边写得更清楚:转写会出错、会上一句话的意图强度低于在任务框里
 * 写下的需求,而 Runner 跑在开发者自己的机器上。
 *
 * 所以这里的按钮是"批准 / 驳回",不是"执行"。批准之后由持有那份代码的
 * 节点去跑,服务端只记号。
 */
const taskEls = new Map(); // action item id → 元素

async function loadTasks(mid) {
  try {
    (await api(`/meetings/${mid}/action-items`)).forEach(upsertTask);
  } catch {
    /* 拉不到就从现在开始收 */
  }
}

function upsertTask(it) {
  let el = taskEls.get(it.id);
  if (!el) {
    el = document.createElement("div");
    el.className = "task";
    taskEls.set(it.id, el);
    $("tasks").appendChild(el);
  }
  const pending = it.status === "proposed";
  el.classList.toggle("done", !pending);
  el.innerHTML =
    `<div class="t"></div>` +
    (it.source_quote ? `<div class="q">原话:<span class="sq"></span></div>` : "") +
    `<div class="acts"></div>`;
  el.querySelector(".t").textContent = it.text + (it.owner_hint ? ` — ${it.owner_hint}` : "");
  if (it.source_quote) el.querySelector(".sq").textContent = it.source_quote;

  const acts = el.querySelector(".acts");
  if (pending) {
    const ok = document.createElement("button");
    ok.className = "ok";
    ok.textContent = "批准";
    ok.onclick = () => decide(it.id, true, ok);
    const no = document.createElement("button");
    no.textContent = "驳回";
    no.onclick = () => decide(it.id, false, no);
    acts.append(ok, no);
  } else {
    const v = document.createElement("span");
    v.className = "verdict";
    v.style.color = it.status === "confirmed" ? "var(--green)" : "var(--faint)";
    v.textContent =
      (it.status === "confirmed" ? "已批准" : "已驳回") +
      (it.decided_by ? ` · ${it.decided_by}` : "");
    acts.append(v);
  }
  refreshTaskCount();
}

function refreshTaskCount() {
  const n = [...taskEls.values()].filter((e) => !e.classList.contains("done")).length;
  $("tasksCard").hidden = taskEls.size === 0;
  $("taskCount").textContent = n ? `${n} 条待批` : "无待批";
  $("taskCount").className = "badge " + (n ? "off" : "on");
}

async function decide(id, confirm, btn) {
  btn.disabled = true;
  try {
    upsertTask(await api(`/action-items/${id}/decide`, {
      method: "POST",
      body: JSON.stringify({ confirm }),
    }));
  } catch (e) {
    btn.disabled = false;
    // 403 多半是没有 CreateTask 权限,或者你是 Agent 账号——两种都要说清
    toast(`没能裁决:${e.message || e}`, true);
  }
}

$("micBtn").onclick = async () => {
  const on = state.room?.localParticipant.isMicrophoneEnabled;
  try {
    await state.room.localParticipant.setMicrophoneEnabled(!on);
    $("micBtn").textContent = !on ? "麦克风开" : "开麦";
    $("micBtn").classList.toggle("active", !on);
    syncStage(); // 自己的格子也要跟着变

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
  clearStage();
  show("lobby");
  refreshMeetings().catch(() => {});
};

// ---------------------------------------------------------------- 启动

(function boot() {
  $("server").value = location.origin;
  syncRegisterEntry();
  // 带着重置链接进来:直接显示重置表单,不管有没有旧会话
  if (new URLSearchParams(location.search).get("reset")) {
    show("login");
    authForms("reset");
    return;
  }
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
