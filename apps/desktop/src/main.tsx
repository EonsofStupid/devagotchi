import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { nativeBridge } from "./bridge";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App client={nativeBridge} />
  </StrictMode>,
);
