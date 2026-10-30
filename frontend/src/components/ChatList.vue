<script setup lang="ts">
import { NButton, NEmpty, NInput, NModal, NScrollbar, NSpace, NSwitch, NText } from "naive-ui";
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../api/client";
import type { Instance } from "../generated";
import { chatTitle } from "../lib/events";
import { phaseBadge } from "../lib/phase";
import { ago, usd } from "../lib/time";
import { notifyError } from "../notify";
import { frontDoorName, refreshInstances, state, waitingByInstance } from "../store";
import PhaseTag from "./PhaseTag.vue";

const router = useRouter();
const selected = computed(() => state.session?.instance.name ?? null);

const chats = computed(() => {
  const door = frontDoorName.value;
  return state.instances
    .filter((i) => i.state !== "archived" || state.showArchived || i.name === selected.value)
    .sort((a, b) => {
      if (a.name === door) return -1;
      if (b.name === door) return 1;
      // Chats waiting for the user come first, then the most recently active.
      const wa = waitingByInstance.value[a.name] ? 1 : 0;
      const wb = waitingByInstance.value[b.name] ? 1 : 0;
      return wb - wa || b.last_active.localeCompare(a.last_active);
    });
});

function badge(i: Instance) {
  return phaseBadge({
    state: i.state,
    status: state.liveStatus[i.name] ?? null,
    waitingOnUser: waitingByInstance.value[i.name],
    archiveRequested: i.archive_requested_at !== null,
  });
}

function cost(i: Instance): string {
  const c = state.usage?.total_by_instance[i.name]?.cost_usd;
  return c ? usd(c) : "";
}

const creating = ref(false);
const firstMessage = ref("");
const definition = ref("");
const busy = ref(false);

async function create() {
  const text = firstMessage.value.trim();
  if (!text) return;
  busy.value = true;
  try {
    const r = await call("new_chat", { body: { text, definition: definition.value.trim() || undefined } });
    creating.value = false;
    firstMessage.value = "";
    definition.value = "";
    await refreshInstances();
    await router.push(`/chat/${encodeURIComponent(r.instance)}`);
  } catch (e) {
    notifyError("Could not start the chat", e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div style="display: flex; flex-direction: column; height: 100%">
    <div style="padding: 10px 10px 6px">
      <NButton type="primary" block @click="creating = true">New chat</NButton>
    </div>
    <NScrollbar style="flex: 1; min-height: 0">
      <div style="padding: 4px 6px">
        <NEmpty v-if="!chats.length" description="No chats yet." style="margin-top: 30px" />
        <RouterLink
          v-for="i in chats"
          :key="i.name"
          :to="`/chat/${encodeURIComponent(i.name)}`"
          class="chat-item"
          :class="{ selected: i.name === selected }"
        >
          <div style="display: flex; align-items: center; gap: 6px">
            <span class="chat-title" :title="i.name">
              <template v-if="i.name === frontDoorName">&#9733; </template>{{ chatTitle(i) }}
            </span>
            <PhaseTag :badge="badge(i)" />
          </div>
          <div class="chat-sub">
            <span>{{ i.definition }}</span>
            <span v-if="cost(i)"> · {{ cost(i) }}</span>
            <span> · {{ ago(i.last_active) }}</span>
          </div>
        </RouterLink>
      </div>
    </NScrollbar>
    <div style="padding: 8px 12px; border-top: 1px solid rgba(127, 127, 127, 0.2)">
      <NSpace align="center" size="small">
        <NSwitch v-model:value="state.showArchived" size="small" />
        <NText depth="3" style="font-size: 12px">show archived</NText>
      </NSpace>
    </div>
    <NModal v-model:show="creating" preset="card" title="New chat" style="max-width: 520px; width: calc(100vw - 32px)">
      <NSpace vertical>
        <NInput
          v-model:value="firstMessage"
          type="textarea"
          :autosize="{ minRows: 3, maxRows: 10 }"
          placeholder="What is this chat about? (your first message)"
          @keydown.enter.exact.prevent="create"
        />
        <NInput v-model:value="definition" placeholder="agent definition (default: chat)" />
        <NButton type="primary" :loading="busy" :disabled="!firstMessage.trim()" @click="create">Start</NButton>
      </NSpace>
    </NModal>
  </div>
</template>

<style scoped>
.chat-item {
  display: block;
  padding: 8px 10px;
  border-radius: 6px;
  color: inherit;
  text-decoration: none;
}
.chat-item:hover {
  background: rgba(127, 127, 127, 0.1);
}
.chat-item.selected {
  background: rgba(47, 91, 211, 0.14);
}
.chat-title {
  flex: 1;
  min-width: 0;
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.chat-sub {
  font-size: 12px;
  opacity: 0.65;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
