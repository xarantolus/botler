<script setup lang="ts">
// Resume what a breaker paused: held messages of a pair are delivered.
import { ref } from "vue";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import { notifyError, notifySuccess } from "../../notify";
import { refreshStatus } from "../../store";

const props = defineProps<{ subject: string; size?: "xs" | "sm" }>();
const busy = ref(false);

async function resume() {
  const ok = await confirm({
    title: "Resume?",
    description:
      "The breaker is cleared. For two agents that kept messaging each other, their held messages are delivered and the count starts again.",
    confirmLabel: "Resume",
    color: "warning",
  });
  if (!ok) return;
  busy.value = true;
  try {
    const r = await call("clear_breaker", { body: { subject: props.subject } });
    notifySuccess("Resumed", r.released ? `${r.released} held message(s) delivered.` : undefined);
    await refreshStatus();
  } catch (e) {
    notifyError("Could not resume", e);
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
    icon="i-lucide-play"
    label="Resume"
    :loading="busy"
    @click="resume"
  />
</template>
