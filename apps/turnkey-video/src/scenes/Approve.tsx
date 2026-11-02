import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { Caption, HubBar, Mark, Phone, Scene, Tap, appear, clamp, typed } from "../kit";
import { c, font } from "../theme";
import { AddressBar, SignedInNav } from "./Claim";

const TITLE = "Restart caddy on server";
const DESCRIPTION =
  "What: systemctl restart caddy\n\nWhy: it is still serving the old site config\n\nUndo: restore the old Caddyfile and restart caddy again";

const Bubble: React.FC<{ me?: boolean; at: number; children: React.ReactNode }> = ({ me, at, children }) => {
  const f = useCurrentFrame();
  const shown = appear(f, at, 10);
  if (f < at) return null;
  return (
    <div style={{ alignSelf: me ? "flex-end" : "flex-start", maxWidth: 470, margin: "0 0 14px", padding: me ? "12px 16px" : "2px 0",
      borderRadius: 16, background: me ? "#efefec" : "transparent", opacity: shown, transform: `translateY(${(1 - shown) * 8}px)`,
      lineHeight: 1.45 }}>{children}</div>
  );
};

const ToolChip: React.FC<{ at: number; name: string; text: string; done?: boolean }> = ({ at, name, text, done }) => {
  const f = useCurrentFrame();
  if (f < at) return null;
  return (
    <div style={{ alignSelf: "flex-start", display: "flex", gap: 10, alignItems: "center", margin: "0 0 14px", padding: "8px 12px",
      borderRadius: 10, border: `1px solid ${c.appLine}`, background: "#fff", fontSize: 14, opacity: appear(f, at, 8) }}>
      <span style={{ fontFamily: font.mono, color: c.accent, fontWeight: 700 }}>{name}</span>
      <span style={{ color: c.appMuted }}>{text}</span>
      {done !== undefined && <span style={{ color: done ? c.online : c.wait, fontWeight: 700 }}>{done ? "✓" : "…"}</span>}
    </div>
  );
};

/** Step 6: the AI asks before it restarts a service; the owner approves on the phone. */
export const Approve: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const decided = f >= 206;
  const note = interpolate(f, [70, 84], [0, 1], clamp);
  const page = f >= 128;
  return (
    <Scene dur={dur}>
      <div style={{ position: "absolute", left: 64, top: 34, width: 760, height: 540, borderRadius: 14, background: c.app,
        border: `1px solid ${c.appLine}`, boxShadow: "0 40px 80px -40px rgba(12,40,26,.4)", fontFamily: font.ui, color: c.appText,
        fontSize: 17, display: "flex", flexDirection: "column", padding: "28px 32px", overflow: "hidden" }}>
        <Bubble me at={4}>caddy on server is still serving the old site. Can you restart it?</Bubble>
        <Bubble at={24}>{typed("Restarting caddy drops its open connections for a moment, so I'll ask you first.", f, 24, 60)}</Bubble>
        <ToolChip at={52} name="approval_request" text={TITLE} />
        <ToolChip at={60} name="approval_wait" text={decided ? "approved" : "waiting for your answer"} done={decided} />
        <ToolChip at={222} name="run" text="systemctl restart caddy" done={f >= 240} />
        <Bubble at={246}>{typed("Approved and done: caddy restarted on server and is serving the new site.", f, 246, 70)}</Bubble>
      </div>
      <Phone style={{ position: "absolute", left: 880, top: 22 }}>
        {!page ? (
          <div style={{ position: "absolute", inset: 0, background: `linear-gradient(180deg, ${c.deep}, #0d1f16)`, color: c.onDeep }}>
            <div style={{ textAlign: "center", marginTop: 90, font: `300 84px/1 ${font.ui}` }}>18:04</div>
            <div style={{ textAlign: "center", color: c.onDeepMuted, fontSize: 18, marginTop: 8 }}>Sunday, 1 November</div>
            <div style={{ position: "relative", margin: "60px 16px 0", padding: "14px 16px", borderRadius: 22,
              background: "rgba(240,246,242,.92)", color: c.text, opacity: note, transform: `translateY(${(1 - note) * -20}px)` }}>
              <div style={{ display: "flex", alignItems: "center", gap: 8, color: c.muted, fontSize: 14 }}>
                <Mark size={20} />Devagotchi<span style={{ marginLeft: "auto" }}>now</span>
              </div>
              <div style={{ fontWeight: 700, marginTop: 6 }}>Approval needed</div>
              <div>{TITLE}</div>
              <Tap x={150} y={50} at={118} />
            </div>
          </div>
        ) : (
          <div style={{ opacity: appear(f, 128, 10) }}>
            <AddressBar host="hub.example.com" />
            <HubBar compact markOnly right={<SignedInNav />} />
            <div style={{ padding: "22px 20px" }}>
              <h1 style={{ font: `700 26px/1.25 ${font.ui}`, margin: "0 0 12px" }}>Approvals</h1>
              {decided ? (
                <p style={{ color: c.muted, opacity: appear(f, 206, 10) }}>Decided home-3fa9. The agent continues on its own.</p>
              ) : (
                <div style={{ background: c.surface, border: `1px solid ${c.line}`, borderRadius: 8, padding: 16 }}>
                  <div style={{ font: `700 18px/1.3 ${font.ui}` }}>{TITLE}</div>
                  <div style={{ color: c.muted, fontSize: 14, marginTop: 4 }}>Your AI asks · 2026-11-01 18:04 UTC · home</div>
                  <div style={{ whiteSpace: "pre-wrap", font: `14px/1.45 ${font.mono}`, background: c.bg, padding: 12, borderRadius: 6,
                    margin: "12px 0" }}>{DESCRIPTION}</div>
                  <div style={{ color: c.muted, fontSize: 14 }}>Note (optional)</div>
                  <div style={{ height: 40, border: `1px solid ${c.line}`, borderRadius: 6, background: c.bg, marginTop: 4 }} />
                  <div style={{ position: "relative", display: "flex", gap: 12, marginTop: 12 }}>
                    <div style={{ flex: 1, padding: 14, borderRadius: 6, background: c.online, color: "#fff", fontWeight: 600, textAlign: "center" }}>
                      Approve
                    </div>
                    <div style={{ flex: 1, padding: 14, borderRadius: 6, border: `1px solid ${c.revoked}`, color: c.revoked,
                      fontWeight: 600, textAlign: "center" }}>Deny</div>
                    <Tap x={66} y={25} at={196} />
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </Phone>
      <Caption n={6} text="Your AI asks first. You approve on your phone." />
    </Scene>
  );
};
