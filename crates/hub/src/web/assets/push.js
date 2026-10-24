// Turns on approval notifications for this phone: registers the service worker, asks for
// permission (it must follow a tap; on iPhone the page must be on the Home Screen), subscribes
// with the hub's public key and tells the hub where to push. Once permission is given, every
// visit saves the subscription again (renewing it if the push service dropped it), so a phone
// the hub stopped reaching heals by opening the page. No other script runs on the hub.
(function () {
  "use strict";
  var button = document.getElementById("notify");
  var status = document.getElementById("notify-status");
  if (!button || !status) return;
  function say(text) { status.textContent = text; }
  if (!("serviceWorker" in navigator) || !("PushManager" in window) || !("Notification" in window)) {
    say("This browser cannot get notifications here. On iPhone, add this page to the Home Screen and open it from there.");
    button.disabled = true;
    return;
  }
  function bytes(b64) {
    var padded = b64.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (b64.length % 4)) % 4);
    var raw = atob(padded);
    var out = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }
  // This phone's subscription, made if there is none, saved on the hub.
  function subscribeAndSave() {
    return navigator.serviceWorker.ready.then(function (reg) {
      return reg.pushManager.getSubscription().then(function (sub) {
        return sub || reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(button.dataset.key) });
      });
    }).then(function (sub) {
      return fetch("/push/subscribe", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(sub.toJSON()),
      });
    }).then(function (res) {
      say(res.ok ? "Notifications are on for this phone." : "The hub did not save this phone (" + res.status + ").");
    });
  }
  navigator.serviceWorker.register("/sw.js").then(function () {
    if (Notification.permission === "granted") return subscribeAndSave();
    if (Notification.permission === "denied") say("Notifications are blocked for this site; allow them in the browser's site settings.");
  }).catch(function () {});
  button.addEventListener("click", function () {
    Notification.requestPermission().then(function (permission) {
      if (permission !== "granted") {
        say("Notifications are blocked for this site; allow them in the browser's site settings, then tap again.");
        return null;
      }
      return subscribeAndSave();
    }).catch(function (e) {
      say("Could not turn on notifications: " + e.message);
    });
  });
})();
