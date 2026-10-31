<script setup lang="ts">
// The transcript of a chat with the kit's chat elements: the user's messages as bubbles on the
// right, agent turns and incoming events on the left, "working…" while the agent works.
// Not UChatMessages: ChatView does all the scrolling, and the kit's list deep-watches every
// message on each streamed chunk (and would scroll to the last user message when the agent gets
// busy). Each message is its own component (TranscriptEntry), so a chunk re-renders only the
// message it changes. A long chat renders its latest PAGE messages; "Show earlier" adds more (the
// view's scroll anchoring keeps the reader's place).
import type { ChatStatus } from "ai";
import { computed, ref, watch } from "vue";
import type { TranscriptMessage } from "../../lib/transcript";
import TranscriptEntry from "./TranscriptEntry.vue";

const props = defineProps<{
  messages: TranscriptMessage[];
  status: ChatStatus;
  /** Why held messages wait, when it is not the usual (shown under them). */
  queuedNote: string | null;
  /** The chat (to refresh after taking a message back). */
  instance: string;
}>();

const PAGE = 150;
const shown = ref(PAGE);
watch(
  () => props.instance,
  () => (shown.value = PAGE),
);
const hidden = computed(() => Math.max(0, props.messages.length - shown.value));
const visible = computed(() => (hidden.value ? props.messages.slice(hidden.value) : props.messages));
</script>

<template>
  <div :data-status="props.status" class="flex w-full flex-1 flex-col gap-5">
    <UButton
      v-if="hidden"
      class="self-center"
      color="neutral"
      variant="soft"
      size="sm"
      icon="i-lucide-chevrons-up"
      :label="`Show ${Math.min(hidden, PAGE)} earlier (${hidden} in all)`"
      @click="shown += PAGE"
    />
    <TranscriptEntry
      v-for="m in visible"
      :key="m.metadata?.key ?? m.id"
      :message="m"
      :busy="props.status !== 'ready'"
      :queued-note="props.queuedNote"
      :instance="props.instance"
    />
    <UChatMessage v-if="props.status === 'submitted'" id="indicator" role="assistant" side="left" variant="naked" :parts="[]">
      <template #content>
        <div class="flex items-center gap-2 py-1 text-sm">
          <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
          <UChatShimmer text="Working…" />
        </div>
      </template>
    </UChatMessage>
  </div>
</template>
