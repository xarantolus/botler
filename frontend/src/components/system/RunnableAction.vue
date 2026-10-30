<script setup lang="ts">
// "Run now" for a oneshot job, "Restart" for a service.
import { ref } from "vue";
import { call } from "../../api/client";
import type { JobView } from "../../generated";
import { notifyError, notifySuccess } from "../../notify";

const props = defineProps<{ job: JobView; paused: boolean; size?: "xs" | "sm" | "md" }>();
const emit = defineEmits<{ done: [runId: string | null] }>();
const busy = ref(false);

async function act() {
  busy.value = true;
  const name = props.job.name;
  try {
    if (props.job.lifetime === "service") {
      await call("restart_service", { body: { service: name } });
      notifySuccess(`Restarting ${name}`, "It starts again in a moment.");
      emit("done", null);
    } else {
      const r = await call("run_job", { body: { job: name } });
      notifySuccess(r.queued ? `${name} queued` : `${name} started`, r.queued ? "It runs after the active run." : undefined);
      emit("done", r.run_id);
    }
  } catch (e) {
    notifyError(props.job.lifetime === "service" ? `Could not restart ${name}` : `Could not run ${name}`, e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <UTooltip :text="paused ? 'botler is paused' : job.lifetime === 'service' ? 'Stop it; it starts again right away' : 'Start a run now'">
    <UButton
      :size="size ?? 'xs'"
      color="neutral"
      variant="outline"
      :icon="job.lifetime === 'service' ? 'i-lucide-rotate-cw' : 'i-lucide-play'"
      :label="job.lifetime === 'service' ? 'Restart' : 'Run now'"
      :loading="busy"
      :disabled="paused"
      @click.stop="act"
    />
  </UTooltip>
</template>
