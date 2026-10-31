<script setup lang="ts">
// Markdown from agents and other untrusted sources, rendered and sanitized (lib/markdown.ts).
// Mentions of chats, proposals, jobs, … link to their page (lib/references.ts), inside the app.
import { computed } from "vue";
import { useRouter } from "vue-router";
import { renderInline, renderMarkdown } from "../../lib/markdown";
import { refTarget } from "../../lib/references";
import { knownReferences } from "../../store";

const props = defineProps<{ source: string; inline?: boolean }>();
const router = useRouter();
const html = computed(() =>
  props.inline ? renderInline(props.source, knownReferences.value) : renderMarkdown(props.source, knownReferences.value),
);

function onClick(e: MouseEvent) {
  const to = refTarget(e);
  if (!to) return;
  e.preventDefault();
  void router.push(to);
}
</script>

<template>
  <!-- eslint-disable-next-line vue/no-v-html -- sanitized by DOMPurify in lib/markdown.ts -->
  <span v-if="inline" class="md md-inline" @click="onClick" v-html="html" />
  <!-- eslint-disable-next-line vue/no-v-html -- sanitized by DOMPurify in lib/markdown.ts -->
  <div v-else class="md" @click="onClick" v-html="html" />
</template>
