import { createRoot } from "react-dom/client";
import { App } from "../../src/App";
import { createFixtureClient } from "./fixture-client";
import "../../src/styles.css";
import "./fixture.css";

const scenario = new URLSearchParams(window.location.search).get("scenario") || "activity";
createRoot(document.getElementById("root")!).render(<>
  <div className="fixture-banner"><strong>TEST FIXTURE</strong><span>Synthetic broker responses · {scenario} · no real commands are running</span></div>
  <div className="fixture-surface"><App client={createFixtureClient(scenario)} /></div>
</>);
