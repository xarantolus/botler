<script setup lang="ts">
// The parts of one transcript message (lib/transcript.ts), each with the kit's chat element for
// it: reasoning, text (Markdown), tool calls, and botler's own parts as cards.
import { computed } from "vue";
import type { TranscriptMessage } from "../../lib/transcript";
import { toolRunning } from "../../lib/transcript";
import DialogCard from "../common/DialogCard.vue";
import EnvelopeCard from "../common/EnvelopeCard.vue";
import MarkdownText from "../common/MarkdownText.vue";
import { shell } from "../../shell";
import MessageFiles from "./MessageFiles.vue";
import NotePart from "./NotePart.vue";
import ToolCall from "./ToolCall.vue";

const props = defineProps<{ message: TranscriptMessage; busy: boolean }>();
/** The message's stable key (an agent turn keeps it from streamed to persisted). */
const key = computed(() => props.message.metadata?.key ?? props.message.id);

/** The user toggled a thinking block: it is theirs now, never closed automatically. */
function onReasoningClick(e: MouseEvent, id: string) {
  if ((e.target as Element).closest("[data-slot=trigger]")) shell.touched[id] = true;
}
</script>

<template>
  <div class="flex min-w-0 flex-col gap-2.5">
    <!-- Keyed by position: a streamed message and its persisted copy have the same parts. -->
    <template v-for="(part, i) in message.parts" :key="i">
      <!-- A closed thinking block is not mounted: each measures its scroll shadow, which lays out
           the whole chat, so a long chat would open slowly. -->
      <UChatReasoning
        v-if="part.type === 'reasoning'"
        :text="part.text"
        :streaming="part.state === 'streaming'"
        :open="shell.open[`${key}:${i}`] ?? false"
        :auto-close-delay="shell.touched[`${key}:${i}`] ? 0 : 500"
        icon="i-lucide-brain"
        unmount-on-hide
        :ui="{ body: 'max-h-72 text-sm' }"
        @update:open="(v: boolean) => (shell.open[`${key}:${i}`] = v)"
        @click.capture="(e: MouseEvent) => onReasoningClick(e, `${key}:${i}`)"
      />
      <MarkdownText v-else-if="part.type === 'text'" :source="part.text" :class="part.state === 'streaming' && 'streaming'" />
      <ToolCall v-else-if="part.type === 'dynamic-tool'" :part="part" :running="toolRunning(part, busy)" />
      <DialogCard v-else-if="part.type === 'data-dialog'" :dialog="part.data" compact class="my-1" />
      <EnvelopeCard v-else-if="part.type === 'data-envelope'" :env="part.data.envelope" :at="part.data.at" />
      <NotePart v-else-if="part.type === 'data-note'" :note="part.data" />
      <UCollapsible v-else-if="part.type === 'data-session'" class="rounded-md border border-dashed border-primary/50">
        <UButton
          color="neutral"
          variant="ghost"
          size="sm"
          block
          icon="i-lucide-history"
          trailing-icon="i-lucide-chevron-down"
          label="New session, continued from a handover"
          class="justify-start text-muted"
          :ui="{ trailingIcon: 'ms-auto group-data-[state=open]:rotate-180 transition-transform' }"
        />
        <template #content>
          <div class="space-y-2 border-t border-default px-3 py-2.5 text-sm text-muted">
            <MarkdownText :source="part.data.summary" />
            <ULink
              :to="{ query: { file: part.data.previous } }"
              class="inline-flex items-center gap-1 text-xs text-primary hover:underline"
            >
              <UIcon name="i-lucide-history" class="size-3.5" />Read the previous session
            </ULink>
            <ul v-if="part.data.outstanding.length" class="list-disc ps-5">
              <li v-for="o in part.data.outstanding" :key="o">outstanding: {{ o }}</li>
            </ul>
            <ul v-if="part.data.state.length" class="list-disc ps-5">
              <li v-for="s in part.data.state" :key="s">state: {{ s }}</li>
            </ul>
          </div>
        </template>
      </UCollapsible>
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
    <MessageFiles v-if="message.metadata?.files?.length" :files="message.metadata.files" />
  </div>
</template>
