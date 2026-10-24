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
self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var target = new URL((event.notification.data && event.notification.data.url) || "/approvals", self.location.origin);
  if (target.origin !== self.location.origin) target = new URL("/approvals", self.location.origin);
  event.waitUntil(self.clients.openWindow(target.href));
});
