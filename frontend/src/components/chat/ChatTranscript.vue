<script setup lang="ts">
// The transcript of a chat with the kit's chat elements: the user's messages as bubbles on the
// right, agent turns and incoming events on the left, "working…" while the agent works.
// Not UChatMessages: ChatView does all the scrolling, and the kit's list deep-watches every
// message on each streamed chunk (and would scroll to the last user message when the agent gets
// busy). Each message is its own component (TranscriptEntry), so a chunk re-renders only the
// message it changes.
import type { ChatStatus } from "ai";
import type { TranscriptMessage } from "../../lib/transcript";
import TranscriptEntry from "./TranscriptEntry.vue";

const props = defineProps<{
  messages: TranscriptMessage[];
  status: ChatStatus;
  /** Why queued messages wait (shown under them). */
  queuedNote: string;
  /** The chat (to refresh after taking a message back). */
  instance: string;
}>();
</script>

<template>
  <div :data-status="props.status" class="flex w-full flex-1 flex-col gap-5">
    <TranscriptEntry
      v-for="m in props.messages"
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
