// The hub's service worker: shows approval notifications and opens the approval when tapped.
// It caches nothing; the approvals page is always fetched fresh.
self.addEventListener("install", function () { self.skipWaiting(); });
self.addEventListener("activate", function (event) { event.waitUntil(self.clients.claim()); });
self.addEventListener("push", function (event) {
  var data = {};
  try { data = event.data ? event.data.json() : {}; } catch (e) { data = {}; }
  event.waitUntil(self.registration.showNotification(data.title || "Devagotchi", {
    body: data.body || "Something is waiting for your approval.",
    data: { url: data.url || "/approvals" },
    tag: data.tag || "devagotchi-approval",
    renotify: true,
    requireInteraction: true,
    icon: "/assets/icon.svg",
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
