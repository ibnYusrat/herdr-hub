// Browser notifications: fire when a pane's agent transitions INTO blocked
// while the page is hidden (SPEC §7). Permission is requested from the
// toolbar bell; nothing is notified without it.

const prevStatus = new Map<string, string>();

export function notificationSupported(): boolean {
  return typeof window !== "undefined" && "Notification" in window;
}

export function notificationPermission(): NotificationPermission | "unsupported" {
  return notificationSupported() ? Notification.permission : "unsupported";
}

export async function requestNotificationPermission(): Promise<NotificationPermission | "unsupported"> {
  if (!notificationSupported()) return "unsupported";
  if (Notification.permission === "default") await Notification.requestPermission();
  return Notification.permission;
}

/** Track one pane's status; notify on →blocked transitions when hidden. */
export function trackAgentStatus(
  server: string,
  paneId: string,
  status: string,
  who: string,
): void {
  const key = `${server}:${paneId}`;
  const was = prevStatus.get(key);
  prevStatus.set(key, status);
  if (status !== "blocked" || was === "blocked" || was === undefined) return;
  if (!document.hidden) return;
  if (!notificationSupported() || Notification.permission !== "granted") return;
  const n = new Notification(`herdr — ${who} blocked`, {
    tag: key,
    body: "the agent is waiting for an answer",
  });
  n.onclick = () => {
    window.focus();
    n.close();
  };
}
