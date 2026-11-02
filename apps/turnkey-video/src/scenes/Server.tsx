import React from "react";
import { useCurrentFrame } from "remotion";
import { Caption, Cursor, Scene, Terminal, appear, typed } from "../kit";
import { CLAIM_CODE, HUB, c } from "../theme";

const UP = "docker compose up -d";
const LOGS = 'docker compose logs hub | grep -A1 "Claim it in a browser"';
// What the hub prints when it starts with no owner (crates/hub/src/lib.rs), as Compose shows it.
const UNCLAIMED =
  "hub-1  | devagotchi hub is unclaimed: nobody can sign in as its owner yet. Claim it in a browser before 2026-11-02 18:00 UTC:";
const LINK = `${HUB}/claim#${CLAIM_CODE}`;

const Prompt: React.FC = () => <span style={{ color: c.onDeepMuted }}>~/devagotchi/deploy/compose $ </span>;

/** Steps 1 and 2: one command brings the hub up, and its log prints the claim link. */
export const Server: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const at = (frame: number) => (f >= frame ? 1 : 0);
  const upDone = f >= 14 + (UP.length * 30) / 24;
  const logsAt = 170;
  const logsDone = f >= logsAt + (LOGS.length * 30) / 40;
  const glow = appear(f, 236, 16);
  return (
    <Scene dur={dur}>
      <div style={{ position: "absolute", left: 140, top: 48 }}>
        <Terminal title="hub.example.com" width={1000} height={500}>
          <div>
            <Prompt />
            {typed(UP, f, 14, 24)}
            {!upDone && <Cursor />}
          </div>
          <div style={{ opacity: at(54), color: c.onDeepMuted }}>[+] Running 3/3</div>
          {["postgres-1", "hub-1     ", "caddy-1   "].map((name, i) => (
            <div key={name} style={{ opacity: at(62 + i * 9) }}>
              <span style={{ color: "#7cc4a0" }}> ✔</span> Container devagotchi-{name}  Started
            </div>
          ))}
          {f >= logsAt - 10 && (
            <div style={{ marginTop: 10 }}>
              <Prompt />
              {typed(LOGS, f, logsAt, 40)}
              {!logsDone && f >= logsAt && <Cursor />}
            </div>
          )}
          <div style={{ opacity: at(226), color: c.onDeep }}>{UNCLAIMED}</div>
          <div style={{ opacity: at(232) }}>
            hub-1  |{"   "}
            <span style={{ color: "#7cc4a0", fontWeight: 700, borderRadius: 6, padding: "1px 4px",
              background: `rgba(124,196,160,${0.16 * glow})`, boxShadow: `0 0 0 ${2 * glow}px rgba(124,196,160,${0.55 * glow})` }}>
              {LINK}
            </span>
          </div>
        </Terminal>
      </div>
      <Caption n={1} text="One command on a server" until={160} />
      <Caption n={2} text="The log prints the link that claims it" at={172} />
    </Scene>
  );
};
