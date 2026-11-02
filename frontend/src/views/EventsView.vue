<script setup lang="ts">
// The event journal, filterable by type and chat; an event's trace (who it reached, which runs
// it started, its thread) opens beside it, with jumps to where it landed in each chat.
import type { TableColumn, TableRow } from "@nuxt/ui";
import { computed, ref, shallowRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import CodeBlock from "../components/common/CodeBlock.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { BotlerEvent, Trace } from "../generated";
import { eventIcon, eventSummary, sourceLabel } from "../lib/events";
import { provenanceColor, runStatusColor } from "../lib/runs";
import { notifyError } from "../notify";
import { state } from "../store";
import { useQueryList, useQueryParam } from "../lib/queryState";

const route = useRoute();
const router = useRouter();

const typeFilter = useQueryList("type");
const chatParam = useQueryParam("chat", "");
const chatFilter = computed<string | null | undefined>({
  get: () => chatParam.value || undefined,
  set: (v) => (chatParam.value = v ?? ""),
});

const typeOptions = computed(() => [...new Set(state.events.map((e) => e.type))].sort());
const chatOptions = computed(() =>
  state.instances.map((i) => ({ label: i.title ? `${i.title} (${i.name})` : i.name, value: i.name })),
);

function touches(e: BotlerEvent, inst: string): boolean {
  return e.to === inst || (e.source.kind === "agent" && e.source.name === inst);
}

const rows = computed(() =>
  state.events.filter(
    (e) =>
      (!typeFilter.value.length || typeFilter.value.includes(e.type)) && (!chatFilter.value || touches(e, chatFilter.value)),
  ),
);

const columns: TableColumn<BotlerEvent>[] = [
  { accessorKey: "at", header: "When" },
  { accessorKey: "type", header: "Type" },
  { id: "from", header: "From" },
  { accessorKey: "to", header: "To" },
  { accessorKey: "provenance", header: "Trust" },
  { id: "summary", header: "What" },
];

// ---- trace ----
const open = ref(false);
const trace = shallowRef<Trace | null>(null);
const loading = ref(false);

async function show(id: string) {
  open.value = true;
  loading.value = true;
  trace.value = null;
  try {
    trace.value = await call("trace", { params: { id } });
  } catch (e) {
    notifyError("Could not load the trace", e);
    open.value = false;
  } finally {
    loading.value = false;
  }
}

watch(
  () => route.query.event,
  (id) => {
    if (typeof id === "string") void show(id);
  },
  { immediate: true },
);
watch(open, (o) => {
  if (!o && route.query.event) void router.replace({ query: { ...route.query, event: undefined } });
});

function jump(instance: string, delivery: string) {
  void router.push({ path: `/chat/${encodeURIComponent(instance)}`, query: { delivery } });
}

function onSelect(_e: Event, row: TableRow<BotlerEvent>) {
  void show(row.original.id);
}
</script>

<template>
  <ViewPanel id="events" title="Events" icon="i-lucide-activity" body-class="p-0 sm:p-0">
    <template #right>
      <UBadge
        :color="state.connection === 'live' ? 'success' : 'warning'"
        variant="subtle"
        :label="state.connection === 'live' ? 'live' : 'reconnecting'"
      />
    </template>
    <template #toolbar>
      <div class="flex w-full flex-wrap items-center gap-2 py-2">
        <USelectMenu
          v-model="typeFilter"
          :items="typeOptions"
          multiple
          placeholder="All types"
          icon="i-lucide-filter"
          class="w-full sm:w-64"
          aria-label="Filter by type"
          :search-input="{ placeholder: 'Filter types…' }"
        />
        <USelectMenu
          v-model="chatFilter"
          :items="chatOptions"
          value-key="value"
          placeholder="Any chat"
          icon="i-lucide-message-square"
          class="w-full sm:w-64"
          aria-label="Filter by chat"
          :clear="true"
        />
        <span class="ms-auto text-xs text-muted tabular-nums">{{ rows.length }} of {{ state.events.length }}</span>
      </div>
    </template>

    <UTable
      :data="rows"
      :columns="columns"
      sticky
      :loading="!state.events.length && state.connection === 'connecting'"
      class="flex-1"
      :ui="{ td: 'py-2.5', tr: 'cursor-pointer' }"
      :get-row-id="(e: BotlerEvent) => e.id"
      @select="onSelect"
    >
      <template #at-cell="{ row }"><TimeAgo :at="row.original.at" class="whitespace-nowrap text-muted" /></template>
      <template #type-cell="{ row }">
        <span class="inline-flex items-center gap-1.5 whitespace-nowrap">
          <UIcon :name="eventIcon(row.original.type)" class="size-4 shrink-0 text-muted" />
          <span class="font-medium text-highlighted">{{ row.original.type }}</span>
          <UBadge v-if="row.original.urgent" color="error" variant="subtle" size="sm" label="urgent" />
        </span>
      </template>
      <template #from-cell="{ row }"><span class="whitespace-nowrap">{{ sourceLabel(row.original.source) }}</span></template>
      <template #to-cell="{ row }">
        <span class="whitespace-nowrap" :class="!row.original.to && 'text-dimmed'">{{ row.original.to ?? "subscribers" }}</span>
      </template>
      <template #provenance-cell="{ row }">
        <UBadge :color="provenanceColor(row.original.provenance)" variant="subtle" size="sm" :label="row.original.provenance" />
      </template>
      <template #summary-cell="{ row }">
        <span class="line-clamp-2 min-w-48 max-w-xl text-muted">{{ eventSummary(row.original) }}</span>
      </template>
      <template #empty>
        <UEmpty class="sticky start-0 max-w-[calc(100vw-2rem)]" icon="i-lucide-activity" title="No events" description="Nothing matches these filters yet." variant="naked" />
      </template>
    </UTable>

    <USlideover v-model:open="open" title="Event trace" :description="trace ? trace.event.id : 'Loading'" :ui="{ content: 'sm:max-w-xl' }">
      <template #body>
        <div v-if="loading" class="space-y-3">
          <USkeleton class="h-6 w-1/2" />
          <USkeleton class="h-32 w-full" />
          <USkeleton class="h-10 w-full" />
        </div>
        <div v-else-if="trace" class="space-y-6 text-sm">
          <section class="space-y-2">
            <div class="flex flex-wrap items-center gap-2">
              <UIcon :name="eventIcon(trace.event.type)" class="size-5 text-primary" />
              <span class="text-base font-semibold text-highlighted">{{ trace.event.type }}</span>
              <UBadge :color="provenanceColor(trace.event.provenance)" variant="subtle" size="sm" :label="trace.event.provenance" />
            </div>
            <p class="text-muted">
              from {{ sourceLabel(trace.event.source) }}<template v-if="trace.event.to"> to {{ trace.event.to }}</template> ·
              <TimeAgo :at="trace.event.at" />
            </p>
            <CodeBlock label="Payload" :text="JSON.stringify(trace.event.payload, null, 2)" max-height="20rem" />
          </section>

          <section class="space-y-2">
            <h3 class="font-semibold text-highlighted">Reached</h3>
            <p v-if="!trace.reached.length" class="text-muted">No chat directly.</p>
            <div
              v-for="r in trace.reached"
              :key="r.delivery.id"
              class="flex items-center gap-3 rounded-md border border-default px-3 py-2"
            >
              <UIcon name="i-lucide-message-square" class="size-4 shrink-0 text-muted" />
              <div class="min-w-0 flex-1">
                <div class="truncate font-medium">{{ r.title || r.slug || r.instance }}</div>
                <div class="truncate text-xs text-muted">
                  {{ r.delivery.status }}<template v-if="r.delivery.note"> · {{ r.delivery.note }}</template>
                </div>
              </div>
              <UButton
                size="sm"
                variant="soft"
                trailing-icon="i-lucide-arrow-right"
                label="Jump to chat"
                @click="jump(r.instance, r.session_anchor.delivery_id)"
              />
            </div>
          </section>

          <section v-if="trace.runs.length" class="space-y-2">
            <h3 class="font-semibold text-highlighted">Runs it started</h3>
            <ULink
              v-for="r in trace.runs"
              :key="r.id"
              :to="{ name: 'system', query: { tab: 'history', run: r.id } }"
              class="flex items-center gap-2 rounded-md px-1 py-1 hover:bg-elevated"
            >
              <UIcon name="i-lucide-play" class="size-4 text-muted" />
              <span class="font-medium">{{ r.runnable }}</span>
              <UBadge :color="runStatusColor(r.status)" variant="subtle" size="sm" :label="r.status" />
              <TimeAgo :at="r.created_at" class="ms-auto text-xs text-muted" />
            </ULink>
          </section>

          <section v-if="trace.thread.length" class="space-y-2">
            <h3 class="font-semibold text-highlighted">Thread</h3>
            <UTimeline
              :items="
                trace.thread.map((t) => ({
                  value: t.id,
                  title: t.type,
                  description: eventSummary(t),
                  date: sourceLabel(t.source),
                  icon: eventIcon(t.type),
                }))
              "
              :default-value="trace.event.id"
              size="xs"
              :ui="{ title: 'cursor-pointer hover:underline', description: 'line-clamp-2' }"
              @select="(_e: Event, item: { value?: string | number }) => item.value && show(String(item.value))"
            />
          </section>
        </div>
      </template>
    </USlideover>
  </ViewPanel>
</template>
