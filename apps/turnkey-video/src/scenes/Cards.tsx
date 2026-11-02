import React from "react";
import { useCurrentFrame } from "remotion";
import { Mark, Scene, appear } from "../kit";
import { c, font } from "../theme";

/** The opening card: the landing page's headline. */
export const Title: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const a = appear(f, 4, 16);
  const b = appear(f, 16, 16);
  return (
    <Scene dur={dur} bg={c.deep}>
      <div style={{ position: "absolute", left: 120, top: 190, color: c.onDeep }}>
        <div style={{ opacity: a, transform: `translateY(${(1 - a) * 16}px)`, display: "flex", alignItems: "center", gap: 16,
          font: `700 30px/1 ${font.ui}`, letterSpacing: ".02em" }}>
          <Mark size={56} bg={c.onDeep} egg={c.deep} face={c.onDeep} />Devagotchi
        </div>
        <div style={{ marginTop: 34, maxWidth: 900, font: `700 64px/1.04 ${font.display}`, letterSpacing: "-0.02em",
          opacity: b, transform: `translateY(${(1 - b) * 16}px)` }}>
          One hub between your AI and every machine you have.
        </div>
        <div style={{ marginTop: 26, color: c.onDeepMuted, font: `24px/1.4 ${font.ui}`, opacity: appear(f, 30, 16) }}>
          Set up from an empty server, step by step.
        </div>
      </div>
    </Scene>
  );
};

/** The closing card. */
export const Closing: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const a = appear(f, 4, 18);
  return (
    <Scene dur={dur} bg={c.deep}>
      <div style={{ position: "absolute", inset: 0, display: "grid", placeItems: "center", color: c.onDeep, textAlign: "center" }}>
        <div style={{ opacity: a, transform: `scale(${0.96 + 0.04 * a})` }}>
          <Mark size={120} bg={c.onDeep} egg={c.deep} face={c.onDeep} />
          <div style={{ marginTop: 34, font: `700 60px/1.08 ${font.display}`, letterSpacing: "-0.02em" }}>
            Devagotchi: your AI, on your machines.
          </div>
          <div style={{ marginTop: 18, color: c.onDeepMuted, font: `24px/1.4 ${font.ui}`, opacity: appear(f, 20, 16) }}>
            Self-host it with one command, or use the hosted hub.
          </div>
        </div>
      </div>
    </Scene>
  );
};
