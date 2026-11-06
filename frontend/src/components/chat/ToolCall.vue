<script setup lang="ts">
// One tool call of an agent turn: its name and what it was asked, with arguments and result
// folded away; spinning while it runs. The result is shown as what it is (lib/toolResult.ts):
// links, a table, key/value pairs, Markdown, an image, or code; the raw text one click away.
import { computed, ref } from "vue";
import { argsSummary, argsText, resultText, toolIcon } from "../../lib/tools";
import { resultView } from "../../lib/toolResult";
import type { ToolPart } from "../../lib/transcript";
import { shell } from "../../shell";
import { state } from "../../store";
import CodeBlock from "../common/CodeBlock.vue";
import MarkdownText from "../common/MarkdownText.vue";
import ResultLinks from "./ResultLinks.vue";
import ResultTable from "./ResultTable.vue";

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
const view = computed(() =>
  output.value === null
    ? null
    : resultView(output.value, failed.value, state.session?.tool_renders[props.part.toolName]),
);
const raw = ref(false);
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
    :open="shell.open[part.toolCallId] ?? false"
    :data-tool-call="part.toolCallId"
    @update:open="(v: boolean) => (shell.open[part.toolCallId] = v)"
    :ui="{
      root: 'my-0.5',
      trigger: 'w-full cursor-pointer',
      leadingIcon: failed ? 'text-error' : undefined,
      label: 'font-mono text-xs',
      suffix: failed ? 'text-error' : undefined,
      body: 'pt-2 space-y-2 whitespace-normal',
    }"
  >
    <CodeBlock v-if="args" label="Arguments" :text="args" max-height="16rem" />
    <p v-if="!view" class="text-xs text-dimmed">{{ running ? "Running…" : "No result yet." }}</p>
    <CodeBlock v-else-if="view.kind === 'code' || raw" label="Result" :text="output ?? ''" max-height="20rem" />
    <div v-else>
      <div class="mb-1 flex items-center justify-between text-xs font-medium text-muted">
        <span>{{ view.kind === "error" ? "Error" : "Result" }}</span>
        <UButton size="xs" color="neutral" variant="link" label="Raw" class="p-0" @click="raw = true" />
      </div>
      <UAlert
        v-if="view.kind === 'error'"
        color="error"
        variant="subtle"
        icon="i-lucide-circle-x"
        :description="view.text"
        :ui="{ description: 'font-mono text-xs whitespace-pre-wrap break-words max-h-60 overflow-auto' }"
      />
      <ResultLinks v-else-if="view.kind === 'links'" :items="view.items" :meta="view.meta" />
      <ResultTable v-else-if="view.kind === 'table'" :columns="view.columns" :rows="view.rows" :meta="view.meta" />
      <dl v-else-if="view.kind === 'keyvalue'" class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 rounded-md border border-muted px-3 py-2 text-xs">
        <template v-for="[k, v] in view.entries" :key="k">
          <dt class="font-mono text-muted">{{ k }}</dt>
          <dd class="min-w-0 break-words text-default">{{ v }}</dd>
        </template>
      </dl>
      <MarkdownText
        v-else-if="view.kind === 'markdown'"
        :source="view.text"
        class="max-h-96 overflow-auto rounded-md border border-muted px-3 py-2 text-sm"
      />
      <img
        v-else-if="view.kind === 'image'"
        :src="view.src"
        alt="Tool result image"
        referrerpolicy="no-referrer"
        class="max-h-80 max-w-full rounded-md border border-muted"
      />
    </div>
  </UChatTool>
</template>
