<script setup lang="ts">
// The parts of one transcript message (lib/transcript.ts), each with the kit's chat element for
// it: reasoning, text (Markdown), tool calls, and botler's own parts as cards.
import type { TranscriptMessage } from "../../lib/transcript";
import { toolRunning } from "../../lib/transcript";
import DialogCard from "../common/DialogCard.vue";
import EnvelopeCard from "../common/EnvelopeCard.vue";
import MarkdownText from "../common/MarkdownText.vue";
import NotePart from "./NotePart.vue";
import ToolCall from "./ToolCall.vue";

defineProps<{ message: TranscriptMessage; busy: boolean }>();
</script>

<template>
  <div class="flex min-w-0 flex-col gap-2.5">
    <template v-for="(part, i) in message.parts" :key="`${message.id}-${i}`">
      <UChatReasoning
        v-if="part.type === 'reasoning'"
        :text="part.text"
        :streaming="part.state === 'streaming'"
        icon="i-lucide-brain"
        :ui="{ body: 'max-h-72 text-sm' }"
      />
      <MarkdownText v-else-if="part.type === 'text'" :source="part.text" :class="part.state === 'streaming' && 'streaming'" />
      <ToolCall v-else-if="part.type === 'dynamic-tool'" :part="part" :running="toolRunning(part, busy)" />
      <DialogCard v-else-if="part.type === 'data-dialog'" :dialog="part.data" class="my-1" />
      <EnvelopeCard v-else-if="part.type === 'data-envelope'" :env="part.data.envelope" :at="part.data.at" />
      <NotePart v-else-if="part.type === 'data-note'" :note="part.data" />
      <UCollapsible v-else-if="part.type === 'data-compaction'" class="rounded-md border border-dashed border-default">
        <UButton
          color="neutral"
          variant="ghost"
          size="sm"
          block
          icon="i-lucide-fold-vertical"
          trailing-icon="i-lucide-chevron-down"
          label="Earlier conversation compacted"
          class="justify-start text-muted"
          :ui="{ trailingIcon: 'ms-auto group-data-[state=open]:rotate-180 transition-transform' }"
        />
        <template #content>
          <MarkdownText :source="part.data.summary" class="border-t border-default px-3 py-2.5 text-sm text-muted" />
        </template>
      </UCollapsible>
      <UAlert
        v-else-if="part.type === 'data-error'"
        color="error"
        variant="subtle"
        icon="i-lucide-circle-alert"
        title="The model returned an error"
        :description="part.data.text"
      />
    </template>
  </div>
</template>
