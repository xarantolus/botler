<script setup lang="ts">
import { computed } from "vue";
import { renderInline, renderMarkdown } from "../lib/markdown";

const props = defineProps<{ source: string; inline?: boolean }>();
const html = computed(() => (props.inline ? renderInline(props.source) : renderMarkdown(props.source)));
</script>

<template>
  <!-- Sanitized by DOMPurify in renderMarkdown. -->
  <span v-if="inline" class="md" v-html="html" />
  <div v-else class="md" v-html="html" />
</template>
