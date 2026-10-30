<script setup lang="ts">
// The transcript of a chat with the kit's chat elements: the user's messages as bubbles on the
// right, agent turns and incoming events on the left, "working…" while the agent works.
import type { ChatStatus } from "ai";
import { call } from "../../api/client";
import { notifyError } from "../../notify";
import { refreshSession } from "../../store";
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
  /** The chat (to refresh after taking a message back). */
  instance: string;
}>();

/** Take back a queued message (the hub refuses once it is delivered). */
async function cancel(m: TranscriptMessage) {
  const id = m.metadata?.delivery;
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
  <UChatMessages
    :messages="props.messages"
    :status="props.status"
    should-auto-scroll
    :should-scroll-to-bottom="props.startAtEnd"
    :ui="{ root: 'gap-5 px-0', viewport: 'hidden' }"
  >
    <!-- The kit places the scroll-down button at 86% of the panel's height, which drifts with the
         zoom level and the composer's height; it sits just above the composer instead (ChatView). -->
    <template #viewport="{ onClick }">
      <Teleport defer to="#chat-composer">
        <UButton
          icon="i-lucide-arrow-down"
          color="neutral"
          variant="outline"
          size="sm"
          aria-label="Scroll to the latest message"
          class="absolute -top-10 left-1/2 -translate-x-1/2 rounded-full bg-default shadow-sm"
          @click="onClick"
        />
      </Teleport>
    </template>
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
        :ui="{
          container: m.role === 'user' ? 'max-w-[88%] sm:max-w-[75%]' : m.role === 'system' ? 'pb-2' : undefined,
          // A queued message keeps its badge and cancel button in view (others show on hover).
          actions: m.metadata?.queued ? '[@media(hover:hover)]:opacity-100' : undefined,
        }"
      >
        <template #content>
          <MessageParts :message="m" :busy="props.status !== 'ready'" />
        </template>
        <template v-if="m.role !== 'system'" #actions>
          <div class="flex items-center gap-1.5 pt-1 text-xs text-dimmed">
            <template v-if="m.metadata?.queued">
              <UBadge color="neutral" variant="subtle" size="sm" icon="i-lucide-clock" label="queued" />
              <span>{{ queuedNote }}</span>
              <UButton
                icon="i-lucide-x"
                size="xs"
                color="neutral"
                variant="ghost"
                aria-label="Take back"
                title="Take back"
                @click="cancel(m)"
              />
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
