<script setup lang="ts">
import { useToast } from "@nuxt/ui/composables/useToast";
import { watch } from "vue";
import AppShell from "./components/shell/AppShell.vue";
import LoginView from "./components/shell/LoginView.vue";
import { startLive, stopLive } from "./live";
import { bindToaster } from "./notify";
import { checkSignedIn, refreshAll, state } from "./store";

bindToaster(useToast());
void checkSignedIn();

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
  <UApp :toaster="{ position: 'bottom-right', duration: 6000 }" :tooltip="{ delayDuration: 250 }">
    <AppShell v-if="state.authed" />
    <LoginView v-else-if="state.authChecked" />
    <UEmpty
      v-else-if="state.unreachable"
      icon="i-lucide-wifi-off"
      title="Cannot reach the hub"
      :description="`${state.unreachable}. Trying again…`"
      variant="naked"
      class="h-full"
    />
  </UApp>
</template>
