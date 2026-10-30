<script setup lang="ts">
// A note: something delivered for the agent's information only (it did not start a turn).
import type { NoteData } from "../../lib/transcript";
import MarkdownText from "../common/MarkdownText.vue";
import TimeAgo from "../common/TimeAgo.vue";

defineProps<{ note: NoteData }>();
</script>

<template>
  <!-- The user stopped the chat: one quiet line. -->
  <p v-if="note.kind === 'chat.stopped'" class="flex items-center gap-1.5 text-xs text-dimmed">
    <UIcon name="i-lucide-octagon-x" class="size-3.5" />
    <span>{{ note.text }}</span>
    <span>· <template v-if="note.localTime">{{ note.localTime }}</template><TimeAgo v-else :at="note.at" /></span>
  </p>
  <aside v-else class="border-s-2 border-dashed border-accented ps-3 text-sm text-muted" :aria-label="`Note from ${note.from}`">
    <div class="mb-0.5 flex flex-wrap items-center gap-1.5 text-xs text-dimmed">
      <UIcon name="i-lucide-sticky-note" class="size-3.5" />
      <span>note from <strong class="font-medium text-muted">{{ note.from }}</strong></span>
      <span v-if="note.kind !== 'message'">· {{ note.kind }}</span>
      <span>· <template v-if="note.localTime">{{ note.localTime }}</template><TimeAgo v-else :at="note.at" /></span>
    </div>
    <MarkdownText :source="note.text" />
  </aside>
</template>
