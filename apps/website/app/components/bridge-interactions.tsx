"use client";

import { useState, type CSSProperties } from "react";
import { Button, Tab, TabList, TabPanel, Tabs } from "react-aria-components";
import { graphExamples } from "../../lib/devagotchi-reference";

export function CodeBlock({ code, label = "Example" }: { code: string; label?: string }) {
  const [copyStatus, setCopyStatus] = useState("");
  async function copy() {
    try { await navigator.clipboard.writeText(code); setCopyStatus("Copied"); }
    catch { setCopyStatus("Select the code to copy"); }
  }
  return <div className="bridge-code"><div className="bridge-code-bar"><span>{label}</span><Button onPress={copy} aria-label={`Copy ${label}`}>{copyStatus || "Copy"}</Button><span className="bridge-sr-only" role="status">{copyStatus}</span></div><pre tabIndex={0}><code>{code}</code></pre></div>;
}

export function GraphExplorer({ compact = false }: { compact?: boolean }) {
  const [mode, setMode] = useState<"local" | "mesh">("local");
  const [selected, setSelected] = useState("test");
  const example = graphExamples[mode];
  const node = example.nodes.find(item => item.id === selected) ?? example.nodes[0];
  return <div className={`bridge-graph${compact ? " bridge-graph-compact" : ""}`}>
    <div className="bridge-graph-top"><span><i/> task / workspace-check</span><span className="bridge-concept-label">Interactive concept</span></div>
    <Tabs selectedKey={mode} onSelectionChange={key => { setMode(key as "local" | "mesh"); setSelected(key === "local" ? "test" : "windows"); }}>
      <TabList aria-label="Example topology" className="bridge-tabs"><Tab id="local">One workstation</Tab><Tab id="mesh">Internal mesh <span>Planned</span></Tab></TabList>
      <TabPanel id={mode} className="bridge-graph-panel">
        <div className="bridge-graph-canvas" aria-label={example.title}>
          <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">{example.edges.map(([from,to]) => { const a = example.nodes.find(item => item.id === from)!; const b = example.nodes.find(item => item.id === to)!; const x1 = a.x + 20; const y1 = a.y + 9; const x2 = b.x; const y2 = b.y + 9; return <path key={`${from}-${to}`} d={`M${x1},${y1} C${(x1+x2)/2},${y1} ${(x1+x2)/2},${y2} ${x2},${y2}`} className={node.id === to || node.id === from ? "selected" : ""}/>; })}</svg>
          {example.nodes.map(item => <Button key={item.id} className={`bridge-graph-node${node.id === item.id ? " is-selected" : ""}`} style={{"--node-x":`${item.x}%`,"--node-y":`${item.y}%`} as CSSProperties} onPress={() => setSelected(item.id)} aria-pressed={node.id === item.id}><span className="bridge-node-kind">{item.kind}</span><strong>{item.title}</strong><small>{item.device}</small></Button>)}
        </div>
        <div className="bridge-node-detail" aria-live="polite"><div><span className="bridge-node-kind">{node.kind} / {node.id}</span><h3>{node.title}</h3><p>{node.detail}</p></div><code>{node.command}</code></div>
      </TabPanel>
    </Tabs>
    <div className="bridge-graph-bottom"><span><i/> Select a node to inspect its role</span><span>Illustrative data · no commands run</span></div>
  </div>;
}

export function ApiExplorer() {
  const [endpoint, setEndpoint] = useState("/api/v1/capabilities");
  const [result, setResult] = useState("Choose a reference endpoint, then send a GET request.");
  const [state, setState] = useState<"idle" | "loading" | "success" | "error">("idle");
  async function read() {
    setState("loading");
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10000);
    try {
      const response = await fetch(endpoint, { headers: { Accept: "application/json" }, signal: controller.signal });
      const data = await response.json();
      setResult(JSON.stringify(data, null, 2));
      setState(response.ok ? "success" : "error");
    } catch { setResult("The reference request did not complete. Check your connection and try again."); setState("error"); }
    finally { clearTimeout(timer); }
  }
  return <div className="bridge-api-explorer"><div className="bridge-api-controls"><label htmlFor="reference-endpoint">GET</label><select id="reference-endpoint" value={endpoint} onChange={event => { setEndpoint(event.target.value); setState("idle"); setResult("Endpoint changed. Send a request to read its response."); }}><option value="/api/v1/capabilities">/api/v1/capabilities</option><option value="/api/v1/reference?topic=local-tools">/api/v1/reference?topic=local-tools</option><option value="/api/v1/reference?topic=task-graphs">/api/v1/reference?topic=task-graphs</option><option value="/openapi.json">/openapi.json</option></select><Button onPress={read} isDisabled={state === "loading"} className="bridge-button">{state === "loading" ? "Reading…" : "Send request"}<span aria-hidden="true">↗</span></Button></div><div className="bridge-response-status" role="status">{state === "success" ? "200 · Live response from this website" : state === "error" ? "Request failed" : "Read-only · no API key required"}</div><pre tabIndex={0} aria-label="API response"><code>{result}</code></pre></div>;
}
