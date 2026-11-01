<script setup lang="ts">
// Let a chat that reached its daily budget spend more today only (its definition stays as it is).
import { ref } from "vue";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import { notifyError, notifySuccess } from "../../notify";
import { refreshStatus } from "../../store";

const props = defineProps<{ chat: string; size?: "xs" | "sm" }>();
const busy = ref(false);

async function raise() {
  const r = await confirm({
    title: "Allow more today?",
    description: "Added to this chat's budget until midnight; tomorrow its usual budget applies again. It resumes once it is under the new limit.",
    confirmLabel: "Allow",
    color: "warning",
    reason: { label: "Dollars more today", value: "1", required: true },
  });
  if (!r) return;
  const usd = Number(r.reason.replace(/^\$/, "").trim());
  if (!Number.isFinite(usd) || usd <= 0) {
    notifyError("Not an amount", new Error(`"${r.reason}" is not a dollar amount`));
    return;
  }
  busy.value = true;
  try {
    const out = await call("raise_budget", { body: { chat: props.chat, usd } });
    notifySuccess(
      "Budget raised for today",
      `$${out.spent_usd.toFixed(2)} spent of $${(out.usd_per_day ?? 0).toFixed(2)} today.`,
    );
    await refreshStatus();
  } catch (e) {
    notifyError("Could not raise the budget", e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <UButton
    :size="size ?? 'sm'"
    color="warning"
    variant="solid"
    icon="i-lucide-circle-dollar-sign"
    label="Allow more today"
    :loading="busy"
    @click="raise"
  />
</template>
