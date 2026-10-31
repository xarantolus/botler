// Actions on a chat used from more than one place (its title bar, the command palette).

import { call } from "./api/client";
import { confirm } from "./confirm";
import { notifyError, notifySuccess } from "./notify";
import { refreshInstances, refreshSession } from "./store";

/** Stop a chat after confirming: abort its turn and cancel its queue, optionally its children's. */
export async function stopChat(chat: string): Promise<void> {
  const ok = await confirm({
    title: "Stop this chat?",
    description: "Its running turn is aborted and the messages queued for it are cancelled. It reads your reason when it next wakes.",
    confirmLabel: "Stop",
    color: "error",
    check: { label: "Also stop the chats it spawned" },
    reason: { label: "Reason", placeholder: "What it should do differently" },
  });
  if (!ok) return;
  try {
    const r = await call("stop_chat", { body: { chat, cascade: ok.checked, reason: ok.reason || undefined } });
    const aborted = r.stopped.filter((s) => s.aborted).length;
    const cancelled = r.stopped.reduce((n, s) => n + s.cancelled, 0);
    const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;
    const what = [
      ...(r.stopped.length > 1 ? [`${r.stopped.length} chats`] : []),
      ...(aborted ? [`${plural(aborted, "turn")} aborted`] : []),
      ...(cancelled ? [`${plural(cancelled, "queued message")} cancelled`] : []),
    ];
    notifySuccess("Stopped", what.join(", ") || "Nothing was running.");
  } catch (e) {
    notifyError("Could not stop the chat", e);
  }
  await Promise.all([refreshSession(chat), refreshInstances()]);
}

/** Ask a chat to hand over to a fresh session of itself (its report seeds the new session). */
export async function handoverChat(chat: string): Promise<void> {
  const ok = await confirm({
    title: "Start a fresh session?",
    description:
      "The agent writes a handover report (summary, outstanding work, where its state lives) after its current turn, then continues in a new session that starts from the report. The old session stays readable.",
    confirmLabel: "Start fresh session",
    reason: { label: "Reason", placeholder: "Why now, or what to focus on" },
  });
  if (!ok) return;
  try {
    const r = await call("handover", { body: { chat, to: "self", reason: ok.reason || undefined } });
    notifySuccess(r.requested ? "Handover requested" : "A handover is already pending", "The new session starts after the report.");
  } catch (e) {
    notifyError("Could not request the handover", e);
  }
  await refreshSession(chat);
}
