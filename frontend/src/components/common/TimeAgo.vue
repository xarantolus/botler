<script setup lang="ts">
// A relative time ("3 min ago") with the exact time on hover; re-renders every half minute.
import { computed } from "vue";
import { ago } from "../../lib/time";
import { useNow } from "../../now";

const props = defineProps<{ at: string | null | undefined }>();
const now = useNow();
const text = computed(() => ago(props.at, now.value));
const exact = computed(() => (props.at ? new Date(props.at).toLocaleString() : undefined));
</script>

<template>
  <time v-if="at" :datetime="at" :title="exact">{{ text }}</time>
</template>
