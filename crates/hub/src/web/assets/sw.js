// The hub's service worker: shows approval notifications and opens the approval when tapped.
// The default title and the icon below are filled in from the hub's site when it serves this file.
// It keeps the phone app's frame (its offline page), its script and the stylesheet, so the
// app opens offline with "You're offline" instead of a browser error; every page is still
// fetched fresh whenever the hub can be reached.
var FRAME = "devagotchi-app-1";
var KEPT = ["/app/offline", "/assets/hub.css", "/assets/app.js"];
self.addEventListener("install", function (event) {
  event.waitUntil(caches.open(FRAME).then(function (cache) { return cache.addAll(KEPT); })
    .catch(function () {}).then(function () { return self.skipWaiting(); }));
});
self.addEventListener("activate", function (event) {
  event.waitUntil(caches.keys().then(function (names) {
    return Promise.all(names.filter(function (name) { return name !== FRAME; })
      .map(function (name) { return caches.delete(name); }));
  }).then(function () { return self.clients.claim(); }));
});
self.addEventListener("fetch", function (event) {
  var request = event.request;
  if (request.method !== "GET") return;
  var url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  var app = url.pathname === "/app" || url.pathname.indexOf("/app/") === 0 || url.pathname === "/approvals";
  if (request.mode === "navigate" && app) {
    event.respondWith(fetch(request).catch(function () {
      return caches.match("/app/offline").then(function (kept) { return kept || Response.error(); });
    }));
  } else if (KEPT.indexOf(url.pathname) > 0) { // the script and the stylesheet
    event.respondWith(fetch(request).then(function (fresh) {
      if (fresh.ok) {
        var copy = fresh.clone();
        caches.open(FRAME).then(function (cache) { cache.put(url.pathname, copy); });
      }
      return fresh;
    }).catch(function () {
      return caches.match(url.pathname).then(function (kept) { return kept || Response.error(); });
    }));
  }
});
self.addEventListener("push", function (event) {
  var data = {};
  try { data = event.data ? event.data.json() : {}; } catch (e) { data = {}; }
  event.waitUntil(self.registration.showNotification(data.title || @TITLE@, {
    body: data.body || "Something is waiting for your approval.",
    data: { url: data.url || "/approvals" },
    tag: data.tag || "devagotchi-approval",
    renotify: true,
    requireInteraction: true,
    icon: @ICON@,
  }));
});
// The push service replaced this phone's subscription: subscribe again with the same key and
// tell the hub, so pushes keep arriving without anyone opening the page.
self.addEventListener("pushsubscriptionchange", function (event) {
  var options = event.oldSubscription && event.oldSubscription.options;
  var next = event.newSubscription ? Promise.resolve(event.newSubscription)
    : options ? self.registration.pushManager.subscribe(options) : Promise.reject(new Error("no options"));
  event.waitUntil(next.then(function (sub) {
    return fetch("/push/subscribe", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(sub.toJSON()),
    });
  }).catch(function () {}));
});
self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var target = new URL((event.notification.data && event.notification.data.url) || "/approvals", self.location.origin);
  if (target.origin !== self.location.origin) target = new URL("/approvals", self.location.origin);
  event.waitUntil(self.clients.openWindow(target.href));
});
