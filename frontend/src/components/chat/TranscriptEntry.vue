<script setup lang="ts">
// One message of the transcript. Its own component so a streamed chunk, which changes only the
// last message, re-renders that one and not every message of a long chat.
import { computed } from "vue";
import { call } from "../../api/client";
import { notifyError } from "../../notify";
import { refreshSession } from "../../store";
import type { TranscriptMessage } from "../../lib/transcript";
import { usd } from "../../lib/time";
import TimeAgo from "../common/TimeAgo.vue";
import MessageParts from "./MessageParts.vue";

const props = defineProps<{
  message: TranscriptMessage;
  /** The agent works (tool calls without a result are running). */
  busy: boolean;
  /** Why held messages wait, when it is not the usual (shown under them). */
  queuedNote: string | null;
  /** The chat (to refresh after taking a message back). */
  instance: string;
}>();

/** When a message not in the session yet reaches the agent. */
const waitsFor = computed(() => {
  const q = props.message.metadata?.queued;
  if (!q) return null;
  if (q.handed_over) return "it reads this once its running tool is done";
  return props.queuedNote ?? (q.mode === "queue" ? "delivered when the agent is done" : "delivered when the agent is ready");
});

/** Take back a queued message (the hub refuses once it is delivered). */
async function cancel() {
  const id = props.message.metadata?.delivery;
  if (!id) return;
  try {
    await call("cancel_message", { params: { id } });
  } catch (e) {
    notifyError("Could not take the message back", e);
  }
  await refreshSession(props.instance);
}

function sideProps(m: TranscriptMessage) {
  if (m.role === "user") {
    return { side: "right" as const, variant: (m.metadata?.queued ? "outline" : "soft") as "outline" | "soft" };
  }
  return { side: "left" as const, variant: "naked" as const };
}
</script>

<template>
  <UChatMessage
    v-bind="{ ...message, ...sideProps(message) }"
    :data-delivery="message.metadata?.delivery ?? undefined"
    :data-message="message.id"
    :data-role="message.role"
    :class="message.metadata?.queued && 'opacity-70'"
    :ui="{
      container: message.role === 'user' ? 'max-w-[88%] sm:max-w-[75%]' : message.role === 'system' ? 'pb-2' : undefined,
      // A queued message keeps its badge and cancel button in view (others show on hover).
      actions: message.metadata?.queued ? '[@media(hover:hover)]:opacity-100' : undefined,
    }"
  >
    <template #content>
      <MessageParts :message="message" :busy="busy" />
    </template>
    <template v-if="message.role !== 'system'" #actions>
      <div class="flex items-center gap-1.5 pt-1 text-xs text-dimmed">
        <template v-if="message.metadata?.queued">
          <UBadge v-if="message.metadata.queued.handed_over" color="neutral" variant="subtle" size="sm" icon="i-lucide-send" label="sent" />
          <UBadge v-else color="neutral" variant="subtle" size="sm" icon="i-lucide-clock" label="queued" />
          <span>{{ waitsFor }}</span>
          <UButton
            v-if="!message.metadata.queued.handed_over"
            icon="i-lucide-x"
            size="xs"
            color="neutral"
            variant="ghost"
            aria-label="Take back"
            title="Take back"
            @click="cancel"
          />
        </template>
        <template v-else-if="message.role === 'user'">
          <span>you</span>
          <span v-if="message.metadata?.localTime" :title="message.metadata.at ?? undefined">· {{ message.metadata.localTime }}</span>
          <span v-else>· <TimeAgo :at="message.metadata?.at" /></span>
          <span v-if="message.metadata?.gap">· {{ message.metadata.gap }}</span>
        </template>
        <template v-else-if="message.role === 'assistant' && !message.metadata?.live">
          <UIcon name="i-lucide-bot" class="size-3.5" />
          <span v-if="message.metadata?.model">{{ message.metadata.model }}</span>
          <span v-if="message.metadata?.costUsd">· {{ usd(message.metadata.costUsd, 4) }}</span>
          <span>· <TimeAgo :at="message.metadata?.at" /></span>
        </template>
      </div>
    </template>
  </UChatMessage>
</template>

