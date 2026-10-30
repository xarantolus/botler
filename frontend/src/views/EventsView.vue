<script setup lang="ts">
import {
  NButton,
  NDataTable,
  NDrawer,
  NDrawerContent,
  NEmpty,
  NSelect,
  NSpace,
  NSpin,
  NTag,
  NText,
  type DataTableColumns,
} from "naive-ui";
import { computed, h, ref, watch, shallowRef } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import type { BotlerEvent, Trace } from "../generated";
import { eventSummary, sourceLabel } from "../lib/events";
import { ago } from "../lib/time";
import { notifyError } from "../notify";
import { state } from "../store";

const route = useRoute();
const router = useRouter();

const typeFilter = ref<string[]>([]);
const instanceFilter = ref<string | null>(null);

const typeOptions = computed(() =>
  [...new Set(state.events.map((e) => e.type))].sort().map((t) => ({ label: t, value: t })),
);
const instanceOptions = computed(() =>
  state.instances.map((i) => ({ label: i.title ? `${i.title} (${i.name})` : i.name, value: i.name })),
);

function touches(e: BotlerEvent, inst: string): boolean {
  return e.to === inst || (e.source.kind === "agent" && e.source.name === inst);
}

const rows = computed(() =>
  state.events.filter(
    (e) =>
      (!typeFilter.value.length || typeFilter.value.includes(e.type)) &&
      (!instanceFilter.value || touches(e, instanceFilter.value)),
  ),
);

const provType = (p: string) => (p === "trusted" ? "success" : p === "external" ? "warning" : "error");

const columns: DataTableColumns<BotlerEvent> = [
  { title: "When", key: "at", width: 110, render: (e) => h("span", { title: e.at }, ago(e.at)) },
  { title: "Type", key: "type", width: 170, render: (e) => h(NTag, { size: "small", bordered: false }, () => e.type) },
  { title: "From", key: "source", width: 150, ellipsis: { tooltip: true }, render: (e) => sourceLabel(e.source) },
  { title: "To", key: "to", width: 130, ellipsis: { tooltip: true }, render: (e) => e.to ?? "subscribers" },
  {
    title: "Trust",
    key: "provenance",
    width: 95,
    render: (e) => h(NTag, { size: "small", type: provType(e.provenance), bordered: false }, () => e.provenance),
  },
  { title: "What", key: "summary", ellipsis: { tooltip: true }, render: (e) => eventSummary(e) },
];

// ---- trace drawer ----
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
  if (!o && route.query.event) void router.replace({ query: {} });
});

function jump(instance: string, delivery: string) {
  void router.push({ path: `/chat/${encodeURIComponent(instance)}`, query: { delivery } });
}

const rowProps = (e: BotlerEvent) => ({ style: "cursor: pointer", onClick: () => void show(e.id) });
</script>

<template>
  <div style="display: flex; flex-direction: column; height: 100%; padding: 8px 12px; gap: 8px; min-height: 0">
    <NSpace>
      <NSelect
        v-model:value="typeFilter"
        multiple
        clearable
        filterable
        :options="typeOptions"
        placeholder="all types"
        style="min-width: 220px; max-width: 420px"
        size="small"
      />
      <NSelect
        v-model:value="instanceFilter"
        clearable
        filterable
        :options="instanceOptions"
        placeholder="any chat"
        style="min-width: 200px"
        size="small"
      />
      <NText depth="3" style="font-size: 12px; line-height: 28px">
        {{ rows.length }} of {{ state.events.length }} · {{ state.connection === "live" ? "live" : "refreshing" }}
      </NText>
    </NSpace>
    <NDataTable
      :columns="columns"
      :data="rows"
      :row-key="(e: BotlerEvent) => e.id"
      :row-props="rowProps"
      size="small"
      flex-height
      style="flex: 1; min-height: 0"
      :scroll-x="820"
    />
    <NDrawer v-model:show="open" :width="560" placement="right" style="max-width: 100vw">
      <NDrawerContent title="Event trace" closable>
        <NSpin v-if="loading" />
        <template v-else-if="trace">
          <NSpace vertical>
            <div>
              <NTag :bordered="false">{{ trace.event.type }}</NTag>
              <NText depth="3"> from {{ sourceLabel(trace.event.source) }} · {{ trace.event.provenance }} · {{ ago(trace.event.at) }}</NText>
            </div>
            <pre class="mono">{{ JSON.stringify(trace.event.payload, null, 2) }}</pre>

            <strong>Reached</strong>
            <NEmpty v-if="!trace.reached.length" description="No chat directly." size="small" />
            <div v-for="r in trace.reached" :key="r.delivery.id" style="display: flex; gap: 8px; align-items: center">
              <span style="flex: 1">
                {{ r.title || r.slug || r.instance }}
                <NText depth="3">· {{ r.delivery.status }}{{ r.delivery.note ? ` (${r.delivery.note})` : "" }}</NText>
              </span>
              <NButton size="small" type="primary" ghost @click="jump(r.instance, r.session_anchor.delivery_id)">
                Jump to chat
              </NButton>
            </div>

            <template v-if="trace.runs.length">
              <strong>Runs it started</strong>
              <div v-for="r in trace.runs" :key="r.id">
                <RouterLink :to="{ name: 'runs', query: { run: r.id } }">{{ r.runnable }}</RouterLink>
                <NText depth="3"> · {{ r.status }} · {{ ago(r.created_at) }}</NText>
              </div>
            </template>

            <template v-if="trace.thread.length">
              <strong>Thread</strong>
              <div v-for="t in trace.thread" :key="t.id" style="cursor: pointer" @click="show(t.id)">
                <NTag size="small" :bordered="false">{{ t.type }}</NTag>
                <NText depth="3"> {{ sourceLabel(t.source) }} · {{ ago(t.at) }}</NText>
                <div style="font-size: 13px">{{ eventSummary(t) }}</div>
              </div>
            </template>
          </NSpace>
        </template>
      </NDrawerContent>
    </NDrawer>
  </div>
</template>
