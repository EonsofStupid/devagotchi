import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { describeError, type BridgeClient } from "./bridge";
import type { BridgeError, OperationState, OperationSummary, OperationView } from "./generated/contracts";
import { isActive, useInspector, type Inspector } from "./useInspector";

type View = "activity" | "workspaces" | "connection";
type IconName = "activity" | "folder" | "connection" | "refresh" | "search" | "arrow" | "copy" | "stop" | "terminal" | "clock" | "check" | "chevron" | "computer" | "info" | "close";

function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  const paths: Record<IconName, ReactNode> = {
    activity: <path d="M3 12h4l3-7 4 14 3-7h4" />,
    folder: <path d="M3 7a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />,
    connection: <><path d="m9 15 6-6M8 17l-1 1a4 4 0 0 1-6-6l5-5a4 4 0 0 1 6 0M16 7l1-1a4 4 0 0 1 6 6l-5 5a4 4 0 0 1-6 0" transform="translate(0 0) scale(.92)" /></>,
    refresh: <><path d="M20 7v5h-5M4 17v-5h5" /><path d="M6.1 7A7 7 0 0 1 18 5l2 2M4 17l2 2a7 7 0 0 0 11.9-2" /></>,
    search: <><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 4 4" /></>,
    arrow: <path d="M5 12h14m-5-5 5 5-5 5" />,
    copy: <><rect x="8" y="8" width="12" height="12" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></>,
    stop: <rect x="6" y="6" width="12" height="12" rx="2" />,
    terminal: <><path d="m5 7 5 5-5 5M13 17h6" /></>,
    clock: <><circle cx="12" cy="12" r="8.5" /><path d="M12 7v5l3 2" /></>,
    check: <path d="m5 12 4 4 10-10" />,
    chevron: <path d="m9 5 7 7-7 7" />,
    computer: <><rect x="3" y="4" width="18" height="13" rx="2" /><path d="M8 21h8m-4-4v4" /></>,
    info: <><circle cx="12" cy="12" r="9" /><path d="M12 11v5m0-9v.1" /></>,
    close: <path d="m6 6 12 12M18 6 6 18" />,
  };
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}

export function BrandMark({ large = false }: { large?: boolean }) {
  return <svg className={large ? "brand-mark large" : "brand-mark"} viewBox="0 0 48 48" fill="none" aria-hidden="true">
    <path d="M17 11V7m14 4V7" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" />
    <path d="M7 24c0-9 6-14 17-14s17 5 17 14v5c0 8-6 13-17 13S7 37 7 29Z" fill="currentColor" fillOpacity=".09" stroke="currentColor" strokeWidth="2" />
    <path d="M17 23v5m14-5v5" stroke="currentColor" strokeWidth="3.5" strokeLinecap="round" />
    <path d="M21 34h6" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    <path d="M3 24v6m42-6v6" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
  </svg>;
}

const stateLabels: Record<OperationState, string> = {
  accepted: "Accepted", running: "Running", succeeded: "Succeeded", failed: "Failed",
  cancelled: "Cancelled", timed_out: "Timed out", interrupted: "Interrupted", unknown: "Unknown outcome",
};

function StatePill({ state }: { state: OperationState }) {
  return <span className={"state-pill state-" + state}><span className="state-dot" />{stateLabels[state]}</span>;
}

function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  if (milliseconds < 1000) return Math.max(0, milliseconds).toFixed(0) + " ms";
  if (seconds < 60) return seconds + "s";
  return Math.floor(seconds / 60) + "m " + String(seconds % 60).padStart(2, "0") + "s";
}

function time(value: number): string {
  return new Date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function dateTime(value: number): string {
  return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function leaf(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) || path;
}

function visibleText(value: string): string {
  // React escapes markup. Control bytes become visible characters; no terminal
  // parser interprets escape sequences, links, HTML or terminal control codes.
  return value.replace(/[\x00-\x08\x0b-\x1f\x7f]/g, (character) =>
    character === "\x7f" ? "␡" : String.fromCharCode(0x2400 + character.charCodeAt(0)));
}

async function copyText(text: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return;
    }
  } catch { /* WebKit may require the DOM copy path even in a local webview. */ }
  const previousFocus = document.activeElement;
  const field = document.createElement("textarea");
  field.value = text;
  field.className = "clipboard-staging";
  field.setAttribute("aria-hidden", "true");
  document.body.appendChild(field);
  field.select();
  const copied = document.execCommand("copy");
  field.remove();
  if (previousFocus instanceof HTMLElement) previousFocus.focus();
  if (!copied) throw new Error("Copy is unavailable. Select the command ID and copy it manually.");
}

function ErrorNote({ error, compact = false }: { error: BridgeError; compact?: boolean }) {
  return <div className={"error-note" + (compact ? " compact" : "")} role="alert">
    <Icon name="info" /><div><strong>{error.code.replaceAll("_", " ")}</strong><p>{error.message}</p></div>
  </div>;
}

export function App({ client }: { client: BridgeClient }) {
  const inspector = useInspector(client);
  const [view, setView] = useState<View>("activity");
  const [toast, setToast] = useState<{ message: string; error: boolean } | null>(null);
  const navigated = useRef(false);

  function navigate(next: View) { navigated.current = true; setView(next); }
  useEffect(() => {
    if (inspector.connection?.source === "unconfigured" && !navigated.current) setView("connection");
  }, [inspector.connection]);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 4000);
    return () => clearTimeout(timer);
  }, [toast]);
  async function copyId(id: string) {
    try { await copyText(id); setToast({ message: "Command ID copied", error: false }); }
    catch (error) { setToast({ message: describeError(error).message, error: true }); }
  }

  const titles: Record<View, { title: string; description: string }> = {
    activity: { title: "Activity", description: "A clear view of what your AI is doing." },
    workspaces: { title: "Workspaces", description: "The places your commands are working in." },
    connection: { title: "Connection", description: "Your desktop. Your local broker." },
  };
  const connectionLabel = inspector.listLoading ? "Connecting" : inspector.connected ? "Connected" : "Offline";

  return <div className="app-shell">
    <header className="brandbar">
      <div className="brand-lockup"><BrandMark /><div className="wordmark">Devagotchi<span>DESKTOP</span></div></div>
      <p>Your AI, at home in your workspace.</p>
      <span className="local-label"><span />LOCAL BY DESIGN</span>
    </header>
    <aside className="sidebar" aria-label="Main navigation">
      <div className="sidebar-section-label">WORKSPACE</div>
      <nav>
        {(["activity", "workspaces", "connection"] as View[]).map((item) => <button
          key={item} className={"nav-item" + (view === item ? " selected" : "")}
          aria-current={view === item ? "page" : undefined} onClick={() => navigate(item)}>
          <Icon name={item === "workspaces" ? "folder" : item} /><span>{titles[item].title}</span>
          {view === item && <span className="nav-indicator" />}
        </button>)}
      </nav>
      <div className="sidebar-bottom">
        <div className="device-card"><span className="device-icon"><Icon name="computer" /></span><div><strong>This computer</strong><span>{inspector.connection?.platform || "Local desktop"}</span></div></div>
        <div className="sidebar-footer"><span>devagotchi.net</span><span>v{inspector.connection?.app_version || "0.1.0"}</span></div>
      </div>
    </aside>
    <main className="main-content">
      <div className="page-heading">
        <div><h1>{titles[view].title}</h1><p>{titles[view].description}</p></div>
        <div className="heading-actions">
          <span className={"connection-pill " + (inspector.connected ? "online" : inspector.listLoading ? "connecting" : "offline")}><span />{connectionLabel}</span>
          <button className="button subtle icon-button" onClick={inspector.refresh} disabled={inspector.refreshing} aria-label="Refresh broker data" title="Refresh broker data"><Icon name="refresh" /></button>
        </div>
      </div>
      {inspector.listError && view !== "connection" && <div className="connection-banner" role="status">
        <Icon name="connection" /><div><strong>{inspector.lastSynced ? "Connection interrupted" : "Waiting for a local connection"}</strong><span>{inspector.lastSynced ? "The last received view is still available." : "Connect the desktop app to your broker to see commands here."}</span></div>
        <button className="text-button" onClick={() => navigate("connection")}>Check connection<Icon name="arrow" size={16} /></button>
      </div>}
      {view === "activity" && <Activity inspector={inspector} copyId={copyId} />}
      {view === "workspaces" && <Workspaces client={client} operations={inspector.page.operations} connected={inspector.connected} onOpen={(id) => { inspector.select(id); navigate("activity"); }} />}
      {view === "connection" && <Connection inspector={inspector} />}
      <footer className="main-footer"><span><span className="footer-dot" />Commands stay with your broker when you close this window.</span><span>{inspector.lastSynced ? "Last synced " + time(inspector.lastSynced) : "No broker data received"}</span></footer>
    </main>
    {toast && <div className={"toast" + (toast.error ? " toast-error" : "")} role="status"><Icon name={toast.error ? "info" : "check"} />{toast.message}<button aria-label="Dismiss notification" onClick={() => setToast(null)}><Icon name="close" size={16} /></button></div>}
  </div>;
}

function Activity({ inspector, copyId }: { inspector: Inspector; copyId: (id: string) => void }) {
  const [query, setQuery] = useState("");
  const [activeOnly, setActiveOnly] = useState(false);
  const visible = inspector.page.operations.filter((item) => {
    const state = inspector.operation?.operation_id === item.operation_id ? inspector.operation.state : item.state;
    return (!activeOnly || isActive(state)) && (item.display_command + " " + item.operation_id).toLowerCase().includes(query.toLowerCase());
  });

  return <div className="activity-layout">
    <section className="commands-panel" aria-label="Command list">
      <div className="panel-heading"><h2>Commands</h2><span className="quiet-count">{inspector.page.operations.length} loaded</span></div>
      <div className="command-filters">
        <label className="search-field"><Icon name="search" size={16} /><input aria-label="Filter loaded commands" placeholder="Find a command…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
        <div className="segmented-control" aria-label="Command status filter"><button className={!activeOnly ? "chosen" : ""} aria-pressed={!activeOnly} onClick={() => setActiveOnly(false)}>All commands</button><button className={activeOnly ? "chosen" : ""} aria-pressed={activeOnly} onClick={() => setActiveOnly(true)}>Active</button></div>
      </div>
      <div className="command-scroll">
        {inspector.listLoading ? <div className="compact-empty"><span className="loading-ring" /><p>Reading command history…</p></div>
          : visible.length ? <ul className="command-list">{visible.map((item) => {
            const state = inspector.operation?.operation_id === item.operation_id ? inspector.operation.state : item.state;
            return <li key={item.operation_id}><button className={"command-row" + (inspector.selectedId === item.operation_id ? " selected" : "")} data-operation-id={item.operation_id} aria-pressed={inspector.selectedId === item.operation_id} onClick={() => inspector.select(item.operation_id)}>
              <span className="command-row-top"><span className="command-glyph"><Icon name="terminal" size={17} /></span><span className="command-time">{time(item.created_at_ms)}</span><Icon name="chevron" size={14} /></span>
              <span className="command-preview" title={item.display_command}>{item.display_command}</span>
              <span className="command-row-bottom"><StatePill state={state} /><span className="short-id">{item.operation_id.slice(0, 8)}</span></span>
            </button></li>;
          })}</ul>
          : <div className="compact-empty"><Icon name={query || activeOnly ? "search" : "terminal"} size={26} /><h3>{query || activeOnly ? "No matching commands" : "A quiet workspace"}</h3><p>{query || activeOnly ? "Try a different filter for this loaded page." : inspector.connected ? "Commands will appear here when your client starts them." : "Command history appears after the broker connects."}</p>{(query || activeOnly) && <button className="text-button" onClick={() => { setQuery(""); setActiveOnly(false); }}>Clear filters</button>}</div>}
      </div>
      <div className="list-pagination"><span>{inspector.page.operations.length ? (inspector.offset + 1) + "–" + (inspector.offset + inspector.page.operations.length) : "0"} in history</span><div><button className="icon-button pagination-button previous" disabled={inspector.offset === 0 || inspector.refreshing} onClick={() => inspector.changePage(Math.max(0, inspector.offset - 32))} aria-label="Newer commands"><Icon name="chevron" size={15} /></button><button className="icon-button pagination-button" disabled={inspector.page.next_offset == null || inspector.refreshing} onClick={() => inspector.changePage(inspector.page.next_offset ?? 0)} aria-label="Older commands"><Icon name="chevron" size={15} /></button></div></div>
    </section>
    <section className="detail-panel" aria-label="Selected command">
      {inspector.operation ? <CommandDetail inspector={inspector} operation={inspector.operation} copyId={copyId} />
        : inspector.detailLoading ? <div className="detail-placeholder"><span className="loading-ring" /><h2>Opening command…</h2></div>
        : inspector.detailError ? <div className="detail-placeholder"><Icon name="info" size={32} /><h2>Command unavailable</h2><ErrorNote error={inspector.detailError} /><button className="button subtle" onClick={inspector.refresh}><Icon name="refresh" />Try again</button></div>
        : <div className="detail-placeholder welcome"><div className="welcome-orbit"><BrandMark large /></div><span className="eyebrow">A LITTLE CLARITY. A LOT OF CONTROL.</span><h2>{inspector.connected ? "Ready when you are." : "Your workspace starts here."}</h2><p>{inspector.connected ? "Choose a command to see its exact request, current state, and output as it arrives." : "Devagotchi gives your AI a home on your desktop. Connect a local broker, then follow every command from one place."}</p><div className="welcome-capabilities"><span><Icon name="terminal" size={16} />Exact commands</span><span><Icon name="activity" size={16} />Live output</span><span><Icon name="stop" size={16} />Explicit control</span></div></div>}
    </section>
  </div>;
}

function CommandDetail({ inspector, operation, copyId }: { inspector: Inspector; operation: OperationView; copyId: (id: string) => void }) {
  const [following, setFollowing] = useState(true);
  const consoleRef = useRef<HTMLDivElement>(null);
  useEffect(() => setFollowing(true), [operation.operation_id]);
  useEffect(() => {
    if (following && consoleRef.current) consoleRef.current.scrollTop = consoleRef.current.scrollHeight;
  }, [inspector.frames, following]);
  const active = isActive(operation.state);
  const elapsed = (active ? inspector.now : operation.updated_at_ms) - operation.created_at_ms;
  const hasText = inspector.frames.some((frame) => frame.text.length > 0);
  const outputLabel = inspector.detailError ? "Connection interrupted" : inspector.detailLoading ? "Reading output" : inspector.hasMoreOutput ? "Catching up" : operation.output_complete ? "Output complete" : active ? "Following output" : "Output incomplete";

  return <>
    <div className="detail-heading">
      <div className="detail-title-row"><div><span className="eyebrow">COMMAND DETAILS</span><h2>{leaf(operation.executable)}</h2></div><StatePill state={operation.state} /></div>
      <div className="operation-id-row"><code title={operation.operation_id}>{operation.operation_id}</code><button className="text-button copy-id" onClick={() => copyId(operation.operation_id)}><Icon name="copy" size={14} />Copy ID</button></div>
      <div className="detail-facts"><div><span><Icon name="clock" size={13} />Since accepted</span><strong>{duration(elapsed)}</strong></div><div><span>Execution deadline</span><strong>{duration(operation.deadline_ms)}</strong></div><div><span>Exit code</span><strong>{operation.exit_code ?? "—"}</strong></div></div>
    </div>
    <div className="detail-scroll">
      <details className="invocation-panel" open><summary><span>Invocation</span><span>Exact executable and arguments<Icon name="chevron" size={14} /></span></summary><dl>
        <div><dt>Executable</dt><dd><code>{operation.executable}</code></dd></div>
        <div><dt>Arguments</dt><dd><pre>{JSON.stringify(operation.args, null, 2)}</pre></dd></div>
        <div><dt>Directory</dt><dd><code>{operation.cwd}</code></dd></div>
      </dl></details>
      {(operation.detail || operation.cancellation_requested || inspector.actionError) && <div className="outcome-notes">
        {operation.detail && <p><Icon name="info" size={15} /><span>{operation.detail}</span></p>}
        {operation.cancellation_requested && active && <p><Icon name="stop" size={15} /><span>Cancellation requested. Waiting for the broker to confirm the outcome.</span></p>}
        {inspector.actionError && <ErrorNote error={inspector.actionError} compact />}
      </div>}
      <section className="output-panel" aria-label="Command output">
        <div className="output-heading"><h3><Icon name="terminal" size={16} />Output</h3><div><span className={"output-status" + (active && !inspector.detailError ? " following" : "")}>{active && !inspector.detailError && <span />}{outputLabel}</span><button className={"follow-button" + (following ? " enabled" : "")} onClick={() => setFollowing((current) => !current)} aria-pressed={following} title="Keep the newest output in view"><Icon name="arrow" size={14} />Follow</button></div></div>
        {(inspector.gap || inspector.trimmed) && <div className="output-notice"><Icon name="info" size={14} />{inspector.gap ? "Earlier output is no longer retained by the broker. " : ""}{inspector.trimmed ? "This view shows the most recent retained text to keep the app responsive." : ""}</div>}
        {inspector.detailError && <ErrorNote error={inspector.detailError} compact />}
        <div className="console" ref={consoleRef} tabIndex={0} aria-label="Scrollable plain text output" onScroll={(event) => {
          const element = event.currentTarget;
          if (element.scrollHeight - element.scrollTop - element.clientHeight > 56) setFollowing(false);
        }}>
          {hasText ? <pre className="console-output">{inspector.frames.map((frame) => <span key={frame.sequence} className={frame.stream === "stderr" ? "stderr" : "stdout"}>{visibleText(frame.text)}</span>)}</pre>
            : <div className="console-empty"><span className="prompt-chevron">›</span><span>{inspector.detailLoading ? "Reading retained output…" : inspector.frames.length ? "Output received with no displayable text." : operation.output_complete ? "This command produced no text output." : active ? "Waiting for the first output…" : "No retained text output; completeness cannot be confirmed."}</span></div>}
        </div>
        <div className="output-footer"><span><i className="stdout-dot" />stdout<i className="stderr-dot" />stderr</span><span>Plain text · control codes visible</span></div>
      </section>
    </div>
    <div className="command-actionbar"><div><span>Accepted {dateTime(operation.created_at_ms)}</span><details className="ownership-detail"><summary>Control scope<Icon name="info" size={12} /></summary><p>{operation.ownership_scope}</p></details></div>{active && <button className="button cancel-button" disabled={inspector.cancelling || operation.cancellation_requested || !inspector.connected} onClick={() => void inspector.cancel()}><Icon name="stop" size={15} />{inspector.cancelling ? "Requesting…" : operation.cancellation_requested ? "Cancellation requested" : "Cancel command"}</button>}</div>
  </>;
}

function Workspaces({ client, operations, connected, onOpen }: { client: BridgeClient; operations: OperationSummary[]; connected: boolean; onOpen: (id: string) => void }) {
  const [details, setDetails] = useState<OperationView[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<BridgeError | null>(null);
  const ids = operations.map((item) => item.operation_id).join(",");
  useEffect(() => {
    let disposed = false;
    if (!ids) { setDetails([]); setLoading(false); return; }
    if (!connected) { setLoading(false); return; }
    const operationIds = ids.split(",");
    let next = 0;
    const received: OperationView[] = [];
    let failure: BridgeError | null = null;
    setLoading(true);
    setError(null);
    async function worker() {
      while (next < operationIds.length && !disposed) {
        const id = operationIds[next++];
        try {
          const value = await client.getOperation({ operation_id: id });
          if (value.operation_id !== id) throw { code: "response_mismatch", message: "A working-directory response did not match its command." };
          received.push(value);
        } catch (error) { failure = describeError(error); }
      }
    }
    void Promise.all(Array.from({ length: Math.min(4, operationIds.length) }, () => worker())).then(() => {
      if (!disposed) { setDetails(received); setError(failure); setLoading(false); }
    });
    return () => { disposed = true; };
  }, [client, ids, connected]);
  const groups = useMemo(() => {
    const paths = new Map<string, OperationView[]>();
    for (const operation of details) paths.set(operation.cwd, [...(paths.get(operation.cwd) || []), operation]);
    return [...paths.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }, [details]);

  return <section className="workspaces-view">
    <div className="section-intro"><div><span className="eyebrow">OBSERVED DIRECTORIES</span><h2>Every command has a place.</h2><p>Working directories from the current page of command history. These are observations, not a list of granted access.</p></div><span className="intro-icon"><Icon name="folder" size={30} /></span></div>
    {error && <ErrorNote error={error} />}
    {loading ? <div className="wide-empty"><span className="loading-ring" /><h3>Reading working directories…</h3></div>
      : groups.length ? <div className="workspace-grid">{groups.map(([path, items]) => {
        const recent = [...items].sort((a, b) => b.created_at_ms - a.created_at_ms)[0];
        return <article className="workspace-card" key={path}><span className="workspace-folder"><Icon name="folder" size={23} /></span><h3>{leaf(path)}</h3><code>{path}</code><div><span>{items.length} {items.length === 1 ? "command" : "commands"} on this page</span><span>Latest {dateTime(recent.created_at_ms)}</span></div><button className="text-button" onClick={() => onOpen(recent.operation_id)}>View latest command<Icon name="arrow" size={16} /></button></article>;
      })}</div>
      : <div className="wide-empty"><Icon name="folder" size={30} /><h3>No working directories to show yet</h3><p>{connected ? "They will appear after your client starts commands in a workspace." : "Connect to your broker to read directories from command history."}</p></div>}
    <div className="scope-explanation"><Icon name="info" /><div><h3>Access stays under your control.</h3><p>Workspace and executable grants are configured at the broker. This inspector can read command history and request cancellation; it cannot expand those grants.</p></div></div>
  </section>;
}

function Connection({ inspector }: { inspector: Inspector }) {
  const info = inspector.connection;
  const sourceLabels = { command_line: "Command-line argument", environment: "DEVAGOTCHI_SOCKET", unconfigured: "Not configured" };
  return <section className="connection-view">
    <div className={"connection-overview" + (inspector.connected ? " connected" : "")}><div className="connection-illustration"><Icon name="computer" size={34} /><span /><BrandMark /></div><div><span className="eyebrow">LOCAL BROKER</span><h2>{inspector.connected ? "You're connected." : inspector.listLoading ? "Finding your broker…" : inspector.listError?.code === "desktop_required" ? "Open the desktop app." : info?.source === "unconfigured" ? "Give Devagotchi a connection." : "Unable to connect."}</h2><p>{inspector.connected ? "Your commands and output come directly from the broker on this computer." : "Start your broker independently, then launch Devagotchi with its absolute socket path."}</p></div><button className="button subtle" disabled={inspector.refreshing} onClick={inspector.refresh}><Icon name="refresh" size={16} />{inspector.refreshing ? "Checking…" : "Check again"}</button></div>
    {inspector.listError && <ErrorNote error={inspector.listError} />}
    <div className="connection-details"><h3>Connection details</h3><dl>
      <div><dt>Broker endpoint</dt><dd><code>{info?.socket_path || "Not configured"}</code></dd></div>
      <div><dt>Configured by</dt><dd>{info ? sourceLabels[info.source] : "Native desktop app required"}</dd></div>
      <div><dt>Platform</dt><dd className="platform-value">{info?.platform || "Unavailable outside the desktop app"}</dd></div>
      <div><dt>Last successful sync</dt><dd>{inspector.lastSynced ? dateTime(inspector.lastSynced) : "No broker data received"}</dd></div>
    </dl></div>
    <div className="connection-guidance"><h3>A small, explicit connection.</h3><p>Pass the path of your existing broker when opening the desktop app:</p><pre>devagotchi --socket &lt;absolute broker socket path&gt;</pre><p>You can also set <code>DEVAGOTCHI_SOCKET</code> before launching the app. The desktop window does not start a broker, change workspace access, or rerun commands when reconnecting.</p></div>
  </section>;
}
