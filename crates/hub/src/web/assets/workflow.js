// A workflow's page follows the hub: every few seconds it asks /go/<token>/status where the
// steps stand and moves the ticks, facts and states in place, and shows "Keep your account"
// once this browser may. It never reloads the page, so nobody loses the line they are
// copying; "Check again" does the same without JavaScript. It also copies the line. The hub
// checks everything; this only reads what it is told and writes text, never markup. No
// other script talks to anyone but this hub.
"use strict";

(function () {
  const EVERY = 3000;

  // Copy buttons: shown only when this script runs, since they need it.
  const steps = document.getElementById("steps");
  const copyLabel = steps ? steps.dataset.copy : "";
  const copiedLabel = steps ? steps.dataset.copied : "";
  for (const button of document.querySelectorAll("button.copy[data-copy]")) {
    button.hidden = false;
    button.addEventListener("click", async () => {
      const source = document.getElementById(button.dataset.copy);
      if (!source) return;
      try {
        await navigator.clipboard.writeText(source.textContent);
      } catch (_) {
        // No clipboard here (an old browser, an insecure page): select it for the person.
        const range = document.createRange();
        range.selectNodeContents(source);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return;
      }
      button.textContent = copiedLabel || button.textContent;
      setTimeout(() => {
        button.textContent = copyLabel || button.textContent;
      }, 2000);
    });
  }

  const source = steps && steps.dataset.status;
  if (!source || steps.dataset.live !== "true") return;
  const check = document.getElementById("check");
  if (check) check.hidden = true;

  function labelFor(state, optional) {
    if (state === "pending" && optional) return steps.dataset.optional || "";
    return steps.dataset[state] || "";
  }

  function showSteps(answer) {
    const items = steps.querySelectorAll("li.step");
    (answer.steps || []).forEach((step, at) => {
      const item = items[at];
      if (!item) return;
      if (item.dataset.state !== step.state) {
        item.dataset.state = step.state;
        const label = item.querySelector(".step-state");
        if (label) label.textContent = labelFor(step.state, item.dataset.optional === "true");
      }
      const fact = item.querySelector(".fact");
      if (fact && typeof step.fact === "string" && fact.textContent !== step.fact) {
        fact.textContent = step.fact;
      }
    });
    const progress = document.getElementById("progress");
    if (progress && typeof answer.progress === "string") progress.textContent = answer.progress;
    const finished = document.getElementById("finished");
    if (finished) finished.hidden = answer.state !== "completed";
    const keep = document.getElementById("keep");
    if (keep && answer.keep === true) keep.hidden = false;
  }

  let stopped = false;
  let timer = null;
  function later() {
    clearTimeout(timer);
    timer = setTimeout(look, EVERY);
  }
  async function look() {
    clearTimeout(timer);
    if (stopped) return;
    if (document.visibilityState === "hidden") {
      later();
      return;
    }
    let answer = null;
    try {
      const response = await fetch(source, {
        credentials: "same-origin",
        cache: "no-store",
        headers: { Accept: "application/json" },
      });
      answer = await response.json();
    } catch (_) {
      answer = null;
    }
    if (answer) {
      if (Array.isArray(answer.steps)) showSteps(answer);
      // Done: the page says so and stops asking. Gone (expired, cancelled): stop, and offer
      // "Check again" for the person to see why.
      if (answer.state === "completed") {
        stopped = true;
        for (const form of document.querySelectorAll("form.get-command")) form.hidden = true;
        return;
      }
      if (answer.state !== "open" && answer.state !== "unavailable") {
        stopped = true;
        if (check) check.hidden = false;
        return;
      }
    }
    later();
  }
  later();
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && !stopped) look();
  });
})();
