self.addEventListener("push", (event) => {
  const fallback = { title: "Your Devagotchi needs you", body: "A tiny check-in keeps the bond growing." };
  let data = fallback;
  try { data = { ...fallback, ...event.data.json() }; } catch {}
  event.waitUntil(self.registration.showNotification(data.title, { body: data.body, tag: data.tag || "devagotchi-care", data: { url: data.url || "/sanctuary" } }));
});
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(clients.openWindow(event.notification.data?.url || "/sanctuary"));
});
