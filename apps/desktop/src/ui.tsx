/* v4 原子件(自概念稿移植,加了 TS 类型) */
import React from "react";
import { Bot, ChevronDown, ChevronRight, Cloud, HardDrive } from "lucide-react";
import { T } from "./theme";

type Kids = { children?: React.ReactNode };

export const Card = ({ children, className = "", style = {} }: Kids & { className?: string; style?: React.CSSProperties }) => (
  <div className={`rounded-2xl ${className}`} style={{ background: T.shell, border: `1px solid ${T.line}`, ...style }}>
    {children}
  </div>
);

export const Tag = ({ children, tone, style = {} }: Kids & { tone?: "ind" | "red" | "grn" | "amb" | "teal"; style?: React.CSSProperties }) => {
  const m: Record<string, [string, string]> = {
    ind: [T.indigoSoft, T.indigo],
    red: [T.redSoft, T.red],
    grn: [T.greenSoft, T.green],
    amb: [T.amberSoft, T.amber],
    teal: [T.tealSoft, T.teal],
  };
  const [bg, fg] = tone ? m[tone] : [T.soft, T.sub];
  return (
    <span className="inline-flex items-center gap-1 text-[10.5px] px-2 py-0.5 rounded-lg" style={{ background: bg, color: fg, ...style }}>
      {children}
    </span>
  );
};

export const Pct = ({ up, hero, children }: Kids & { up?: boolean; hero?: boolean }) => (
  <span
    className="inline-flex text-[10.5px] font-semibold px-2 py-0.5 rounded-full"
    style={hero ? { background: "rgba(255,255,255,.2)", color: "#fff" } : { background: up ? T.greenSoft : T.redSoft, color: up ? T.green : T.red }}
  >
    {children}
  </span>
);

export const RouteTag = ({ local }: { local?: boolean }) => (
  <span
    className="inline-flex items-center gap-1 text-[10.5px] font-semibold px-2 py-0.5 rounded-full"
    style={{ background: local ? T.tealSoft : T.indigoSoft, color: local ? T.teal : T.indigo }}
  >
    {local ? <HardDrive size={11} /> : <Cloud size={11} />}
    {local ? "本地" : "云端"}
  </span>
);

export const LvTag = ({ level }: { level: string }) => (
  <Tag tone={level === "restricted" ? "red" : level === "internal" ? "amb" : undefined}>{level}</Tag>
);

export const ChipDd = ({ children }: Kids) => (
  <span className="inline-flex items-center gap-1 text-[11px] px-2.5 py-1 rounded-full" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}>
    {children} <ChevronDown size={12} />
  </span>
);

export const IBtn = ({ children, onClick, className = "", disabled }: Kids & { onClick?: () => void; className?: string; disabled?: boolean }) => (
  <button
    onClick={onClick}
    disabled={disabled}
    className={`inline-flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl ${className}`}
    style={{ background: T.indigo, color: "#fff", opacity: disabled ? 0.5 : 1 }}
  >
    {children}
  </button>
);

export const SideSec = ({ children }: Kids) => (
  <div className="mt-4 mb-1.5 px-2 text-[10px] font-semibold tracking-widest" style={{ color: "#B9BCCB" }}>
    {children}
  </div>
);

export const SideItem = ({
  icon,
  label,
  active,
  onClick,
  extra,
}: {
  icon: React.ReactNode;
  label: string;
  active?: boolean;
  onClick?: () => void;
  extra?: React.ReactNode;
}) => (
  <button
    onClick={onClick}
    className="w-full flex items-center gap-2.5 px-3 py-2 rounded-xl text-left text-[13px]"
    style={{
      background: active ? T.indigo : "transparent",
      color: active ? "#fff" : "#5A5E70",
      fontWeight: active ? 600 : 500,
      boxShadow: active ? "0 6px 14px rgba(91,91,245,.28)" : "none",
    }}
  >
    {icon}
    {label}
    {extra}
  </button>
);

export const CollapseSec = ({ label, open, onToggle, children }: Kids & { label: string; open: boolean; onToggle: () => void }) => (
  <>
    <button onClick={onToggle} className="w-full flex items-center px-2 mt-4 mb-1.5">
      <span className="text-[10px] font-semibold tracking-widest" style={{ color: "#B9BCCB" }}>
        {label}
      </span>
      <ChevronDown size={12} className="ml-auto" style={{ color: "#B9BCCB", transform: open ? "none" : "rotate(-90deg)", transition: "transform .15s" }} />
    </button>
    {open && <div className="fade">{children}</div>}
  </>
);

export const Bub = ({ children, fresh }: Kids & { fresh?: boolean }) => (
  <div className={`flex gap-2 ${fresh ? "fade" : ""}`}>
    <div className="w-6 h-6 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.indigo, color: "#fff" }}>
      <Bot size={12} />
    </div>
    <div className="rounded-xl px-3 py-2 leading-relaxed" style={{ background: T.soft }}>
      {children}
    </div>
  </div>
);

export const CB = ({ who, bot, children }: Kids & { who: string; bot?: boolean }) => (
  <div className="flex gap-2.5">
    <div
      className="w-7 h-7 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold"
      style={{ background: bot ? T.indigoSoft : "#E4E6EF", color: bot ? T.indigo : "#5A5E70" }}
    >
      {bot ? <Bot size={14} /> : who[0]}
    </div>
    <div className="min-w-0">
      <div className="text-[11px] font-semibold" style={{ color: bot ? T.indigo : T.ink }}>
        {who}
      </div>
      <div className="mt-0.5 leading-relaxed rounded-xl px-3 py-2" style={{ background: T.soft, color: "#454A5C" }}>
        {children}
      </div>
    </div>
  </div>
);

export const Kpi = ({
  hero,
  icon,
  pct,
  label,
  val,
  cap,
}: {
  hero?: boolean;
  icon: React.ReactNode;
  pct?: React.ReactNode;
  label: string;
  val: React.ReactNode;
  cap: string;
}) => (
  <div
    className="rounded-2xl p-4"
    style={hero ? { background: T.indigo, color: "#fff", boxShadow: "0 10px 24px rgba(91,91,245,.3)" } : { background: "#fff", border: `1px solid ${T.line}` }}
  >
    <div className="flex items-center">
      <div className="w-9 h-9 rounded-xl flex items-center justify-center" style={{ background: hero ? "#fff" : T.soft, color: hero ? T.indigo : T.ink }}>
        {icon}
      </div>
      <span className="ml-auto">{pct}</span>
    </div>
    <div className="text-xs mt-3.5" style={{ color: hero ? "#DCDCFE" : T.sub }}>
      {label}
    </div>
    <div className="text-[26px] font-extrabold mt-0.5 tracking-tight">{val}</div>
    <div className="text-[10.5px] mt-0.5" style={{ color: hero ? "#BDBDF9" : T.faint }}>
      {cap}
    </div>
  </div>
);

export const TodoRow = ({ tone, t, n }: { tone: string; t: string; n: string }) => (
  <div className="flex items-center gap-3 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
    <span className="w-1 rounded-full" style={{ height: 34, background: tone }} />
    <div>
      <div className="text-[13px] font-semibold">{t}</div>
      <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>
        {n}
      </div>
    </div>
    <span className="ml-auto w-7 h-7 rounded-full flex items-center justify-center" style={{ background: T.soft, color: "#5A5E70" }}>
      <ChevronRight size={14} />
    </span>
  </div>
);

export const LegRow = ({ icon, l, v, p }: { icon: React.ReactNode; l: string; v: React.ReactNode; p?: React.ReactNode }) => (
  <div className="flex items-center gap-2.5 py-2" style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="w-7 h-7 rounded-lg flex items-center justify-center" style={{ background: T.soft, color: "#5A5E70" }}>
      {icon}
    </div>
    <span className="text-[12.5px] font-medium">{l}</span>
    <span className="ml-auto text-[13px] font-bold">{v}</span>
    {p}
  </div>
);

export const ApRow = ({ bg, icon, nm, sb, right }: { bg: string; icon?: React.ReactNode; nm: string; sb: string; right?: React.ReactNode }) => (
  <div className="flex items-center gap-2.5 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="w-8 h-8 rounded-full flex items-center justify-center shrink-0" style={{ background: bg, color: "#fff" }}>
      {icon || <Bot size={15} />}
    </div>
    <div className="min-w-0">
      <div className="text-[13px] font-semibold">{nm}</div>
      <div className="text-[11px] truncate" style={{ color: T.sub, maxWidth: 150 }}>
        {sb}
      </div>
    </div>
    <span className="ml-auto shrink-0">{right}</span>
  </div>
);
