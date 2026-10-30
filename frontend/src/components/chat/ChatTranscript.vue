<script setup lang="ts">
// The transcript of a chat with the kit's chat elements: the user's messages as bubbles on the
// right, agent turns and incoming events on the left, "working…" while the agent works.
import type { ChatStatus } from "ai";
import type { TranscriptMessage } from "../../lib/transcript";
import { usd } from "../../lib/time";
import TimeAgo from "../common/TimeAgo.vue";
import MessageParts from "./MessageParts.vue";

const props = defineProps<{
  messages: TranscriptMessage[];
  status: ChatStatus;
  /** Why queued messages wait (shown under them). */
  queuedNote: string;
  /** Start at the end (not when a link points at a message further up). */
  startAtEnd: boolean;
}>();

function sideProps(m: TranscriptMessage) {
  if (m.role === "user") {
    return { side: "right" as const, variant: (m.metadata?.queued ? "outline" : "soft") as "outline" | "soft" };
  }
  return { side: "left" as const, variant: "naked" as const };
}
</script>

<template>
  <UChatMessages
    :messages="props.messages"
    :status="props.status"
    should-auto-scroll
    :should-scroll-to-bottom="props.startAtEnd"
    :auto-scroll="{ color: 'neutral', variant: 'outline', size: 'sm' }"
    :ui="{ root: 'gap-5 px-0' }"
  >
    <template #default="{ registerMessageRef }">
      <UChatMessage
        v-for="m in props.messages"
        :key="m.id"
        :ref="(el) => registerMessageRef(m.id, el as never)"
        v-bind="{ ...m, ...sideProps(m) }"
        :data-delivery="m.metadata?.delivery ?? undefined"
        :data-message="m.id"
        :data-role="m.role"
        :class="m.metadata?.queued && 'opacity-70'"
        :ui="{ container: m.role === 'user' ? 'max-w-[88%] sm:max-w-[75%]' : m.role === 'system' ? 'pb-2' : undefined }"
      >
        <template #content>
          <MessageParts :message="m" :busy="props.status !== 'ready'" />
        </template>
        <template v-if="m.role !== 'system'" #actions>
          <div class="flex items-center gap-1.5 pt-1 text-xs text-dimmed">
            <template v-if="m.metadata?.queued">
              <UIcon name="i-lucide-clock" class="size-3.5" />
              <span>Queued: {{ queuedNote }}</span>
            </template>
            <template v-else-if="m.role === 'user'">
              <span>you</span>
              <span v-if="m.metadata?.localTime" :title="m.metadata.at ?? undefined">· {{ m.metadata.localTime }}</span>
              <span v-else>· <TimeAgo :at="m.metadata?.at" /></span>
              <span v-if="m.metadata?.gap">· {{ m.metadata.gap }}</span>
            </template>
            <template v-else-if="m.role === 'assistant' && !m.metadata?.live">
              <UIcon name="i-lucide-bot" class="size-3.5" />
              <span v-if="m.metadata?.model">{{ m.metadata.model }}</span>
              <span v-if="m.metadata?.costUsd">· {{ usd(m.metadata.costUsd, 4) }}</span>
              <span>· <TimeAgo :at="m.metadata?.at" /></span>
            </template>
          </div>
        </template>
      </UChatMessage>
    </template>
    <template #indicator>
      <div class="flex items-center gap-2 py-1 text-sm">
        <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
        <UChatShimmer text="Working…" />
      </div>
    </template>
  </UChatMessages>
</template>
