// Ask before doing something consequential: `if (await confirm({...})) ...`. One modal, opened
// through Nuxt UI's overlay provider (mounted by <UApp>).

import { useOverlay } from "@nuxt/ui/composables/useOverlay";
import ConfirmModal from "./components/common/ConfirmModal.vue";

export interface ConfirmOptions {
  title: string;
  description?: string;
  confirmLabel: string;
  color?: "primary" | "warning" | "error";
  /** Ask for an optional reason (returned in the result). */
  reason?: { label: string; placeholder?: string };
}

let modal: ReturnType<ReturnType<typeof useOverlay>["create"]> | null = null;

/** Resolves to `{ reason }` when confirmed; null when cancelled or dismissed. */
export async function confirm(o: ConfirmOptions): Promise<{ reason: string } | null> {
  modal ??= useOverlay().create(ConfirmModal);
  const result: unknown = await modal.open(o);
  return result && typeof result === "object" && "reason" in result ? (result as { reason: string }) : null;
}
