import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { Caption, HubBar, Phone, Scene, Tap, appear, button, clamp, field, label, typed } from "../kit";
import { CLAIM_CODE, c, font } from "../theme";

/** The phone browser's address bar. */
export const AddressBar: React.FC<{ host: string }> = ({ host }) => (
  <div style={{ height: 48, display: "flex", alignItems: "center", justifyContent: "center", background: c.app,
    borderBottom: `1px solid ${c.appLine}` }}>
    <div style={{ padding: "6px 18px", borderRadius: 10, background: "#efefec", color: c.appMuted, font: `15px/1 ${font.ui}` }}>{host}</div>
  </div>
);

export const SignedInNav: React.FC = () => (
  <span style={{ display: "flex", alignItems: "center", gap: 12, fontSize: 15 }}>
    <u>Approvals</u><u>Machines</u>
    <span style={{ ...button(false, { padding: "6px 12px", fontSize: 15 }) }}>Sign out</span>
  </span>
);

const H1: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <h1 style={{ font: `700 26px/1.25 ${font.ui}`, margin: "0 0 8px" }}>{children}</h1>
);

const Side: React.FC<{ opacity: number; top: number; children: React.ReactNode }> = ({ opacity, top, children }) => (
  <div style={{ position: "absolute", left: 96, top, width: 640, opacity, transform: `translateY(${(1 - opacity) * 14}px)` }}>{children}</div>
);

/** Step 3: the link opens on the phone; a name, a passkey, and the owner is signed in. */
export const Claim: React.FC<{ dur: number }> = ({ dur }) => {
  const f = useCurrentFrame();
  const sheet = interpolate(f, [92, 108, 156, 170], [0, 1, 1, 0], clamp);
  const signedIn = f >= 172;
  const page2 = appear(f, 172, 10);
  const link = Math.min(appear(f, 4, 12), 1 - appear(f, 84, 10));
  return (
    <Scene dur={dur}>
      <Side opacity={link} top={200}>
        <div style={{ color: c.muted, font: `600 20px/1.3 ${font.ui}`, marginBottom: 12 }}>The link from the log</div>
        <div style={{ padding: "14px 18px", borderRadius: 12, background: c.deep, color: "#7cc4a0", font: `700 19px/1.3 ${font.mono}` }}>
          hub.example.com/claim#{CLAIM_CODE}
        </div>
        <div style={{ marginTop: 16, color: c.muted, font: `20px/1.45 ${font.ui}` }}>Opened on the phone that will keep your passkey.</div>
      </Side>
      <Side opacity={Math.min(appear(f, 96, 12), 1 - appear(f, 166, 8))} top={230}>
        <div style={{ font: `700 40px/1.08 ${font.display}`, letterSpacing: "-0.015em" }}>Your passkey, made on your phone.</div>
        <div style={{ marginTop: 14, color: c.muted, font: `22px/1.45 ${font.ui}` }}>
          Your fingerprint, face or screen lock unlocks it. There is no password.
        </div>
      </Side>
      <Side opacity={appear(f, 182, 14)} top={230}>
        <div style={{ font: `700 40px/1.08 ${font.display}`, letterSpacing: "-0.015em" }}>You own this hub.</div>
        <div style={{ marginTop: 14, color: c.muted, font: `22px/1.45 ${font.ui}` }}>
          The code is gone for good. From now on you sign in with your passkey.
        </div>
      </Side>
      <Phone style={{ position: "absolute", left: 860, top: 22 }}>
        <AddressBar host="hub.example.com" />
        {!signedIn ? (
          <>
            <HubBar compact />
            <div style={{ padding: "26px 20px" }}>
              <H1>Claim this hub</H1>
              <p style={{ margin: "0 0 12px" }}>
                This hub has no owner yet. Whoever claims it owns it: they reach every machine on it and approve every AI
                that connects to it.
              </p>
              <label style={label}>
                Claim code
                <div style={{ ...field, fontFamily: font.mono, fontSize: 13 }}>{CLAIM_CODE}</div>
              </label>
              <label style={label}>
                Your name
                <div style={field}>{typed("Jesse", f, 40, 9) || <span style={{ color: "#9aa79f" }}>Owner</span>}</div>
              </label>
              <div style={{ position: "relative", marginTop: 18 }}>
                <div style={{ ...button(true, { display: "block", padding: 14 }) }}>Claim this hub and make my passkey</div>
                <Tap x={150} y={25} at={86} />
              </div>
              <p style={{ color: c.muted, fontSize: 15, margin: "16px 0 0" }}>
                Your passkey is how you sign in from now on: your phone's or computer's fingerprint, face or PIN, and your
                passkey manager can share it with your other devices.
              </p>
            </div>
            <div style={{ position: "absolute", inset: 0, background: `rgba(0,0,0,${0.35 * sheet})` }} />
            <div style={{ position: "absolute", left: 0, right: 0, bottom: 0, height: 330, background: c.app,
              borderRadius: "24px 24px 0 0", transform: `translateY(${(1 - sheet) * 340}px)`, padding: "26px 28px",
              fontFamily: font.ui, color: c.appText, textAlign: "center" }}>
              <div style={{ width: 44, height: 5, borderRadius: 3, background: "#d4d4cf", margin: "-12px auto 20px" }} />
              <svg width="56" height="56" viewBox="0 0 24 24" fill="none" stroke={c.appText} strokeWidth="1.4" strokeLinecap="round">
                <path d="M12 3a7 7 0 0 0-7 7v3M19 10a7 7 0 0 0-11.9-5" />
                <path d="M9 21c.8-1.6 1-3.4 1-5.2V10a2 2 0 0 1 4 0v4" />
                <path d="M16 13.5c0 2.6-.4 5-1.5 7.5M6 17c.6-1 1-2.4 1-4v-3" />
              </svg>
              <div style={{ font: `700 21px/1.3 ${font.ui}`, margin: "12px 0 6px" }}>Create a passkey for hub.example.com</div>
              <div style={{ color: c.appMuted, fontSize: 16 }}>Jesse</div>
              <div style={{ position: "relative", marginTop: 28 }}>
                <div style={{ padding: 14, borderRadius: 12, background: c.appText, color: "#fff", fontWeight: 600 }}>Continue</div>
                <Tap x={142} y={25} at={146} color="#ffffff" />
              </div>
            </div>
          </>
        ) : (
          <div style={{ opacity: page2 }}>
            <HubBar compact markOnly right={<SignedInNav />} />
            <div style={{ padding: "26px 20px" }}>
              <H1>Signed in as Jesse</H1>
              <p style={{ color: c.muted, margin: "0 0 8px" }}>
                Account ID <span style={{ fontFamily: font.mono, fontSize: 14 }}>acct_7h2kq9w4mx3bz8tr5vn6pd1c</span>
              </p>
              <p style={{ color: c.muted, margin: "0 0 8px" }}>You own this hub: you reach every machine on it.</p>
              <h2 style={{ font: `700 18px/1.3 ${font.ui}`, margin: "28px 0 12px" }}>Your machines</h2>
              <p style={{ color: c.muted, margin: 0 }}>
                No machines yet. Add one below; a machine the hub operator links for you shows up here too.
              </p>
              <h2 style={{ font: `700 18px/1.3 ${font.ui}`, margin: "28px 0 12px" }}>Add a machine</h2>
            </div>
          </div>
        )}
      </Phone>
      <Caption n={3} text="Claim it from your phone and make your passkey" />
    </Scene>
  );
};
