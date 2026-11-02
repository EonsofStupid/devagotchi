import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { Caption, Cursor, HubBar, Scene, Status, Tap, Terminal, Window, appear, button, clamp, field, typed } from "../kit";
import { HUB, JOIN_CODE, c, font } from "../theme";

const JOIN = `curl -fsSL ${HUB}/join/${JOIN_CODE} | sudo sh`;

const Nav: React.FC = () => (
  <span style={{ display: "flex", alignItems: "center", gap: 16 }}>
    <u>Approvals</u><u>Machines</u><span style={button(false, { padding: "6px 12px" })}>Sign out</span>
  </span>
);

const H2: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <h2 style={{ font: `700 18px/1.3 ${font.ui}`, margin: "26px 0 10px" }}>{children}</h2>
);

const cell: React.CSSProperties = { padding: "10px 12px", borderBottom: `1px solid ${c.line}`, textAlign: "left" };

/** Step 5: name a machine, paste one line on it, and it is online. */
export const Machine: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const codePage = f >= 84 && f < 196;
  const listed = f >= 196;
  const term = interpolate(f, [116, 130, 186, 198], [0, 1, 1, 0], clamp);
  const joinDone = f >= 132 + (JOIN.length * 30) / 44;
  const pulse = listed ? 0.5 + 0.5 * Math.sin((f - 196) / 5) : 0;
  return (
    <Scene dur={dur}>
      <Window
        url={codePage ? "hub.example.com/machines/add" : "hub.example.com/account"}
        width={1000}
        height={540}
        style={{ position: "absolute", left: 140, top: 34 }}
      >
        <div style={{ position: "absolute", inset: 0, fontFamily: font.ui, color: c.text, fontSize: 17 }}>
          <HubBar right={<Nav />} />
          {!codePage ? (
            <div style={{ maxWidth: 728, margin: "0 auto", padding: "28px 20px", opacity: listed ? appear(f, 196, 10) : 1 }}>
              <h1 style={{ font: `700 28px/1.25 ${font.ui}`, margin: "0 0 6px" }}>Signed in as Jesse</h1>
              <p style={{ color: c.muted, margin: 0 }}>You own this hub: you reach every machine on it.</p>
              <H2>Your machines</H2>
              {listed ? (
                <table style={{ width: "100%", borderCollapse: "collapse", background: c.surface, border: `1px solid ${c.line}`,
                  borderRadius: 8, overflow: "hidden", fontSize: 16 }}>
                  <thead>
                    <tr style={{ color: c.muted, fontSize: 14 }}>
                      {["Machine", "Where", "Status", "Enrolled"].map((h) => <th key={h} style={{ ...cell, fontWeight: 600 }}>{h}</th>)}
                    </tr>
                  </thead>
                  <tbody>
                    <tr>
                      <td style={cell}>server</td>
                      <td style={cell}>Added by you</td>
                      <td style={{ ...cell, textShadow: `0 0 ${8 * pulse}px rgba(31,122,69,.6)` }}><Status online /></td>
                      <td style={cell}>2026-11-01 18:06 UTC</td>
                    </tr>
                  </tbody>
                </table>
              ) : (
                <p style={{ color: c.muted, margin: 0 }}>
                  No machines yet. Add one below; a machine the hub operator links for you shows up here too.
                </p>
              )}
              {!listed && (
                <>
                  <H2>Add a machine</H2>
                  <div style={{ color: c.muted, fontSize: 15 }}>Name it (lowercase letters, digits, dashes)</div>
                  <div style={{ ...field, maxWidth: 688 }}>{typed("server", f, 16, 10) || <span style={{ color: "#9aa79f" }}>platinum</span>}</div>
                  <div style={{ position: "relative", marginTop: 12 }}>
                    <div style={{ ...button(true, { display: "block", padding: 14, background: c.online, borderColor: c.online, color: "#fff" }) }}>
                      Get a one-time code
                    </div>
                    <Tap x={344} y={25} at={70} />
                  </div>
                </>
              )}
            </div>
          ) : (
            <div style={{ maxWidth: 728, margin: "0 auto", padding: "28px 20px", opacity: appear(f, 84, 10) }}>
              <h1 style={{ font: `700 28px/1.25 ${font.ui}`, margin: "0 0 10px" }}>Add server</h1>
              <p style={{ margin: "0 0 4px", lineHeight: 1.5 }}>
                Paste one line on that machine. It installs Devagotchi, gives the machine its own key, joins it as <b>server</b> and
                starts it, now and after every restart. The line works once, until 2026-11-01 18:19 UTC; this page is the only
                place it is shown.
              </p>
              <H2>Linux</H2>
              <div style={{ padding: 12, borderRadius: 6, background: c.bg, border: `1px solid ${c.line}`, font: `15px/1.5 ${font.mono}`,
                wordBreak: "break-all" }}>{JOIN}</div>
              <H2>Windows</H2>
              <div style={{ color: c.muted, fontSize: 15 }}>In PowerShell (no administrator needed):</div>
            </div>
          )}
        </div>
      </Window>
      <div style={{ position: "absolute", left: 520, top: 320, opacity: term, transform: `translateY(${(1 - term) * 30}px)` }}>
        <Terminal title="server" width={700} height={170}>
          <span style={{ color: c.onDeepMuted }}>~ $ </span>
          {typed(JOIN, f, 132, 44)}
          {!joinDone && <Cursor />}
        </Terminal>
      </div>
      <Caption n={5} text="Add a machine: one line on it, and it is online" />
    </Scene>
  );
};
