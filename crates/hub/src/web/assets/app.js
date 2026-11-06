// The phone app's one script (`web/app/`): registers the service worker (which keeps the
// frame for offline), shows the install banner for iPhone (Share, then Add to Home Screen)
// or Android (the browser's install prompt) while the app is not installed, takes a closed
// ticket's Undo away when its 10 seconds run out, and reloads the page when it is pulled
// down from the top. Everything it shows is already on the page.
(function () {
  "use strict";
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js").catch(function () {});

  var banner = document.getElementById("install");
  var installed = (window.matchMedia && window.matchMedia("(display-mode: standalone)").matches) ||
    window.navigator.standalone === true;
  function show(os) {
    if (!banner || installed) return;
    var line = banner.querySelector("[data-os=" + os + "]");
    if (!line) return;
    line.hidden = false;
    banner.hidden = false;
  }
  if (/iPhone|iPad|iPod/.test(navigator.userAgent)) show("iphone");
  var prompt = null;
  window.addEventListener("beforeinstallprompt", function (event) {
    event.preventDefault();
    prompt = event;
    show("android");
  });
  var go = document.getElementById("install-go");
  if (go) go.addEventListener("click", function () {
    if (!prompt) return;
    prompt.prompt();
    prompt = null;
    banner.hidden = true;
  });
  window.addEventListener("appinstalled", function () { if (banner) banner.hidden = true; });

  // Undo lasts until the time the page gives (the hub refuses it after that anyway).
  var undo = document.getElementById("undo");
  if (undo) {
    var left = Number(undo.dataset.until) - Date.now();
    window.setTimeout(function () { undo.hidden = true; }, Math.max(0, left));
  }

  // Pull to refresh: the page scrolls inside the frame, so the browser's own pull does not
  // reach it (and an installed app has none).
  var main = document.querySelector("body.app > main");
  if (!main) return;
  var startY = null;
  main.addEventListener("touchstart", function (event) {
    startY = main.scrollTop <= 0 && event.touches.length === 1 ? event.touches[0].clientY : null;
  }, { passive: true });
  main.addEventListener("touchmove", function (event) {
    if (startY === null) return;
    var pulled = event.touches[0].clientY - startY;
    main.style.transform = pulled > 0 ? "translateY(" + Math.min(pulled / 3, 48) + "px)" : "";
    main.dataset.pulled = pulled > 120 ? "yes" : "";
  }, { passive: true });
  main.addEventListener("touchend", function () {
    var reload = main.dataset.pulled === "yes";
    main.style.transform = "";
    main.dataset.pulled = "";
    startY = null;
    if (reload) window.location.reload();
  });
})();
