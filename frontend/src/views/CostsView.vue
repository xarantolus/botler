<script setup lang="ts">
// What models cost over a span of days: per day as bars (a bar opens its day), per chat and per
// model. Agents' costs are pi's estimates at each model's listed price; OpenRouter's own figures
// for the key are on the Settings page.
import type { TableColumn } from "@nuxt/ui";
import { computed, ref, shallowRef, watch } from "vue";
import { call } from "../api/client";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { ChatCost, Costs, ModelCost } from "../generated";
import { PRESETS, type Preset, dayLabel, presetRange, todayIn, tokens } from "../lib/costs";
import { useQueryParam } from "../lib/queryState";
import { usd } from "../lib/time";
import { notifyError } from "../notify";
import { state } from "../store";

const preset = useQueryParam("range", "30d");
const from = useQueryParam("from", "");
const to = useQueryParam("to", "");

const timezone = computed(() => state.usage?.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone);
const span = computed(() =>
  from.value && to.value ? { from: from.value, to: to.value } : presetRange(preset.value as Preset, todayIn(timezone.value)),
);

const costs = shallowRef<Costs | null>(null);
const loading = ref(false);
let seq = 0;
async function load() {
  const n = ++seq;
  loading.value = true;
  try {
    const c = await call("costs", { query: span.value });
    if (n === seq) costs.value = c;
  } catch (e) {
    notifyError("Could not load the costs", e);
  } finally {
    if (n === seq) loading.value = false;
  }
}
watch(span, load, { immediate: true, deep: true });

function choose(p: Preset) {
  from.value = "";
  to.value = "";
  preset.value = p;
}
/** A custom span; one day when a bar is picked. */
function custom(f: string, t: string) {
  if (f && t && f <= t) {
    from.value = f;
    to.value = t;
  }
}

const max = computed(() => Math.max(0, ...(costs.value?.days ?? []).map((d) => d.totals.cost_usd)));
const days = computed(() => costs.value?.days ?? []);

const chatColumns: TableColumn<ChatCost>[] = [
  // Long titles wrap, so the figures stay in view on a phone.
  { accessorKey: "instance", header: "Chat or job", meta: { class: { td: "whitespace-normal break-words min-w-36" } } },
  { id: "cost", header: "Cost" },
  { id: "calls", header: "Calls" },
  { id: "tokens", header: "Tokens" },
];
const modelColumns: TableColumn<ModelCost>[] = [
  { accessorKey: "model", header: "Model", meta: { class: { td: "whitespace-normal break-all min-w-36" } } },
  { id: "cost", header: "Cost" },
  { id: "calls", header: "Calls" },
  { id: "tokens", header: "Tokens" },
];
</script>

<template>
  <ViewPanel id="costs" title="Costs" icon="i-lucide-receipt">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <template #toolbar>
      <div class="flex flex-wrap items-center gap-2 py-1">
        <UFieldGroup size="sm">
          <UButton
            v-for="p in PRESETS"
            :key="p.value"
            :label="p.label"
            color="neutral"
            :variant="!from && preset === p.value ? 'solid' : 'outline'"
            @click="choose(p.value)"
          />
        </UFieldGroup>
        <div class="flex items-center gap-1 text-sm text-muted">
          <UInput type="date" size="sm" :model-value="span.from" aria-label="From" @update:model-value="(v: string) => custom(v, span.to)" />
          <span>to</span>
          <UInput type="date" size="sm" :model-value="span.to" aria-label="To" @update:model-value="(v: string) => custom(span.from, v)" />
        </div>
      </div>
    </template>

    <div v-if="costs" class="mx-auto w-full max-w-4xl space-y-8">
      <div>
        <p class="text-3xl font-semibold tabular-nums text-highlighted">{{ usd(costs.total.cost_usd) }}</p>
        <p class="text-sm text-muted">
          {{ costs.total.calls }} model calls · {{ tokens(costs.total.tokens) }} tokens · {{ dayLabel(costs.from) }} to {{ dayLabel(costs.to) }}
          ({{ costs.timezone }})
        </p>
        <p class="mt-1 text-xs text-dimmed">
          Agents' costs are estimates at each model's listed price; OpenRouter's own figures for the key are under
          <ULink :to="{ name: 'settings' }" class="underline">Settings</ULink>.
        </p>
      </div>

      <section aria-label="Cost per day">
        <div class="relative h-44 border-b border-default">
          <div v-if="max > 0" class="pointer-events-none absolute inset-x-0 top-0 border-t border-dashed border-default">
            <span class="absolute -top-5 right-0 text-xs tabular-nums text-dimmed">{{ usd(max) }}</span>
          </div>
          <div class="flex h-full items-end" :class="days.length > 60 ? 'gap-px' : 'gap-0.5'">
            <UTooltip
              v-for="d in days"
              :key="d.day"
              :text="`${dayLabel(d.day)}: ${usd(d.totals.cost_usd)} · ${d.totals.calls} calls · ${tokens(d.totals.tokens)} tokens`"
            >
              <button
                type="button"
                class="group flex h-full min-w-0 flex-1 items-end"
                :aria-label="`${dayLabel(d.day)}: ${usd(d.totals.cost_usd)}`"
                @click="custom(d.day, d.day)"
              >
                <span
                  class="w-full rounded-t bg-primary group-hover:bg-primary/75"
                  :style="{ height: max > 0 ? `${(d.totals.cost_usd / max) * 100}%` : '0' }"
                />
              </button>
            </UTooltip>
          </div>
        </div>
        <div v-if="days.length > 1" class="mt-1 flex justify-between text-xs text-dimmed">
          <span>{{ dayLabel(days[0]!.day) }}</span>
          <span>{{ dayLabel(days[days.length - 1]!.day) }}</span>
        </div>
      </section>

      <section class="space-y-2">
        <h2 class="text-sm font-medium text-highlighted">Per chat</h2>
        <UTable :data="costs.chats" :columns="chatColumns" :ui="{ td: 'py-2' }">
          <template #instance-cell="{ row }">
            <ULink
              v-if="row.original.definition"
              :to="{ name: 'chat', params: { instance: row.original.instance } }"
              class="font-medium text-highlighted hover:underline"
            >
              {{ row.original.title ?? row.original.instance }}
            </ULink>
            <span v-else class="font-medium text-highlighted">{{ row.original.instance }}</span>
            <p v-if="row.original.title" class="text-xs text-muted">{{ row.original.instance }}</p>
          </template>
          <template #cost-cell="{ row }"><span class="tabular-nums">{{ usd(row.original.totals.cost_usd) }}</span></template>
          <template #calls-cell="{ row }"><span class="tabular-nums">{{ row.original.totals.calls }}</span></template>
          <template #tokens-cell="{ row }"><span class="tabular-nums">{{ tokens(row.original.totals.tokens) }}</span></template>
          <template #empty><UEmpty icon="i-lucide-receipt" title="No model calls" description="Nothing ran in these days." variant="naked" /></template>
        </UTable>
      </section>

      <section v-if="costs.models.length" class="space-y-2">
        <h2 class="text-sm font-medium text-highlighted">Per model</h2>
        <UTable :data="costs.models" :columns="modelColumns" :ui="{ td: 'py-2' }">
          <template #model-cell="{ row }"><code class="font-mono text-xs">{{ row.original.model }}</code></template>
          <template #cost-cell="{ row }"><span class="tabular-nums">{{ usd(row.original.totals.cost_usd) }}</span></template>
          <template #calls-cell="{ row }"><span class="tabular-nums">{{ row.original.totals.calls }}</span></template>
          <template #tokens-cell="{ row }"><span class="tabular-nums">{{ tokens(row.original.totals.tokens) }}</span></template>
        </UTable>
      </section>
    </div>
    <div v-else-if="loading" class="mx-auto w-full max-w-4xl space-y-4" aria-busy="true">
      <USkeleton class="h-9 w-32" />
      <USkeleton class="h-44 w-full" />
    </div>
  </ViewPanel>
</template>
