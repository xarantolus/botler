<script setup lang="ts">
// The System view's timers: what agents (and the user) set to fire later, soonest first, with who
// set it and what it does then. Each can be cancelled.
import { computed, onMounted, ref, shallowRef } from "vue";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import type { Timer } from "../../generated";
import { chatPath } from "../../lib/events";
import { fireLabel } from "../../lib/system";
import { notifyError, notifySuccess } from "../../notify";
import { useNow } from "../../now";
import { state } from "../../store";

const props = defineProps<{ timeZone: string }>();
const now = useNow();
const timers = shallowRef<Timer[] | null>(null);
const busy = ref<string | null>(null);

async function load() {
  try {
    timers.value = await call("list_timers", { body: {} });
  } catch (e) {
    notifyError("Could not load the timers", e);
  }
}
onMounted(() => void load());

const sorted = computed(() => [...(timers.value ?? [])].sort((a, b) => a.fire_at.localeCompare(b.fire_at)));

/** What it does when it fires, and the chat it concerns (linked) if any. */
function does(t: Timer): { verb: string; chat: string | null } {
  if ("resume" in t.target) return { verb: "wakes", chat: t.target.resume };
  if ("spawn" in t.target) return { verb: `starts a new ${t.target.spawn} chat`, chat: null };
  return { verb: `runs ${t.target.run}`, chat: null };
}
/** Who set it: a chat (`agent:<name>`) or the user. */
const setBy = (t: Timer) => t.created_by.replace(/^agent:/, "");

async function cancel(t: Timer) {
  const ok = await confirm({
    title: "Cancel this timer?",
    description: t.prompt ?? "It will not fire.",
    confirmLabel: "Cancel timer",
    color: "warning",
  });
  if (!ok) return;
  busy.value = t.id;
  try {
    const r = await call("cancel_timer", { body: { id: t.id } });
    notifySuccess(r.cancelled ? "Timer cancelled" : "It had already fired", t.prompt ?? "");
    await load();
  } catch (e) {
    notifyError("Could not cancel the timer", e);
  } finally {
    busy.value = null;
  }
}
</script>

<template>
  <section aria-labelledby="sys-timers" class="space-y-3">
    <h2 id="sys-timers" class="flex items-center gap-2 font-semibold text-highlighted">
      <UIcon name="i-lucide-alarm-clock" class="size-5 text-muted" /> Timers
      <span class="text-xs font-normal text-muted">set by agents and you</span>
    </h2>
    <USkeleton v-if="!timers" class="h-12 w-full" />
    <UAlert
      v-else-if="!sorted.length"
      icon="i-lucide-alarm-clock-off"
      title="No timers"
      description="Agents set them to wake themselves or another chat later (set_timer, wait)."
      color="neutral"
      variant="outline"
    />
    <ul v-else class="divide-y divide-default rounded-lg border border-default">
      <li v-for="t in sorted" :key="t.id" class="flex flex-col gap-1 p-3 sm:flex-row sm:items-center sm:gap-4">
        <span class="w-44 shrink-0 text-sm font-medium tabular-nums text-highlighted">{{ fireLabel(t.fire_at, props.timeZone, now) }}</span>
        <span class="min-w-0 flex-1 text-sm">
          <span class="text-default">
            {{ does(t).verb }}
            <ULink v-if="does(t).chat" :to="chatPath(does(t).chat!, state.instances)" class="font-medium text-primary hover:underline">{{ does(t).chat }}</ULink>
          </span>
          <span v-if="t.prompt" class="block truncate text-xs text-muted" :title="t.prompt">{{ t.prompt }}</span>
          <span class="block text-xs text-dimmed">set by {{ setBy(t) }}</span>
        </span>
        <UButton
          size="xs"
          color="neutral"
          variant="ghost"
          icon="i-lucide-x"
          label="Cancel"
          :loading="busy === t.id"
          class="self-start sm:self-center"
          @click="cancel(t)"
        />
      </li>
    </ul>
  </section>
</template>
