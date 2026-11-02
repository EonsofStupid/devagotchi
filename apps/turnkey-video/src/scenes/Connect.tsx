import React from "react";
import { useCurrentFrame } from "remotion";
import { Caption, HubBar, Scene, Tap, Window, appear, button, typed } from "../kit";
import { HUB, c, font } from "../theme";

const appField: React.CSSProperties = {
  marginTop: 6, padding: "11px 12px", border: `1px solid ${c.appLine}`, borderRadius: 10, background: "#fff",
  font: `16px/1.3 ${font.ui}`, color: c.appText, minHeight: 44, boxSizing: "border-box",
};

/** Step 4: Claude adds the hub as a custom connector, the hub asks the owner, connected. */
export const Connect: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const consent = f >= 118 && f < 214;
  const done = f >= 214;
  const fadeIn = (at: number) => appear(f, at, 10);
  return (
    <Scene dur={dur}>
      <Window
        url={consent ? "hub.example.com/oauth/authorize" : "Claude · Settings · Connectors"}
        width={1000}
        height={540}
        style={{ position: "absolute", left: 140, top: 34 }}
      >
        {!consent && (
          <div style={{ position: "absolute", inset: 0, background: c.app, fontFamily: font.ui, color: c.appText }}>
            <div style={{ padding: "34px 44px" }}>
              <div style={{ font: `700 24px/1.2 ${font.ui}` }}>Connectors</div>
              {done && (
                <div style={{ marginTop: 26, display: "flex", alignItems: "center", gap: 16, padding: "16px 18px", borderRadius: 12,
                  border: `1px solid ${c.appLine}`, background: "#fff", opacity: fadeIn(220), maxWidth: 620 }}>
                  <div style={{ width: 40, height: 40, borderRadius: 10, background: c.accent, display: "grid", placeItems: "center",
                    color: "#fff", fontWeight: 700 }}>D</div>
                  <div style={{ flex: 1 }}>
                    <div style={{ fontWeight: 700 }}>Devagotchi</div>
                    <div style={{ color: c.appMuted, fontSize: 14 }}>Custom connector · {HUB.replace("https://", "")}</div>
                  </div>
                  <div style={{ color: c.online, fontWeight: 700 }}>● Connected</div>
                </div>
              )}
            </div>
            {!done && (
              <>
                <div style={{ position: "absolute", inset: 0, background: "rgba(20,20,18,.32)" }} />
                <div style={{ position: "absolute", left: 230, top: 64, width: 540, padding: "26px 28px", borderRadius: 16,
                  background: c.app, boxShadow: "0 30px 60px -20px rgba(0,0,0,.4)" }}>
                  <div style={{ font: `700 21px/1.2 ${font.ui}` }}>Add custom connector</div>
                  <div style={{ marginTop: 18, fontSize: 14, color: c.appMuted }}>Name</div>
                  <div style={appField}>{typed("Devagotchi", f, 12, 16)}</div>
                  <div style={{ marginTop: 14, fontSize: 14, color: c.appMuted }}>Remote MCP server URL</div>
                  <div style={{ ...appField, fontFamily: font.mono, fontSize: 15 }}>{typed(`${HUB}/mcp`, f, 36, 34)}</div>
                  <div style={{ marginTop: 14, fontSize: 13, color: c.appMuted }}>OAuth Client ID and Secret: left empty</div>
                  <div style={{ position: "relative", marginTop: 20, display: "flex", justifyContent: "flex-end" }}>
                    <div style={{ padding: "11px 22px", borderRadius: 10, background: c.appText, color: "#fff", fontWeight: 600 }}>Add</div>
                    <Tap x={474} y={22} at={100} color="#555" />
                  </div>
                </div>
              </>
            )}
          </div>
        )}
        {consent && (
          <div style={{ position: "absolute", inset: 0, background: c.bg, opacity: fadeIn(118), fontFamily: font.ui, color: c.text }}>
            <HubBar
              right={
                <span style={{ display: "flex", alignItems: "center", gap: 16 }}>
                  <u>Approvals</u><u>Machines</u><span style={button(false, { padding: "6px 12px" })}>Sign out</span>
                </span>
              }
            />
            <div style={{ maxWidth: 728, margin: "0 auto", padding: "40px 20px", fontSize: 18 }}>
              <h1 style={{ font: `700 30px/1.25 ${font.ui}`, margin: "0 0 14px" }}>Connect Claude?</h1>
              <p style={{ margin: "0 0 12px", lineHeight: 1.5 }}>
                <b>Claude</b> asks to use this hub as <b>Jesse</b>: every tool on the machines you reach, until you disconnect it or it
                goes unused for 7 days.
              </p>
              <p style={{ color: c.muted, margin: "0 0 22px" }}>It sends you back to https://claude.ai.</p>
              <div style={{ position: "relative", display: "flex", gap: 10 }}>
                <span style={button(true)}>Connect</span>
                <span style={button(false)}>Cancel</span>
                <Tap x={50} y={22} at={192} />
              </div>
            </div>
          </div>
        )}
      </Window>
      <Caption n={4} text="Add it to Claude as a custom connector, and approve it" />
    </Scene>
  );
};
