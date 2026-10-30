<script setup lang="ts">
// The logs of one run, in a modal (open while the model holds a run id).
import { ref, shallowRef, watch } from "vue";
import { call } from "../../api/client";
import type { RunLogs } from "../../generated";
import { runStatusColor } from "../../lib/runs";
import { notifyError } from "../../notify";
import CodeBlock from "./CodeBlock.vue";

const runId = defineModel<string | null>({ required: true });
const logs = shallowRef<RunLogs | null>(null);
const loading = ref(false);

watch(
  runId,
  async (id) => {
    logs.value = null;
    if (!id) return;
    loading.value = true;
    try {
      logs.value = await call("run_logs", { body: { run_id: id, tail: 1000 } });
    } catch (e) {
      notifyError("Could not load the logs", e);
      runId.value = null;
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);
</script>

<template>
  <UModal
    :open="runId !== null"
    :title="logs ? `Logs of ${logs.run.runnable}` : 'Logs'"
    :description="logs ? `${logs.run.kind} run, last 1000 lines` : 'Loading'"
    :ui="{ content: 'sm:max-w-4xl' }"
    @update:open="(o: boolean) => !o && (runId = null)"
  >
    <template #body>
      <div v-if="loading" class="space-y-2">
        <USkeleton class="h-4 w-1/3" />
        <USkeleton class="h-48 w-full" />
      </div>
      <div v-else-if="logs" class="space-y-3">
        <div class="flex flex-wrap items-center gap-2 text-sm text-muted">
          <UBadge :color="runStatusColor(logs.run.status)" variant="subtle" :label="logs.run.status" />
          <span v-if="logs.run.exit_code !== null">exit {{ logs.run.exit_code }}</span>
          <span v-if="logs.run.reason">· {{ logs.run.reason }}</span>
        </div>
        <CodeBlock :text="logs.logs || '(no output)'" max-height="65vh" />
      </div>
    </template>
  </UModal>
</template>
