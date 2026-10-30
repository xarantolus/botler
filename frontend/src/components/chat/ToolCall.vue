<script setup lang="ts">
// One tool call of an agent turn: its name and what it was asked, with arguments and result
// folded away; spinning while it runs.
import { computed } from "vue";
import { argsSummary, argsText, resultText, toolIcon } from "../../lib/tools";
import type { ToolPart } from "../../lib/transcript";
import CodeBlock from "../common/CodeBlock.vue";

const props = defineProps<{ part: ToolPart; running: boolean }>();

const failed = computed(() => props.part.state === "output-error");
const args = computed(() => argsText(props.part.input));
const output = computed(() =>
  props.part.state === "output-available"
    ? resultText(props.part.output)
    : props.part.state === "output-error"
      ? props.part.errorText
      : null,
);
const suffix = computed(() => {
  const s = argsSummary(props.part.input, 72);
  if (failed.value) return s ? `${s} · failed` : "failed";
  return s;
});
</script>

<template>
  <UChatTool
    :text="part.toolName"
    :suffix="suffix"
    :icon="failed ? 'i-lucide-circle-x' : toolIcon(part.toolName)"
    :loading="running"
    :streaming="running"
    :ui="{
      root: 'my-0.5',
      leadingIcon: failed ? 'text-error' : undefined,
      label: 'font-mono text-xs',
      suffix: failed ? 'text-error' : undefined,
      body: 'pt-2 space-y-2 whitespace-normal',
    }"
  >
    <CodeBlock v-if="args" label="Arguments" :text="args" max-height="16rem" />
    <CodeBlock v-if="output !== null" :label="failed ? 'Error' : 'Result'" :text="output" :error="failed" max-height="20rem" />
    <p v-else class="text-xs text-dimmed">{{ running ? "Running…" : "No result yet." }}</p>
  </UChatTool>
</template>
