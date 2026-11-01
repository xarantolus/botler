// Ask before doing something consequential: `if (await confirm({...})) ...`. One modal, opened
// through Nuxt UI's overlay provider (mounted by <UApp>).

import { useOverlay } from "@nuxt/ui/composables/useOverlay";
import ConfirmModal from "./components/common/ConfirmModal.vue";

export interface ConfirmOptions {
  title: string;
  description?: string;
  confirmLabel: string;
  color?: "primary" | "warning" | "error";
  /** Ask for a reason (returned in the result); optional unless `required`. */
  reason?: { label: string; placeholder?: string; required?: boolean; value?: string };
  /** A checkbox (its state is `checked` in the result). */
  check?: { label: string };
}

let modal: ReturnType<ReturnType<typeof useOverlay>["create"]> | null = null;

/** Resolves to `{ reason, checked }` when confirmed; null when cancelled or dismissed. */
export async function confirm(o: ConfirmOptions): Promise<{ reason: string; checked: boolean } | null> {
  modal ??= useOverlay().create(ConfirmModal);
  const result: unknown = await modal.open(o);
  return result && typeof result === "object" && "reason" in result ? (result as { reason: string; checked: boolean }) : null;
}

/** The user's decision on a proposal: approving takes an optional comment, rejecting a reason.
 * Both reach the proposing agent. */
export function confirmDecision(approve: boolean, title?: string): Promise<{ reason: string } | null> {
  return confirm(
    approve
      ? {
          title: title ? `Approve "${title}"?` : "Approve this change?",
          description: "It merges into main and is applied right away.",
          confirmLabel: "Approve",
          reason: { label: "Comment for the agent", placeholder: "Anything it should know" },
        }
      : {
          title: title ? `Reject "${title}"?` : "Reject this change?",
          description: "Its checks stop where they are. The agent reads your reason before trying again.",
          confirmLabel: "Reject",
          color: "error",
          reason: { label: "Reason", placeholder: "What is wrong, or what should change", required: true },
        },
  );
}
