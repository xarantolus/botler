<script setup lang="ts">
import { NSpin, NTag, NTooltip } from "naive-ui";
import type { Badge } from "../lib/phase";

defineProps<{ badge: Badge; size?: "small" | "medium" }>();
</script>

<template>
  <NTooltip :disabled="!badge.detail">
    <template #trigger>
      <NTag :type="badge.type" :size="size ?? 'small'" round :bordered="false">
        <span style="display: inline-flex; align-items: center; gap: 4px">
          <NSpin v-if="badge.busy" :size="10" :stroke-width="22" />
          {{ badge.label }}
        </span>
      </NTag>
    </template>
    {{ badge.detail }}
  </NTooltip>
</template>
