<script setup lang="ts">
// Everything the applied definitions define and what happens next: schedules, services, jobs
// and agents, each job with its recent runs; the full run history (the History tab); and disk use
// (the Storage tab).
import type { TabsItem } from "@nuxt/ui";
import { computed, onMounted, ref, shallowRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import JobSlideover from "../components/system/JobSlideover.vue";
import RunnableAction from "../components/system/RunnableAction.vue";
import RunsTable from "../components/system/RunsTable.vue";
import StoragePanel from "../components/system/StoragePanel.vue";
import TriggerChips from "../components/system/TriggerChips.vue";
import type { JobView, SystemView } from "../generated";
import { portUrl, runDuration, runStatusColor } from "../lib/runs";
import { fireLabel, lastFailed, plain, schedules, serviceStateColor } from "../lib/system";
import { notifyError } from "../notify";
import { useNow } from "../now";
import { refreshStatus, state } from "../store";

const route = useRoute();
const router = useRouter();
const now = useNow();
const hostname = window.location.hostname;
const view = shallowRef<SystemView | null>(null);
const loading = ref(false);
const history = ref<InstanceType<typeof RunsTable> | null>(null);

async function load() {
  loading.value = true;
  try {
    view.value = await call("system", {});
  } catch (e) {
    notifyError("Could not load the system", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

function refresh() {
  void load();
  void history.value?.load();
  void refreshStatus();
}

// Runs finishing, failing or starting by hand change what this shows.
let timer: ReturnType<typeof setTimeout> | undefined;
watch(
  () => state.events[0]?.id,
  () => {
    const t = state.events[0]?.type ?? "";
    if (!t.startsWith("runnable.") && t !== "manual" && t !== "schedule") return;
    clearTimeout(timer);
    timer = setTimeout(refresh, 800);
  },
);

/** The query, minus `drop`, plus `set`. */
function query(set: Record<string, string>, ...drop: string[]) {
  const q = { ...route.query, ...set };
  for (const k of drop) delete q[k];
  return q;
}

const tabs: TabsItem[] = [
  { label: "Overview", value: "overview", icon: "i-lucide-layout-list" },
  { label: "History", value: "history", icon: "i-lucide-history" },
  { label: "Storage", value: "storage", icon: "i-lucide-hard-drive" },
];
const tab = computed({
  get: () => (route.query.tab === "history" || route.query.tab === "storage" ? route.query.tab : "overview"),
  set: (t: string) =>
    void router.replace({ query: t === "overview" ? query({}, "tab", "runnable", "run") : query({ tab: t }, ...(t === "storage" ? ["runnable", "run"] : [])) }),
});

const runnable = computed(() => (typeof route.query.runnable === "string" ? route.query.runnable : undefined));
const logsOf = computed({
  get: () => (typeof route.query.run === "string" ? route.query.run : null),
  set: (id: string | null) => void router.replace({ query: id ? query({ run: id }) : query({}, "run") }),
});

const selected = computed((): JobView | null => {
  const name = route.query.job;
  return (typeof name === "string" && view.value?.jobs.find((j) => j.name === name)) || null;
});
function openJob(name: string) {
  void router.push({ query: query({ job: name }) });
}

const upcoming = computed(() => (view.value ? schedules(view.value) : []));
const services = computed(() => view.value?.jobs.filter((j) => j.service) ?? []);
const jobs = computed(() => view.value?.jobs.filter((j) => !j.service) ?? []);
const tz = computed(() => view.value?.timezone ?? "UTC");
</script>

<template>
  <ViewPanel id="system" title="System" icon="i-lucide-server" body-class="p-0 sm:p-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="refresh" />
    </template>
    <template #toolbar>
      <div class="flex w-full min-w-0 items-center gap-3">
        <UTabs v-model="tab" :items="tabs" :content="false" variant="link" size="sm" />
        <UTooltip v-if="view" :text="`Applied ${view.commit.id}`">
          <span class="ms-auto hidden min-w-0 items-center gap-1.5 truncate text-xs text-muted sm:flex">
            <UIcon name="i-lucide-git-commit-horizontal" class="size-4 shrink-0" />
            <code>{{ view.commit.short }}</code>
            <span class="truncate">{{ view.commit.title }}</span>
            · <TimeAgo :at="view.commit.at" class="whitespace-nowrap" />
          </span>
        </UTooltip>
      </div>
    </template>

    <StoragePanel v-if="tab === 'storage'" />

    <div v-else-if="tab === 'history'" class="flex flex-1 flex-col">
      <div v-if="runnable" class="flex items-center gap-2 border-b border-default px-4 py-2 text-sm">
        <span class="text-muted">Runs of</span>
        <UBadge color="neutral" variant="subtle" :label="runnable" />
        <UButton size="xs" color="neutral" variant="ghost" icon="i-lucide-x" aria-label="Show all runs" @click="router.replace({ query: query({}, 'runnable') })" />
      </div>
      <RunsTable ref="history" v-model:logs="logsOf" :runnable="runnable" />
    </div>

    <div v-else-if="!view" class="space-y-4 p-4 sm:p-6">
      <USkeleton v-for="i in 4" :key="i" class="h-24 w-full" />
    </div>

    <div v-else class="mx-auto w-full max-w-5xl space-y-8 p-4 sm:p-6">
      <UAlert v-if="view.paused" color="warning" variant="subtle" icon="i-lucide-octagon-pause" title="botler is paused" description="Nothing fires and services are stopped until you resume (hub menu)." />

      <section aria-labelledby="sys-schedules" class="space-y-3">
        <h2 id="sys-schedules" class="flex items-center gap-2 font-semibold text-highlighted">
          <UIcon name="i-lucide-calendar-clock" class="size-5 text-muted" /> Schedules
          <span class="text-xs font-normal text-muted">({{ tz }})</span>
        </h2>
        <UAlert v-if="!upcoming.length" icon="i-lucide-calendar-off" title="Nothing scheduled" description="Add a cron or every trigger to a job, an agent or triggers/." color="neutral" variant="outline" />
        <ul v-else class="divide-y divide-default rounded-lg border border-default">
          <li v-for="s in upcoming" :key="s.trigger.id" class="flex flex-col gap-1 p-3 sm:flex-row sm:items-center sm:gap-4">
            <span class="w-44 shrink-0 text-sm font-medium tabular-nums text-highlighted">{{ fireLabel(s.next, tz, now) }}</span>
            <span class="min-w-0 flex-1 text-sm">
              <span class="text-default">{{ plain(s.trigger.then) }}</span>
              <span class="block truncate text-xs text-muted">{{ plain(s.trigger.when) }}</span>
            </span>
            <UBadge v-if="s.trigger.shadow" color="neutral" variant="outline" size="sm" label="shadow" class="self-start sm:self-center" />
            <UButton v-if="s.job" size="xs" color="neutral" variant="ghost" trailing-icon="i-lucide-chevron-right" :label="s.job" class="self-start sm:self-center" @click="openJob(s.job)" />
          </li>
        </ul>
      </section>

      <section aria-labelledby="sys-services" class="space-y-3">
        <h2 id="sys-services" class="flex items-center gap-2 font-semibold text-highlighted">
          <UIcon name="i-lucide-server-cog" class="size-5 text-muted" /> Services
        </h2>
        <UAlert v-if="!services.length" icon="i-lucide-server-off" title="No services" description="Jobs with lifetime = &quot;service&quot; run continuously and show up here." color="neutral" variant="outline" />
        <ul v-else class="divide-y divide-default rounded-lg border border-default">
          <li v-for="j in services" :key="j.name" class="flex flex-wrap items-center gap-x-3 gap-y-2 p-3">
            <UButton variant="link" color="neutral" class="p-0 font-medium text-highlighted" :label="j.name" @click="openJob(j.name)" />
            <UBadge :color="serviceStateColor(j.service!.state)" variant="subtle" size="sm" :label="j.service!.state" />
            <UButton
              v-for="p in j.service!.published"
              :key="p.host_port"
              :to="portUrl(p, hostname)"
              target="_blank"
              size="xs"
              variant="link"
              trailing-icon="i-lucide-external-link"
              :label="`:${p.host_port}`"
              class="px-0.5"
            />
            <span v-if="j.incident" class="text-xs text-error">incident: {{ j.incident.kind }} ×{{ j.incident.count }}</span>
            <RunnableAction :job="j" :paused="view.paused" class="ms-auto" @done="refresh" />
          </li>
        </ul>
      </section>

      <section aria-labelledby="sys-jobs" class="space-y-3">
        <h2 id="sys-jobs" class="flex items-center gap-2 font-semibold text-highlighted">
          <UIcon name="i-lucide-square-terminal" class="size-5 text-muted" /> Jobs
        </h2>
        <UAlert v-if="!jobs.length" icon="i-lucide-square-terminal" title="No jobs" description="Jobs live in jobs/<name>/job.toml." color="neutral" variant="outline" />
        <ul v-else class="divide-y divide-default rounded-lg border border-default">
          <li v-for="j in jobs" :key="j.name" class="flex flex-col gap-2 p-3 sm:flex-row sm:items-center sm:gap-4">
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-center gap-2">
                <UButton variant="link" color="neutral" class="p-0 font-medium text-highlighted" :label="j.name" @click="openJob(j.name)" />
                <UBadge v-if="j.incident" color="error" variant="subtle" size="sm" icon="i-lucide-siren" :label="`incident: ${j.incident.kind}`" />
                <UBadge v-else-if="lastFailed(j)" color="error" variant="subtle" size="sm" label="last run failed" />
              </div>
              <p v-if="j.description" class="line-clamp-2 text-sm text-muted">{{ j.description }}</p>
              <TriggerChips v-if="j.triggers.length" :triggers="j.triggers" :timezone="tz" :now="now" class="mt-1" />
              <p v-else class="mt-1 text-xs text-muted">no trigger</p>
            </div>
            <div class="flex items-center gap-1.5">
              <span v-if="!j.recent_runs.length" class="text-xs text-dimmed">never ran</span>
              <UTooltip v-for="r in j.recent_runs" :key="r.id" :text="`${r.status}${runDuration(r) ? ` · ${runDuration(r)}` : ''} · ${new Date(r.created_at).toLocaleString()}`">
                <UButton
                  size="xs"
                  :color="runStatusColor(r.status)"
                  variant="soft"
                  class="tabular-nums"
                  :label="runDuration(r) || r.status"
                  :aria-label="`Logs of the ${r.status} run`"
                  @click="router.push({ query: query({ tab: 'history', runnable: j.name, run: r.id }) })"
                />
              </UTooltip>
            </div>
            <RunnableAction :job="j" :paused="view.paused" class="self-start sm:self-center" @done="refresh" />
          </li>
        </ul>
      </section>

      <section aria-labelledby="sys-agents" class="space-y-3">
        <h2 id="sys-agents" class="flex items-center gap-2 font-semibold text-highlighted">
          <UIcon name="i-lucide-bot" class="size-5 text-muted" /> Agents
        </h2>
        <UAlert v-if="!view.agents.length" icon="i-lucide-bot-off" title="No agents" color="neutral" variant="outline" />
        <ul v-else class="divide-y divide-default rounded-lg border border-default">
          <li v-for="a in view.agents" :key="a.name" class="space-y-1.5 p-3">
            <div class="flex flex-wrap items-center gap-2">
              <span class="font-medium text-highlighted">{{ a.name }}</span>
              <UBadge color="neutral" variant="subtle" size="sm" :label="a.kind" />
              <UBadge v-if="a.always_on" color="primary" variant="subtle" size="sm" label="always on" />
              <span class="ms-auto text-xs text-muted">{{ a.instances }} {{ a.instances === 1 ? "chat" : "chats" }}</span>
            </div>
            <p v-if="a.description" class="line-clamp-2 text-sm text-muted">{{ a.description }}</p>
            <div v-if="a.tools.length || a.secrets.length" class="flex flex-wrap gap-1">
              <UBadge v-for="t in a.tools" :key="`t${t}`" color="neutral" variant="outline" size="sm" icon="i-lucide-wrench" :label="t" />
              <UBadge v-for="s in a.secrets" :key="`s${s}`" color="neutral" variant="outline" size="sm" icon="i-lucide-key-round" :label="s" />
            </div>
            <TriggerChips v-if="a.triggers.length" :triggers="a.triggers" :timezone="tz" :now="now" />
          </li>
        </ul>
      </section>
    </div>

    <JobSlideover
      :job="selected"
      :timezone="tz"
      :paused="view?.paused ?? false"
      @close="router.push({ query: query({}, 'job') })"
      @changed="refresh"
    />
  </ViewPanel>
</template>
