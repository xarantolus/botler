<script setup lang="ts">
// What a chat is doing (lib/phase.ts), as a badge; the longer explanation on hover.
import { computed } from "vue";
import { badgeColor, type Badge } from "../../lib/phase";

const props = defineProps<{ badge: Badge; size?: "sm" | "md" }>();
const color = computed(() => badgeColor(props.badge.type));
</script>

<template>
  <UTooltip :text="badge.detail" :disabled="!badge.detail">
    <UBadge
      :color="color"
      variant="subtle"
      :size="size ?? 'sm'"
      :icon="badge.busy ? 'i-lucide-loader-circle' : undefined"
      :ui="{ leadingIcon: 'animate-spin' }"
      :label="badge.label"
      class="shrink-0 rounded-full"
    />
  </UTooltip>
</template>
