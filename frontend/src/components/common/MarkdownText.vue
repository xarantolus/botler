<script setup lang="ts">
// Markdown from agents and other untrusted sources, rendered and sanitized (lib/markdown.ts).
import { computed } from "vue";
import { renderInline, renderMarkdown } from "../../lib/markdown";

const props = defineProps<{ source: string; inline?: boolean }>();
const html = computed(() => (props.inline ? renderInline(props.source) : renderMarkdown(props.source)));
</script>

<template>
  <!-- eslint-disable-next-line vue/no-v-html -- sanitized by DOMPurify in lib/markdown.ts -->
  <span v-if="inline" class="md md-inline" v-html="html" />
  <!-- eslint-disable-next-line vue/no-v-html -- sanitized by DOMPurify in lib/markdown.ts -->
  <div v-else class="md" v-html="html" />
</template>
