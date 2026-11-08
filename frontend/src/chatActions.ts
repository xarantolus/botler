// Actions on a chat used from more than one place (its title bar, the command palette).

import { useOverlay } from "@nuxt/ui/composables/useOverlay";
import { call } from "./api/client";
import ContextSizeModal from "./components/chat/ContextSizeModal.vue";
import type { ContextAdvice } from "./generated/ContextAdvice";
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

export async function pinChat(chat: string, pinned: boolean): Promise<void> {
  try {
    await call("pin_chat", { body: { chat, pinned } });
    await refreshInstances();
  } catch (e) {
    notifyError(pinned ? "Could not pin" : "Could not unpin", e);
  }
}

/** Set how large the chat's context grows before it is compacted, in thousands of tokens. */
export async function setChatContext(chat: string, current: number | null): Promise<void> {
  // What the chat's last week would have cost at each size, when it ran enough calls to say.
  let advice: ContextAdvice | null = null;
  try {
    advice = (await call("context_advice", { body: { chat } })).advice;
  } catch (e) {
    notifyError("Could not price the context sizes", e);
  }
  contextModal ??= useOverlay().create(ContextSizeModal);
  const ok: unknown = await contextModal.open({ current, advice });
  if (!ok || typeof ok !== "object") return;
  try {
    const { compactAt } = ok as { compactAt: number | null };
    const r = await call("set_chat_context", { body: { chat, compact_at: compactAt } });
    notifySuccess("Context size", r.note);
    await refreshInstances();
  } catch (e) {
    notifyError("Could not set the context size", e);
  }
}
let contextModal: ReturnType<ReturnType<typeof useOverlay>["create"]> | null = null;

export async function renameChat(chat: string, current: string): Promise<void> {
  const ok = await confirm({
    title: "Rename this chat",
    description: "Its agent keeps your title from now on.",
    confirmLabel: "Rename",
    reason: { label: "Title", required: true, value: current },
  });
  if (!ok) return;
  try {
    await call("rename_chat", { body: { chat, title: ok.reason } });
    await refreshInstances();
  } catch (e) {
    notifyError("Could not rename", e);
  }
}

/** Archive at once, without waiting for the agent's handover report. */
export async function archiveNow(chat: string): Promise<void> {
  const ok = await confirm({
    title: "Archive this chat now?",
    description: "It leaves the list right away, without the agent's handover report. Nothing is deleted; a message to it restores it.",
    confirmLabel: "Archive",
    color: "warning",
  });
  if (!ok) return;
  try {
    await call("archive_chat", { params: { instance: chat }, body: { force: true } });
    await refreshInstances();
  } catch (e) {
    notifyError("Could not archive", e);
  }
}
