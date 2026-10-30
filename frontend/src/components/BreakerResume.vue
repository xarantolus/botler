<script setup lang="ts">
// Resume what a breaker paused: held messages of a pair are delivered.
import { NButton } from "naive-ui";
import { ref } from "vue";
import { call } from "../api/client";
import { dialog, message, notifyError } from "../notify";
import { refreshStatus } from "../store";

const props = defineProps<{ subject: string; size?: "tiny" | "small" }>();
const busy = ref(false);

function resume() {
  dialog.warning({
    title: "Resume?",
    content:
      "The breaker is cleared. For two agents that kept messaging each other, their held messages are delivered and the count starts again.",
    positiveText: "Resume",
    negativeText: "Cancel",
    onPositiveClick: async () => {
      busy.value = true;
      try {
        const r = await call("clear_breaker", { body: { subject: props.subject } });
        message.success(r.released ? `Resumed; ${r.released} held message(s) delivered` : "Resumed");
        await refreshStatus();
      } catch (e) {
        notifyError("Could not resume", e);
      } finally {
        busy.value = false;
      }
    },
  });
}
</script>

<template>
  <NButton :size="size ?? 'small'" type="warning" :loading="busy" @click="resume">Resume</NButton>
</template>
