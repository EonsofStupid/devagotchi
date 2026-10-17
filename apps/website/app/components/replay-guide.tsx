"use client";

import { useEffect, useMemo, useState } from "react";
import {
  Button,
  Dialog,
  DialogTrigger,
  Modal,
  ModalOverlay,
  Popover,
  Tab,
  TabList,
  TabPanel,
  Tabs,
} from "react-aria-components";

type ReplayStep = {
  title: string;
  body: string;
  action: string;
  path: string;
  icon: string;
};

type ReplayTrack = {
  id: string;
  title: string;
  short: string;
  duration: string;
  source?: string;
  steps: ReplayStep[];
};

const TRACKS: ReplayTrack[] = [
  {
    id: "join",
    title: "Join Devagotchi",
    short: "ChatGPT sign-in and first-run choices",
    duration: "45 sec",
    steps: [
      { title: "Start with ChatGPT", body: "Choose Sign in with ChatGPT. The platform completes identity verification; Devagotchi never handles your ChatGPT password.", action: "Sign in with ChatGPT", path: "devagotchi.com", icon: "↗" },
      { title: "Enter your sanctuary", body: "After sign-in, your private sanctuary opens with Clyffy.AI as the starter primary companion.", action: "Open sanctuary", path: "devagotchi.com/sanctuary", icon: "⌂" },
      { title: "Choose reminder preferences", body: "Allow gentle browser reminders or choose Maybe later. The device preference lasts 30 days and does not replace ChatGPT authentication.", action: "Allow reminders", path: "devagotchi.com/sanctuary", icon: "♬" },
    ],
  },
  {
    id: "manage",
    title: "Manage your sanctuary",
    short: "Upload, select, care, and level up",
    duration: "70 sec",
    steps: [
      { title: "Add a companion", body: "Choose Add a companion, upload a transparent GIF, PNG, or WebP, then give the pet a name and species.", action: "+ Add a companion", path: "devagotchi.com/sanctuary", icon: "+" },
      { title: "Pick exactly one primary", body: "Use Make primary in the gallery. Your former primary stays safe in the collection while the new one moves into the habitat.", action: "Make primary", path: "devagotchi.com/sanctuary#collection", icon: "★" },
      { title: "Care and progress", body: "Focus earns the most XP, treats restore energy, and play strengthens bond. Daily activity grows the streak.", action: "Focus together", path: "devagotchi.com/sanctuary", icon: "◎" },
      { title: "Read the progress signals", body: "Level, XP, bond, energy, focus, and streak all update from account-owned game state—not browser-only storage.", action: "View progress", path: "devagotchi.com/sanctuary", icon: "↗" },
    ],
  },
  {
    id: "plugin",
    title: "Bring Devagotchi to ChatGPT",
    short: "Current plugin connection and publishing flow",
    duration: "2 min",
    source: "https://developers.openai.com/plugins/quickstart",
    steps: [
      { title: "Prepare the MCP endpoint", body: "Deploy a public HTTPS MCP endpoint using streamable HTTP, normally at /mcp. Secure MCP Tunnel is suitable for private developer testing.", action: "Verify https://…/mcp", path: "your-server.example/mcp", icon: "⇄" },
      { title: "Enable Developer mode", body: "In ChatGPT, open Settings, select Security and login, and turn on Developer mode. Availability can depend on workspace policy.", action: "Turn on Developer mode", path: "chatgpt.com/settings", icon: "⚙" },
      { title: "Create the plugin connection", body: "Open ChatGPT Plugins, select the plus button, add the user-facing name and description, then enter the MCP server URL under Connection.", action: "+ New plugin", path: "chatgpt.com/plugins", icon: "+" },
      { title: "Review and test tools", body: "Confirm discovered tools and metadata, then test direct, indirect, follow-up, confirmation, and unsupported prompts in a fresh ChatGPT Work conversation.", action: "Run test prompts", path: "chatgpt.com/work", icon: "✓" },
      { title: "Prepare public submission", body: "The publisher needs Apps Management: Write, a verified developer or business identity, listing and policy URLs, accurate tool annotations, starter prompts, five positive tests, and three negative tests.", action: "Open submission portal", path: "platform.openai.com", icon: "↑" },
    ],
  },
];

export function ReplayGuide({ compact = false }: { compact?: boolean }) {
  const [replayOpen, setReplayOpen] = useState(false);
  const [trackId, setTrackId] = useState("join");
  const [stepIndex, setStepIndex] = useState(0);
  const [playing, setPlaying] = useState(false);
  const track = useMemo(() => TRACKS.find((item) => item.id === trackId) ?? TRACKS[0], [trackId]);
  const step = track.steps[stepIndex];

  useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => {
      setStepIndex((current) => {
        if (current >= track.steps.length - 1) {
          setPlaying(false);
          return current;
        }
        return current + 1;
      });
    }, 2800);
    return () => window.clearInterval(timer);
  }, [playing, track.steps.length]);

  function startReplay(id: string) {
    setTrackId(id);
    setStepIndex(0);
    setPlaying(true);
    setReplayOpen(true);
  }

  function selectTrack(key: React.Key) {
    setTrackId(String(key));
    setStepIndex(0);
    setPlaying(false);
  }

  return (
    <>
      <DialogTrigger>
        <Button className={`replay-trigger${compact ? " compact" : ""}`}>Replay guide</Button>
        <Popover className="guide-popover" placement="bottom end">
          <Dialog className="guide-launcher">
            {({ close }) => (
              <>
                <div className="guide-launcher-header"><div><p>GUIDED REPLAYS</p><h2>Show me exactly how</h2></div><span>3 TRACKS</span></div>
                <div className="guide-card-list">
                  {TRACKS.map((item, index) => (
                    <Button className="guide-card" key={item.id} onPress={() => { close(); startReplay(item.id); }}>
                      <span className="guide-card-icon">{index === 0 ? "↗" : index === 1 ? "★" : "⇄"}</span>
                      <span><b>{item.title}</b><small>{item.short} • {item.duration}</small></span><span>›</span>
                    </Button>
                  ))}
                </div>
                <small className="guide-source-note">Guided simulations—not recordings of private user sessions. Keyboard, touch, focus trapping, and dismissal are handled by React Aria.</small>
              </>
            )}
          </Dialog>
        </Popover>
      </DialogTrigger>

      <ModalOverlay className="replay-overlay" isOpen={replayOpen} onOpenChange={setReplayOpen} isDismissable>
        <Modal className="replay-modal">
          <Dialog className="replay-dialog" aria-label="Devagotchi guided replay">
            {({ close }) => (
              <>
                <header className="replay-topbar"><div className="replay-brandline"><span>D</span><div><p>SESSION REPLAY CENTER</p><h2>Learn Devagotchi by watching it happen</h2></div></div><Button className="replay-close" onPress={close} aria-label="Close replay guide">×</Button></header>
                <Tabs className="replay-layout" selectedKey={trackId} onSelectionChange={selectTrack} orientation="vertical">
                  <aside className="replay-sidebar"><TabList className="replay-tabs" aria-label="Replay tracks">{TRACKS.map((item) => <Tab className="replay-tab" id={item.id} key={item.id}><b>{item.title}</b><small>{item.duration}</small></Tab>)}</TabList></aside>
                  {TRACKS.map((item) => (
                    <TabPanel className="replay-panel" id={item.id} key={item.id}>
                      <div className="replay-panel-wrap">
                        <div className="replay-meta"><div><p>GUIDED SIMULATION</p><h3>{item.title}</h3></div><span className="session-status"><i /> SAFE REPLAY • NO PRIVATE DATA</span></div>
                        <div className="replay-canvas" aria-live="polite">
                          <div className="replay-browser-bar"><i /><i /><i /><span>{step.path}</span></div>
                          <div className="replay-screen"><div className="replay-mock-card"><span className="replay-step-icon">{step.icon}</span><div><p>STEP {stepIndex + 1} OF {track.steps.length}</p><h4>{step.title}</h4><span>{step.body}</span></div><span className="replay-action-mock">{step.action}</span></div></div>
                          <span className="replay-cursor" aria-hidden="true" /><div className="replay-coachmark"><span>NEXT ACTION</span><b>{step.action}</b></div>
                        </div>
                        <div className="replay-progress-area"><div className="replay-progress-track" aria-hidden="true"><i style={{ width: `${((stepIndex + 1) / track.steps.length) * 100}%` }} /></div><span>{stepIndex + 1} / {track.steps.length}</span></div>
                        <div className="replay-step-copy"><div><p>CURRENT STEP</p><h4>{step.title}</h4><span>{step.body}</span>{item.source && <><br /><a className="replay-official-link" href={item.source} target="_blank" rel="noreferrer">Open the official OpenAI guide ↗</a></>}</div><div className="replay-controls"><Button className="replay-control" isDisabled={stepIndex === 0} onPress={() => { setPlaying(false); setStepIndex((value) => Math.max(0, value - 1)); }} aria-label="Previous step">←</Button><Button className="replay-control primary" onPress={() => setPlaying((value) => !value)}>{playing ? "Pause" : "Play replay"}</Button><Button className="replay-control" isDisabled={stepIndex === track.steps.length - 1} onPress={() => { setPlaying(false); setStepIndex((value) => Math.min(track.steps.length - 1, value + 1)); }} aria-label="Next step">→</Button></div></div>
                      </div>
                    </TabPanel>
                  ))}
                </Tabs>
              </>
            )}
          </Dialog>
        </Modal>
      </ModalOverlay>
    </>
  );
}
