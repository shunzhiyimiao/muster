/* 个人资料(2026-11-03,服务端 profile.rs):头像、显示名、邮箱、部门 / 职位、简介、偏好的模型通道,
   以及我自己的登录记录。只在连着服务端时出现——资料本来就是服务端上的东西。 */
import { useEffect, useRef, useState } from "react";
import { T } from "../theme";
import { Card } from "../ui";
import { RemoteLoginEvent, RemoteProfile, api, fmtTime } from "../api";

export function ProfileView() {
  const [p, setP] = useState<RemoteProfile | null>(null);
  const [avatar, setAvatar] = useState<string | null>(null);
  const [providers, setProviders] = useState<{ id: string; name: string; locality: string }[]>([]);
  const [logins, setLogins] = useState<RemoteLoginEvent[]>([]);
  const [form, setForm] = useState({ display_name: "", email: "", department: "", title: "", bio: "", preferred_provider: "" });
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const file = useRef<HTMLInputElement>(null);

  const load = async () => {
    try {
      const prof = await api.remoteProfile();
      setP(prof);
      setForm({
        display_name: prof.display_name ?? "",
        email: prof.email ?? "",
        department: prof.department ?? "",
        title: prof.title ?? "",
        bio: prof.bio ?? "",
        preferred_provider: prof.preferred_provider ?? "",
      });
      if (prof.avatar_url) {
        const bytes = await api.remoteAvatar(prof.id);
        setAvatar(bytes ? URL.createObjectURL(new Blob([new Uint8Array(bytes)])) : null);
      } else {
        setAvatar(null);
      }
      api.remoteMyLogins(20).then(setLogins).catch(() => setLogins([]));
      api
        .remoteCatalog()
        .then((cat) => {
          const list = Object.entries((cat?.providers ?? {}) as Record<string, { display_name?: string; locality?: string }>).map(([id, c]) => ({
            id,
            name: c.display_name || id,
            locality: c.locality || "",
          }));
          setProviders(list);
        })
        .catch(() => setProviders([]));
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    }
  };
  useEffect(() => {
    load();
  }, []);

  const save = async () => {
    setBusy(true);
    setMsg(null);
    try {
      const prof = await api.remoteProfileUpdate(form);
      setP(prof);
      setMsg({ ok: true, text: "已保存" });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const pick = async (f: File | undefined) => {
    if (!f) return;
    if (f.size > 512 * 1024) return setMsg({ ok: false, text: "头像最大 512 KiB" });
    setBusy(true);
    setMsg(null);
    try {
      const buf = new Uint8Array(await f.arrayBuffer());
      await api.remoteAvatarUpload(Array.from(buf));
      await load();
      setMsg({ ok: true, text: "头像已更新" });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
      if (file.current) file.current.value = "";
    }
  };

  const field = (label: string, key: keyof typeof form, type = "text", span = 1) => (
    <label className="block" style={{ gridColumn: `span ${span}` }}>
      <span className="text-[11px]" style={{ color: T.faint }}>{label}</span>
      <input
        type={type}
        value={form[key]}
        onChange={(e) => setForm((f) => ({ ...f, [key]: e.target.value }))}
        className="w-full mt-1 px-3 py-2 rounded-xl text-xs outline-none"
        style={{ background: T.panel, border: `1px solid ${T.line}` }}
      />
    </label>
  );

  return (
    <div className="px-7 pb-8 pt-2">
      <Card className="p-5">
        <div className="flex items-start gap-5">
          <div className="text-center">
            {avatar ? (
              <img src={avatar} alt="" className="w-20 h-20 rounded-full object-cover" style={{ background: T.panel }} />
            ) : (
              <div className="w-20 h-20 rounded-full flex items-center justify-center text-2xl font-bold text-white" style={{ background: T.indigo }}>
                {[...(p?.display_name || p?.id || "?")][0]}
              </div>
            )}
            <button onClick={() => file.current?.click()} disabled={busy} className="mt-2 text-[11px] font-semibold" style={{ color: T.indigo }}>
              换头像
            </button>
            <input ref={file} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={(e) => pick(e.target.files?.[0])} />
            <div className="text-[10px] mt-1" style={{ color: T.faint }}>PNG / JPEG / WebP,≤ 512 KiB</div>
          </div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <b className="text-sm">个人资料</b>
              <span className="text-[11px]" style={{ color: T.faint }}>{p?.id}</span>
              {p?.last_login_ms && (
                <span className="ml-auto text-[11px]" style={{ color: T.faint }}>
                  最近登录 {fmtTime(p.last_login_ms)}{p.last_login_ip ? ` · ${p.last_login_ip}` : ""}
                </span>
              )}
            </div>
            <div className="grid grid-cols-2 gap-3 mt-3">
              {field("显示名", "display_name")}
              {field("邮箱(唯一,用于找回口令)", "email", "email")}
              {field("部门", "department")}
              {field("职位", "title")}
              <label className="block" style={{ gridColumn: "span 2" }}>
                <span className="text-[11px]" style={{ color: T.faint }}>简介</span>
                <textarea
                  rows={3}
                  value={form.bio}
                  onChange={(e) => setForm((f) => ({ ...f, bio: e.target.value }))}
                  className="w-full mt-1 px-3 py-2 rounded-xl text-xs outline-none"
                  style={{ background: T.panel, border: `1px solid ${T.line}`, font: "inherit" }}
                />
              </label>
              <label className="block" style={{ gridColumn: "span 2" }}>
                <span className="text-[11px]" style={{ color: T.faint }}>偏好的模型通道(空 = 跟组织默认;受限级内容仍只会落本地,这里只是偏好)</span>
                <select
                  value={form.preferred_provider}
                  onChange={(e) => setForm((f) => ({ ...f, preferred_provider: e.target.value }))}
                  className="w-full mt-1 px-3 py-2 rounded-xl text-xs outline-none"
                  style={{ background: T.panel, border: `1px solid ${T.line}` }}
                >
                  <option value="">跟组织默认</option>
                  {providers.map((o) => (
                    <option key={o.id} value={o.id}>
                      {o.name}{o.locality === "cloud" ? "(云端)" : o.locality === "local" ? "(本地)" : ""}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <div className="flex items-center gap-3 mt-4">
              <button onClick={save} disabled={busy} className="px-4 py-2 rounded-xl text-xs font-semibold whitespace-nowrap shrink-0" style={{ background: T.indigo, color: "#fff", opacity: busy ? 0.5 : 1 }}>
                保存
              </button>
              {msg && <span className="text-[11.5px]" style={{ color: msg.ok ? T.green : T.red }}>{msg.text}</span>}
            </div>
          </div>
        </div>
      </Card>

      <Card className="p-5 mt-4">
        <b className="text-sm">我的登录记录</b>
        <div className="text-[11px] mt-1" style={{ color: T.faint }}>最近 20 次。看到不是你的登录,马上改口令。</div>
        <div className="mt-3 divide-y" style={{ borderColor: T.line }}>
          {logins.map((l, i) => (
            <div key={i} className="flex items-center gap-3 py-1.5 text-[11.5px]">
              <span style={{ color: l.ok ? T.green : T.red }}>{l.ok ? "✓ 成功" : `✗ ${l.reason === "bad_password" ? "口令错" : l.reason === "pending" ? "待批准" : l.reason === "disabled" ? "已停用" : "失败"}`}</span>
              <span style={{ color: T.sub }}>{fmtTime(l.ts_ms)}</span>
              <span style={{ color: T.faint }}>{l.ip ?? "-"}</span>
              <span className="truncate" style={{ color: T.faint }}>{l.user_agent ?? ""}</span>
            </div>
          ))}
          {logins.length === 0 && <div className="py-2 text-[11.5px]" style={{ color: T.faint }}>还没有记录</div>}
        </div>
      </Card>
    </div>
  );
}
