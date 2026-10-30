<script setup lang="ts">
// The theme: system → light → dark → system. The choice is stored per browser as Nuxt UI's
// color mode stores it (VueUse's `vueuse-color-scheme`: auto, light or dark), so "system" stays
// system and follows the OS live; index.html applies it before the first paint.
import { useColorMode } from "@vueuse/core";
import { computed } from "vue";

const { store } = useColorMode();
const MODES = {
  auto: { label: "system", icon: "i-lucide-monitor", next: "light" },
  light: { label: "light", icon: "i-lucide-sun", next: "dark" },
  dark: { label: "dark", icon: "i-lucide-moon", next: "auto" },
} as const;
const mode = computed(() => MODES[store.value in MODES ? (store.value as keyof typeof MODES) : "auto"]);
</script>

<template>
  <UTooltip :text="`Theme: ${mode.label}`">
    <UButton
      :icon="mode.icon"
      color="neutral"
      variant="ghost"
      size="sm"
      :aria-label="`Theme: ${mode.label} (switch to ${MODES[mode.next].label})`"
      @click="store = mode.next"
    />
  </UTooltip>
</template>
