<script setup lang="ts">
import { NButton, NInput, NPopover, NSpace, NTag, NText, NTooltip } from "naive-ui";
import { computed, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import { dialog, notifyError } from "../notify";
import { logout, refreshStatus, state } from "../store";
import { usd } from "../lib/time";
import BreakerResume from "./BreakerResume.vue";

const toggling = ref(false);

const route = useRoute();
const router = useRouter();
const query = ref("");
watch(
  () => route.query.q,
  (q) => {
    if (route.name === "search" && typeof q === "string") query.value = q;
  },
  { immediate: true },
);
function search() {
  const q = query.value.trim();
  if (q) void router.push({ name: "search", query: { ...(route.name === "search" ? route.query : {}), q } });
}
const s = computed(() => state.status);

const spend = computed(() => {
  const u = state.usage;
  if (!u) return null;
  return { today: u.today.cost_usd, tokens: u.today.tokens, budget: u.default_budget.usd_per_day, tz: u.timezone };
});

function togglePause() {
  const paused = s.value?.paused ?? false;
  dialog[paused ? "info" : "warning"]({
    title: paused ? "Resume botler?" : "Pause everything?",
    content: paused
      ? "Schedules, dispatch and services start again."
      : "This stops all agents and services and freezes schedules and dispatch until you resume.",
    positiveText: paused ? "Resume" : "Pause",
    negativeText: "Cancel",
    onPositiveClick: async () => {
      toggling.value = true;
      try {
        await call(paused ? "resume" : "pause", {});
        await refreshStatus();
      } catch (e) {
        notifyError(paused ? "Resume failed" : "Pause failed", e);
      } finally {
        toggling.value = false;
      }
    },
  });
}
</script>

<template>
  <div style="display: flex; align-items: center; gap: 12px; padding: 10px 14px; min-height: 36px; flex-wrap: wrap">
    <slot name="menu" />
    <strong style="font-size: 17px">botler</strong>
    <NSpace v-if="s" size="small" align="center" :wrap="true" style="flex: 1">
      <NTooltip>
        <template #trigger>
          <NTag size="small" :type="state.connection === 'live' ? 'success' : state.connection === 'polling' ? 'warning' : 'default'" round>
            {{ state.connection === "live" ? "live" : state.connection === "polling" ? "polling" : "connecting" }}
          </NTag>
        </template>
        {{
          state.connection === "live"
            ? "Changes stream in over the live connection."
            : "The live connection is down; refreshing every few seconds while reconnecting."
        }}
      </NTooltip>
      <NTag v-if="s.paused" size="small" type="warning">paused</NTag>
      <NPopover v-if="s.breakers.length" trigger="hover">
        <template #trigger>
          <NTag size="small" type="error">{{ s.breakers.length }} breaker(s)</NTag>
        </template>
        <div v-for="b in s.breakers" :key="b.subject" style="max-width: 460px; display: flex; gap: 8px; align-items: center; margin: 4px 0">
          <span style="flex: 1"><strong>{{ b.subject }}</strong> ({{ b.kind }}): {{ b.reason }}</span>
          <BreakerResume :subject="b.subject" size="tiny" />
        </div>
      </NPopover>
      <NTooltip v-if="spend">
        <template #trigger>
          <NText depth="2" style="font-size: 13px">
            today {{ usd(spend.today)
            }}<span v-if="spend.budget !== null" class="wide-only"> · budget {{ usd(spend.budget) }}/agent</span>
          </NText>
        </template>
        {{ spend.tokens.toLocaleString() }} tokens since midnight ({{ spend.tz }}).
        <template v-if="spend.budget !== null">Agents without their own budget stop at {{ usd(spend.budget) }} a day.</template>
      </NTooltip>
      <NText class="wide-only" depth="3" style="font-size: 12px">v{{ s.version }} · {{ s.commit.slice(0, 10) }} · {{ s.runtime.binary }} {{ s.runtime.version }}</NText>
    </NSpace>
    <div v-else style="flex: 1" />
    <NInput
      v-model:value="query"
      size="small"
      clearable
      placeholder="Search everything"
      style="width: 220px; max-width: 100%"
      @keydown.enter="search"
    />
    <NSpace size="small">
      <NButton size="small" :type="s?.paused ? 'primary' : 'warning'" ghost :loading="toggling" :disabled="!s" @click="togglePause">
        {{ s?.paused ? "Resume" : "Pause" }}
      </NButton>
      <NButton size="small" quaternary @click="logout">Log out</NButton>
    </NSpace>
  </div>
</template>

<style scoped>
@media (max-width: 640px) {
  .wide-only {
    display: none;
  }
}
</style>
