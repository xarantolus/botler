<script setup lang="ts">
// The transcript of a chat with the kit's chat elements: the user's messages as bubbles on the
// right, agent turns and incoming events on the left, "working…" while the agent works.
// Not UChatMessages: ChatView does all the scrolling, and the kit's list deep-watches every
// message on each streamed chunk (and would scroll to the last user message when the agent gets
// busy). While pi compacts the conversation, a line says so and for how long. Each message is its
// own component (TranscriptEntry), so a chunk re-renders only the message it changes.
// Virtual: only the messages near the viewport are in the page (the panel body scrolls them), each
// measured once rendered. The hub sends the latest part of a long chat; scrolling up to its top
// loads the page before it ("Show earlier" does the same by hand), keeping what is in view in place.
import { useVirtualizer } from "@tanstack/vue-virtual";
import type { ChatStatus } from "ai";
import { useIntersectionObserver } from "@vueuse/core";
import { computed, nextTick, onBeforeUnmount, ref, useTemplateRef, watch } from "vue";
import { elapsed } from "../../lib/time";
import type { TranscriptMessage } from "../../lib/transcript";
import { showEarlier, state } from "../../store";
import TranscriptEntry from "./TranscriptEntry.vue";

const props = defineProps<{
  messages: TranscriptMessage[];
  status: ChatStatus;
  /** Why held messages wait, when it is not the usual (shown under them). */
  queuedNote: string | null;
  /** The chat (to refresh after taking a message back). */
  instance: string;
  /** Session items before these, not loaded. */
  earlier: number;
  /** The view follows the end (ChatView): nothing loads above, and the end stays in place. */
  following: boolean;
}>();

const list = useTemplateRef<HTMLElement>("list");
/** Space between messages, px. */
const GAP = 20;
const virtualizer = useVirtualizer(
  computed(() => ({
    count: props.messages.length,
    getScrollElement: () => list.value?.closest<HTMLElement>("[data-slot=body]") ?? null,
    estimateSize: () => 120,
    gap: GAP,
    overscan: 6,
    // Where the list starts within the scrolling panel (the handover note and such sit above).
    scrollMargin: list.value?.offsetTop ?? 0,
    getItemKey: (i: number) => props.messages[i]?.metadata?.key ?? props.messages[i]?.id ?? i,
  })),
);
const rows = computed(() => virtualizer.value.getVirtualItems());
function measure(el: unknown) {
  if (el instanceof Element) virtualizer.value.measureElement(el);
}

/** Scroll message `id` into the page (it may not be rendered yet); false if it is not loaded. */
function scrollToMessage(id: string): boolean {
  const i = props.messages.findIndex((m) => m.id === id || m.metadata?.delivery === id);
  if (i < 0) return false;
  virtualizer.value.scrollToIndex(i, { align: "center" });
  return true;
}
defineExpose({ scrollToMessage });

/** Loading the page before the first message (one at a time). */
const loadingEarlier = ref(false);
async function loadEarlier() {
  if (loadingEarlier.value || !props.earlier) return;
  loadingEarlier.value = true;
  try {
    await showEarlier(props.instance);
  } finally {
    loadingEarlier.value = false;
  }
}
// Messages added above: the one that was first stays where it was (at the end, the end stays).
watch(
  () => props.messages[0]?.metadata?.key ?? props.messages[0]?.id,
  (_, before) => {
    if (props.following) return;
    const i = props.messages.findIndex((m) => (m.metadata?.key ?? m.id) === before);
    if (i > 0) void nextTick(() => virtualizer.value.scrollToIndex(i, { align: "start" }));
  },
);
const top = useTemplateRef<HTMLElement>("top");
// Only once the user scrolled up: a chat opening at its end has the top in view for a moment.
const topInView = ref(false);
useIntersectionObserver(top, ([entry]) => (topInView.value = !!entry?.isIntersecting));
watch(
  () => topInView.value && !props.following,
  (load) => load && void loadEarlier(),
);

const now = ref(Date.now());
const timer = window.setInterval(() => (now.value = Date.now()), 1000);
onBeforeUnmount(() => window.clearInterval(timer));
const compacting = computed(() => {
  const c = state.compacting;
  if (!c) return null;
  const why = c.reason === "overflow" ? "it no longer fit the model's context" : c.reason === "manual" ? "asked to" : "it got long";
  return `Compacting the conversation (${why})… ${elapsed(now.value - c.since)}`;
});
</script>

<template>
  <div :data-status="props.status" class="flex w-full flex-1 flex-col gap-5">
    <div v-if="props.earlier" ref="top" class="self-center">
      <UButton
        :loading="loadingEarlier"
        color="neutral"
        variant="soft"
        size="sm"
        icon="i-lucide-chevrons-up"
        :label="`Show earlier (${props.earlier} more)`"
        @click="loadEarlier"
      />
    </div>
    <div ref="list" class="relative w-full" :style="{ height: `${virtualizer.getTotalSize()}px` }">
      <div
        v-for="row in rows"
        :key="String(row.key)"
        :ref="measure"
        :data-index="row.index"
        class="absolute inset-x-0 top-0"
        :style="{ transform: `translateY(${row.start - virtualizer.options.scrollMargin}px)` }"
      >
        <TranscriptEntry
          :message="props.messages[row.index]!"
          :busy="props.status !== 'ready'"
          :queued-note="props.queuedNote"
          :instance="props.instance"
        />
      </div>
    </div>
    <div v-if="compacting" class="flex items-center gap-2 py-1 text-sm" role="status">
      <UIcon name="i-lucide-fold-vertical" class="size-4 animate-pulse text-muted" />
      <UChatShimmer :text="compacting" />
    </div>
    <UChatMessage v-else-if="props.status === 'submitted'" id="indicator" role="assistant" side="left" variant="naked" :parts="[]">
      <template #content>
        <div class="flex items-center gap-2 py-1 text-sm">
          <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
          <UChatShimmer text="Working…" />
        </div>
      </template>
    </UChatMessage>
  </div>
</template>
