<script setup lang="ts">
import { NBadge, NButton, NDrawer, NDrawerContent, NIcon, NTab, NTabs } from "naive-ui";
import { MenuOutline } from "@vicons/ionicons5";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { state } from "../store";
import AppHeader from "./AppHeader.vue";
import ChatList from "./ChatList.vue";

const route = useRoute();
const router = useRouter();

const narrow = ref(false);
const drawer = ref(false);
let mq: MediaQueryList | null = null;
const onMq = () => (narrow.value = mq?.matches ?? false);
onMounted(() => {
  mq = window.matchMedia("(max-width: 860px)");
  onMq();
  mq.addEventListener("change", onMq);
});
onBeforeUnmount(() => mq?.removeEventListener("change", onMq));
watch(
  () => route.fullPath,
  () => (drawer.value = false),
);

const tab = computed(() => (route.name === "home" || route.name === "chat" ? "chat" : String(route.name ?? "chat")));
function go(name: string) {
  if (name === "chat") void router.push(state.session ? `/chat/${encodeURIComponent(state.session.instance.name)}` : "/");
  else void router.push({ name });
}

const openRequests = computed(() => state.dialogs?.open.length ?? 0);
const openIncidents = computed(() => state.status?.open_incidents ?? 0);
</script>

<template>
  <div class="shell">
    <header class="top">
      <AppHeader>
        <template #menu>
          <NButton v-if="narrow" quaternary circle aria-label="Chats" @click="drawer = true">
            <template #icon><NIcon><MenuOutline /></NIcon></template>
          </NButton>
        </template>
      </AppHeader>
    </header>
    <div class="body">
      <aside v-if="!narrow" class="side"><ChatList /></aside>
      <main class="main">
        <div style="padding: 0 12px; flex: none; overflow-x: auto">
          <NTabs :value="tab" type="line" size="small" @update:value="go">
            <NTab name="chat">Chat</NTab>
            <NTab name="events">Events</NTab>
            <NTab name="requests">
              <NBadge :value="openRequests" :max="99" :show="openRequests > 0" :offset="[10, -2]">Requests</NBadge>
            </NTab>
            <NTab name="runs">Runs</NTab>
            <NTab name="incidents">
              <NBadge :value="openIncidents" :show="openIncidents > 0" type="error" :offset="[10, -2]">Incidents</NBadge>
            </NTab>
            <NTab name="proposals">Proposals</NTab>
            <NTab name="secrets">Secrets</NTab>
            <NTab name="memory">Memory</NTab>
          </NTabs>
        </div>
        <div style="flex: 1; min-height: 0; display: flex; flex-direction: column">
          <RouterView />
        </div>
      </main>
    </div>
    <NDrawer v-model:show="drawer" placement="left" :width="300">
      <NDrawerContent title="Chats" body-content-style="padding: 0; height: 100%">
        <ChatList />
      </NDrawerContent>
    </NDrawer>
  </div>
</template>

<style scoped>
.shell {
  display: flex;
  flex-direction: column;
  height: 100vh;
  height: 100dvh;
}
.top {
  flex: none;
  border-bottom: 1px solid rgba(127, 127, 127, 0.22);
}
.body {
  flex: 1;
  min-height: 0;
  display: flex;
}
.side {
  width: 290px;
  flex: none;
  border-right: 1px solid rgba(127, 127, 127, 0.22);
}
.main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
</style>
