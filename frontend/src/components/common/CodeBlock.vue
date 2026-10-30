<script setup lang="ts">
// Preformatted text (tool arguments and results, JSON payloads, logs), with a copy button.
import { ref } from "vue";
import { notifyError } from "../../notify";

const props = defineProps<{ text: string; label?: string; error?: boolean; maxHeight?: string }>();
const copied = ref(false);

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text);
    copied.value = true;
    window.setTimeout(() => (copied.value = false), 1500);
  } catch (e) {
    notifyError("Could not copy", e);
  }
}
</script>

<template>
  <div class="group/code relative min-w-0">
    <div v-if="label" class="mb-1 text-xs font-medium text-muted">{{ label }}</div>
    <pre
      class="overflow-auto rounded-md border px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
      :class="error ? 'border-error/30 bg-error/5 text-error' : 'border-muted bg-muted text-default'"
      :style="{ maxHeight: maxHeight ?? '24rem' }"
    >{{ text || "(empty)" }}</pre>
    <UButton
      v-if="text"
      :icon="copied ? 'i-lucide-copy-check' : 'i-lucide-copy'"
      :aria-label="copied ? 'Copied' : 'Copy'"
      size="xs"
      color="neutral"
      variant="ghost"
      class="absolute end-1 opacity-0 group-hover/code:opacity-100 focus-visible:opacity-100"
      :class="label ? 'top-6' : 'top-1'"
      @click="copy"
    />
  </div>
</template>
