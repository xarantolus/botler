<script setup lang="ts">
import { darkTheme, NConfigProvider, NGlobalStyle, useOsTheme } from "naive-ui";
import { computed, watch } from "vue";
import AppShell from "./components/AppShell.vue";
import LoginView from "./components/LoginView.vue";
import { startLive, stopLive } from "./live";
import { dark } from "./notify";
import { refreshAll, state } from "./store";

const os = useOsTheme();
const theme = computed(() => (os.value === "dark" ? darkTheme : null));
watch(os, (v) => (dark.value = v === "dark"), { immediate: true });

watch(
  () => state.authed,
  (authed) => {
    if (authed) {
      startLive();
      void refreshAll();
    } else {
      stopLive();
    }
  },
  { immediate: true },
);
</script>

<template>
  <NConfigProvider :theme="theme">
    <NGlobalStyle />
    <AppShell v-if="state.authed" />
    <LoginView v-else />
  </NConfigProvider>
</template>
