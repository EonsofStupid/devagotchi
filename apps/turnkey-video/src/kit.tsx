import React from "react";
import { AbsoluteFill, Easing, interpolate, useCurrentFrame } from "remotion";
import { c, font } from "./theme";

export const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;
export const ease = Easing.bezier(0.2, 0.7, 0.2, 1);

/** 0 → 1 over `dur` frames from `at`. */
export const appear = (frame: number, at: number, dur = 12) =>
  interpolate(frame, [at, at + dur], [0, 1], { ...clamp, easing: ease });

/** `text` typed from `at`, `cps` characters a second. */
export const typed = (text: string, frame: number, at: number, cps = 28) =>
  text.slice(0, Math.max(0, Math.floor(((frame - at) * cps) / 30)));

/** A scene: the hub's background, faded in and out at its edges. */
export const Scene: React.FC<{ dur: number; bg?: string; children: React.ReactNode }> = ({ dur, bg = c.bg, children }) => {
  const f = useCurrentFrame();
  const opacity = Math.min(appear(f, 0, 10), 1 - appear(f, dur - 10, 10));
  return <AbsoluteFill style={{ background: bg, opacity, fontFamily: font.ui, color: c.text }}>{children}</AbsoluteFill>;
};

/** The on-screen caption: the step's number and what is happening, bottom left. */
export const Caption: React.FC<{ n: number; text: string; at?: number; until?: number }> = ({ n, text, at = 6, until }) => {
  const f = useCurrentFrame();
  const shown = appear(f, at, 14) * (until === undefined ? 1 : 1 - appear(f, until, 8));
  return (
    <div style={{ position: "absolute", left: 64, bottom: 44, display: "flex", alignItems: "baseline", gap: 18,
      opacity: shown, transform: `translateY(${(1 - shown) * 12}px)` }}>
      <span style={{ font: `700 54px/1 ${font.display}`, color: c.accent }}>{n}</span>
      <span style={{ font: `600 28px/1.2 ${font.ui}`, color: c.text, letterSpacing: "-0.01em" }}>{text}</span>
    </div>
  );
};

/** The app icon's pet. */
export const Mark: React.FC<{ size: number; bg?: string; egg?: string; face?: string }> = ({ size, bg = c.accent, egg = c.bg, face = c.text }) => (
  <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden>
    <rect width="32" height="32" rx="8" fill={bg} />
    <ellipse cx="16" cy="17.3" rx="9.4" ry="10.6" fill={egg} />
    <circle cx="12.9" cy="15.6" r="1.4" fill={face} />
    <circle cx="19.1" cy="15.6" r="1.4" fill={face} />
    <path d="M13.5 20.1q2.5 1.9 5 0" fill="none" stroke={face} strokeWidth="1.4" strokeLinecap="round" />
  </svg>
);

/** The hub's top bar, as every hub page has it. */
export const HubBar: React.FC<{ right?: React.ReactNode; compact?: boolean; markOnly?: boolean }> = ({ right, compact, markOnly }) => (
  <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", height: compact ? 56 : 60,
    padding: "0 20px", borderBottom: `1px solid ${c.line}`, background: c.bg }}>
    <span style={{ display: "inline-flex", alignItems: "center", gap: 8, fontWeight: 700, letterSpacing: ".02em", color: c.accent }}>
      <Mark size={26} />{markOnly ? null : "Devagotchi"}
    </span>
    {right}
  </div>
);

export const button = (primary = false, extra: React.CSSProperties = {}): React.CSSProperties => ({
  display: "inline-block", padding: "12px 18px", borderRadius: 6, font: `${primary ? 600 : 400} 16px/1.3 ${font.ui}`,
  border: `1px solid ${primary ? c.accent : c.line}`, background: primary ? c.accent : c.surface,
  color: primary ? c.bg : c.text, textAlign: "center", ...extra,
});

export const field: React.CSSProperties = {
  display: "block", width: "100%", padding: 10, marginTop: 4, border: `1px solid ${c.line}`, borderRadius: 6,
  background: c.bg, color: c.text, font: `16px/1.4 ${font.ui}`, minHeight: 44, boxSizing: "border-box",
};

export const label: React.CSSProperties = { display: "block", color: c.muted, fontSize: 14, marginTop: 12 };

/** A terminal on a server: the hub's deep green, the product's monospace. */
export const Terminal: React.FC<{ title: string; width: number; height: number; children: React.ReactNode; style?: React.CSSProperties }> = ({ title, width, height, children, style }) => (
  <div style={{ width, height, borderRadius: 16, background: c.deep, color: c.onDeep, overflow: "hidden",
    boxShadow: "0 40px 80px -40px rgba(12,40,26,.55)", ...style }}>
    <div style={{ height: 40, display: "flex", alignItems: "center", padding: "0 20px", borderBottom: "1px solid rgba(255,255,255,.08)",
      color: c.onDeepMuted, font: `600 14px/1 ${font.ui}` }}>{title}</div>
    <div style={{ padding: "18px 22px", font: `17px/1.65 ${font.mono}`, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{children}</div>
  </div>
);

export const Cursor: React.FC<{ on?: boolean }> = ({ on = true }) => {
  const f = useCurrentFrame();
  return on && Math.floor(f / 15) % 2 === 0 ? (
    <span style={{ display: "inline-block", width: "0.6em", height: "1.1em", verticalAlign: "-0.2em", background: c.onDeep, opacity: 0.8 }} />
  ) : null;
};

/** A phone. Its screen is laid out at a phone's width in CSS pixels and scaled down. */
export const Phone: React.FC<{ children: React.ReactNode; scale?: number; style?: React.CSSProperties }> = ({ children, scale = 0.86, style }) => {
  const w = 340 * scale;
  const h = 700 * scale;
  return (
    <div style={{ width: w + 24, height: h + 24, padding: 12, borderRadius: 46, background: c.phone,
      boxShadow: "0 40px 80px -36px rgba(12,40,26,.6)", ...style }}>
      <div style={{ width: w, height: h, borderRadius: 34, overflow: "hidden", background: c.bg, position: "relative" }}>
        <div style={{ width: 340, height: 700, transform: `scale(${scale})`, transformOrigin: "0 0", position: "relative" }}>{children}</div>
      </div>
    </div>
  );
};

/** A desktop window: an address bar and a page. */
export const Window: React.FC<{ url: string; width: number; height: number; children: React.ReactNode; style?: React.CSSProperties }> = ({ url, width, height, children, style }) => (
  <div style={{ width, height, borderRadius: 14, overflow: "hidden", background: c.bg, border: `1px solid ${c.line}`,
    boxShadow: "0 40px 80px -40px rgba(12,40,26,.45)", display: "flex", flexDirection: "column", ...style }}>
    <div style={{ height: 44, flex: "none", display: "flex", alignItems: "center", gap: 12, padding: "0 16px", background: c.app,
      borderBottom: `1px solid ${c.appLine}` }}>
      <div style={{ flex: 1, height: 28, borderRadius: 8, background: "#efefec", color: c.appMuted, display: "flex",
        alignItems: "center", padding: "0 12px", font: `14px/1 ${font.ui}` }}>{url}</div>
    </div>
    <div style={{ flex: 1, position: "relative", overflow: "hidden" }}>{children}</div>
  </div>
);

/** Where a finger or a pointer presses. */
export const Tap: React.FC<{ x: number; y: number; at: number; color?: string }> = ({ x, y, at, color = c.accent }) => {
  const f = useCurrentFrame();
  if (f < at - 6 || f > at + 18) return null;
  const t = interpolate(f, [at - 6, at, at + 18], [0, 0.55, 0], clamp);
  const s = interpolate(f, [at - 6, at + 18], [0.6, 1.6], clamp);
  return (
    <div style={{ position: "absolute", left: x - 28, top: y - 28, width: 56, height: 56, borderRadius: "50%",
      background: color, opacity: t, transform: `scale(${s})`, pointerEvents: "none" }} />
  );
};

/** The hub's status dot and label. */
export const Status: React.FC<{ online: boolean }> = ({ online }) => (
  <span style={{ color: online ? c.online : "#8a6d1d", fontWeight: 600 }}>● {online ? "Online" : "Not enrolled"}</span>
);
