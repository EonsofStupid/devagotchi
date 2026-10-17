"use client";

import Link from "next/link";
import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { ReplayGuide } from "@/app/components/replay-guide";

type Pet = { id: string; name: string; species: string; assetUrl: string | null; isPrimary: boolean; level: number; xp: number; bond: number; energy: number; focus: number; streak: number; createdAt: number };

export function SanctuaryClient({ displayName, email, signOutHref }: { displayName: string; email: string; signOutHref: string }) {
  const [pets, setPets] = useState<Pet[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [uploadOpen, setUploadOpen] = useState(false);
  const [noticeOpen, setNoticeOpen] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const primary = useMemo(() => pets.find((pet) => pet.isPrimary) ?? pets[0], [pets]);

  async function refresh() {
    const response = await fetch("/api/pets", { cache: "no-store" });
    if (response.ok) setPets(await response.json());
    setLoading(false);
  }

  useEffect(() => {
    void refresh();
    setNoticeOpen(localStorage.getItem("devagotchi-welcome") !== "done");
  }, []);

  function message(text: string) {
    setToast(text);
    window.setTimeout(() => setToast(null), 2600);
  }

  async function care(kind: "focus" | "treat" | "play") {
    if (!primary) return;
    setBusy(kind);
    const response = await fetch(`/api/pets/${primary.id}/care`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ kind }) });
    if (response.ok) {
      const updated: Pet = await response.json();
      setPets((current) => current.map((pet) => pet.id === updated.id ? updated : pet));
      message(kind === "focus" ? "+18 XP — deep work logged" : kind === "treat" ? "Energy restored" : "Bond strengthened");
    }
    setBusy(null);
  }

  async function makePrimary(id: string) {
    setBusy(id);
    const response = await fetch(`/api/pets/${id}/primary`, { method: "POST" });
    if (response.ok) {
      setPets((current) => current.map((pet) => ({ ...pet, isPrimary: pet.id === id })));
      message("Your primary Devagotchi has changed");
    }
    setBusy(null);
  }

  async function upload(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy("upload");
    const response = await fetch("/api/pets", { method: "POST", body: form });
    if (response.ok) {
      await refresh();
      setUploadOpen(false);
      message("New companion joined your gallery");
      event.currentTarget.reset();
    } else {
      const error: unknown = await response.json().catch(() => null);
      message(error && typeof error === "object" && "error" in error && typeof error.error === "string" ? error.error : "Upload failed");
    }
    setBusy(null);
  }

  async function finishWelcome(enableNotifications: boolean) {
    let notificationsEnabled = false;
    if (enableNotifications && "Notification" in window) {
      const permission = await Notification.requestPermission();
      notificationsEnabled = permission === "granted";
      if (notificationsEnabled && "serviceWorker" in navigator) {
        const registration = await navigator.serviceWorker.register("/sw.js");
        await registration.showNotification("Your Devagotchi is awake!", { body: "Clyffy.AI is ready for your next creative session.", tag: "devagotchi-welcome" });
      }
    }
    await fetch("/api/preferences", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ notificationsEnabled, rememberDevice: true }) });
    localStorage.setItem("devagotchi-welcome", "done");
    setNoticeOpen(false);
    message(notificationsEnabled ? "Gentle reminders are on" : "Device remembered for 30 days");
  }

  return (
    <main className="sanctuary-shell">
      <nav className="sanctuary-nav">
        <Link className="brand" href="/"><span className="brand-mark">D</span><span>DEVAGOTCHI</span></Link>
        <div className="sanctuary-tabs"><a href="#home" className="active">Sanctuary</a><a href="#collection">Gallery</a></div>
        <div className="sanctuary-nav-actions"><ReplayGuide compact /><details className="account-menu"><summary><span>{displayName.slice(0, 1).toUpperCase()}</span>{displayName}</summary><div><small>{email}</small><a href={signOutHref}>Sign out</a></div></details></div>
      </nav>

      <div className="sanctuary-body" id="home">
        <header className="dashboard-heading"><div><p>YOUR SANCTUARY</p><h1>Good to see you, {displayName.split(" ")[0]}.</h1><span>Your companion kept the habitat warm.</span></div><button onClick={() => setUploadOpen(true)}>＋ Add a companion</button></header>

        {loading ? <div className="dashboard-loading">Waking your Devagotchi…</div> : primary ? (
          <section className="primary-dashboard">
            <article className="primary-habitat">
              <div className="habitat-sky"><span className="habitat-badge">PRIMARY • LV. {primary.level}</span><div className="habitat-sun" /><div className="habitat-platform" /><img src={primary.assetUrl ?? "/clyffy-showcase.gif"} alt={`${primary.name}, your primary Devagotchi`} /></div>
              <div className="primary-title"><div><p>{primary.species}</p><h2>{primary.name}</h2></div><div className="streak-pill">🔥 {primary.streak} day streak</div></div>
            </article>
            <aside className="care-panel">
              <div><p>DAILY CARE</p><h2>What are we doing next?</h2></div>
              <div className="dashboard-stats">
                {[['FOCUS', primary.focus], ['BOND', primary.bond], ['ENERGY', primary.energy]].map(([label, value]) => <div key={label}><span>{label}</span><b>{value}</b><i><em style={{ width: `${value}%` }} /></i></div>)}
              </div>
              <div className="care-choice-grid">
                <button disabled={busy !== null} onClick={() => care("focus")}><i>◎</i><span><b>Focus together</b><small>Log a creative session • +18 XP</small></span></button>
                <button disabled={busy !== null} onClick={() => care("treat")}><i>◇</i><span><b>Give a treat</b><small>Restore energy • +6 XP</small></span></button>
                <button disabled={busy !== null} onClick={() => care("play")}><i>✦</i><span><b>Take a play break</b><small>Strengthen your bond • +10 XP</small></span></button>
              </div>
              <div className="xp-progress"><span><b>{primary.xp % 100} XP</b> to level {primary.level + 1}</span><i><em style={{ width: `${primary.xp % 100}%` }} /></i></div>
            </aside>
          </section>
        ) : null}

        <section className="collection-section" id="collection">
          <div className="collection-heading"><div><p>YOUR GALLERY</p><h2>Companions you have collected</h2></div><span>{pets.length} / ∞ pets</span></div>
          <div className="collection-grid">
            {pets.map((pet) => <article className={pet.isPrimary ? "collection-card selected" : "collection-card"} key={pet.id}>
              <div className="collection-art"><img src={pet.assetUrl ?? "/clyffy-showcase.gif"} alt={pet.name} />{pet.isPrimary && <span>PRIMARY</span>}</div>
              <div><p>LV. {pet.level} • {pet.species}</p><h3>{pet.name}</h3>{!pet.isPrimary && <button disabled={busy !== null} onClick={() => makePrimary(pet.id)}>Make primary</button>}</div>
            </article>)}
            <button className="add-pet-card" onClick={() => setUploadOpen(true)}><span>＋</span><b>Add a companion</b><small>GIF, PNG, or WebP</small></button>
          </div>
        </section>
      </div>

      {uploadOpen && <div className="modal-backdrop" role="presentation" onMouseDown={(e) => e.target === e.currentTarget && setUploadOpen(false)}><section className="modal-card" role="dialog" aria-modal="true" aria-labelledby="upload-title"><button className="modal-close" onClick={() => setUploadOpen(false)} aria-label="Close">×</button><p>NEW COMPANION</p><h2 id="upload-title">Who is joining the gallery?</h2><form onSubmit={upload}><label className="file-drop"><input ref={fileRef} type="file" name="file" accept="image/png,image/webp,image/gif" required onChange={(event) => { const label = event.currentTarget.nextElementSibling; if (label) label.textContent = event.currentTarget.files?.[0]?.name ?? "Choose pet artwork"; }} /><span>Choose pet artwork</span><small>Transparent GIF, PNG, or WebP • 8 MB max</small></label><label>Name<input name="name" required maxLength={40} placeholder="Mochi Byte" /></label><label>Species<input name="species" required maxLength={40} placeholder="Cloud fox" /></label><label className="check-row"><input type="checkbox" name="primary" value="true" /><span><b>Make this my primary Devagotchi</b><small>Your current primary will move safely to the gallery.</small></span></label><button className="modal-submit" disabled={busy === "upload"}>{busy === "upload" ? "Welcoming…" : "Welcome companion"}</button></form></section></div>}

      {noticeOpen && <aside className="welcome-card"><button onClick={() => finishWelcome(false)} aria-label="Dismiss">×</button><span className="welcome-bell">♬</span><div><p>ONE LAST THING</p><h2>Want gentle care reminders?</h2><span>We will only nudge when your primary Devagotchi needs you. This device is remembered for 30 days.</span></div><div className="welcome-actions"><button onClick={() => finishWelcome(true)}>Allow reminders</button><button onClick={() => finishWelcome(false)}>Maybe later</button></div></aside>}
      {toast && <div className="toast" role="status">✦ {toast}</div>}
    </main>
  );
}
