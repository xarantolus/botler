<script setup lang="ts">
import { NModal, NSpin, NTag, NText } from "naive-ui";
import { ref, watch, shallowRef } from "vue";
import { call } from "../api/client";
import type { RunLogs } from "../generated";
import { notifyError } from "../notify";

const runId = defineModel<string | null>({ required: true });
const logs = shallowRef<RunLogs | null>(null);
const loading = ref(false);

watch(runId, async (id) => {
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
});
</script>

<template>
  <NModal
    :show="runId !== null"
    preset="card"
    :title="logs ? `Logs of ${logs.run.runnable}` : 'Logs'"
    style="max-width: 900px; width: calc(100vw - 32px)"
    @update:show="(v: boolean) => !v && (runId = null)"
  >
    <NSpin v-if="loading" />
    <template v-else-if="logs">
      <div style="margin-bottom: 6px">
        <NTag size="small" :bordered="false">{{ logs.run.status }}</NTag>
        <NText depth="3" style="font-size: 12px">
          {{ logs.run.kind }}<template v-if="logs.run.exit_code !== null"> · exit {{ logs.run.exit_code }}</template>
          <template v-if="logs.run.reason"> · {{ logs.run.reason }}</template>
        </NText>
      </div>
      <pre class="mono" style="max-height: 65vh">{{ logs.logs || "(no output)" }}</pre>
    </template>
  </NModal>
</template>
