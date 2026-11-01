<script setup lang="ts">
// Changes agents proposed to the monorepo and their status (live). A row opens its review page;
// merged ones can be reverted. "Current" holds those still in flight, "History" the decided ones.
// A table on wide screens, stacked rows on a phone.
import type { TableColumn, TableRow } from "@nuxt/ui";
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../api/client";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm } from "../confirm";
import type { Proposal } from "../generated";
import { inFlight, stageSummary } from "../lib/proposals";
import { proposalStatusColor, proposalStatusLabel } from "../lib/runs";
import { notifyError, notifySuccess } from "../notify";
import { useNow } from "../now";
import { refreshProposals, refreshStatus, state } from "../store";

const router = useRouter();
const loading = ref(false);
const now = useNow(1000);

async function load() {
  loading.value = true;
  await refreshProposals();
  loading.value = false;
}
onMounted(load);

const current = computed(() => (state.proposals ?? []).filter(inFlight));
const tab = ref<string | number>("current");
const tabs = computed(() => [
  { label: "Current", value: "current", icon: "i-lucide-git-pull-request-arrow", badge: current.value.length ? String(current.value.length) : undefined },
  { label: "History", value: "history", icon: "i-lucide-history" },
]);
const shown = computed(() => (tab.value === "current" ? current.value : (state.proposals ?? []).filter((p) => !inFlight(p))));

async function revert(commit: string, title: string) {
  const ok = await confirm({
    title: "Revert this change?",
    description: `A revert commit for "${title}" (${commit.slice(0, 10)}) is applied to main right away.`,
    confirmLabel: "Revert",
    color: "warning",
  });
  if (!ok) return;
  try {
    const r = await call("revert", { body: { commit } });
    notifySuccess("Reverted", `main is now ${r.commit.slice(0, 10)}`);
    await Promise.all([load(), refreshStatus()]);
  } catch (e) {
    notifyError("Revert failed", e);
  }
}

const empty = computed(() =>
  tab.value === "current"
    ? {
        icon: "i-lucide-git-pull-request-arrow",
        title: "Nothing in flight",
        description: "When an agent proposes a change to the monorepo, it shows up here for review. Decided ones are under History.",
      }
    : { icon: "i-lucide-history", title: "No decided proposals yet", description: "Merged, rejected and failed proposals end up here." },
);

const columns: TableColumn<Proposal>[] = [
  { accessorKey: "title", header: "Title" },
  { accessorKey: "instance", header: "From" },
  { accessorKey: "kind", header: "Kind" },
  { accessorKey: "status", header: "Status" },
  { accessorKey: "verdict", header: "Verdict" },
  { accessorKey: "updated_at", header: "Updated" },
  { id: "actions", header: "" },
];

function onSelect(_e: Event, row: TableRow<Proposal>) {
  void router.push({ name: "proposal", params: { id: row.original.id } });
}
</script>

<template>
  <ViewPanel id="proposals" title="Proposals" icon="i-lucide-git-pull-request-arrow" body-class="p-0 sm:p-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <UTabs v-model="tab" :items="tabs" :content="false" variant="link" size="sm" class="px-4 pt-2 sm:px-6" />
    <!-- Phone: one stacked row each; the title's link covers the row. -->
    <div class="xl:hidden">
      <div v-if="loading && !state.proposals" class="space-y-3 p-4">
        <USkeleton v-for="i in 3" :key="i" class="h-12 w-full" />
      </div>
      <UEmpty v-else-if="!shown.length" v-bind="empty" variant="naked" class="p-4" />
      <ul v-else class="divide-y divide-default">
        <li v-for="p in shown" :key="p.id" class="relative px-4 py-3 hover:bg-elevated/50">
          <ULink
            :to="{ name: 'proposal', params: { id: p.id } }"
            class="block font-medium break-words text-highlighted after:absolute after:inset-0"
          >
            {{ p.title }}
          </ULink>
          <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
            <UBadge :color="proposalStatusColor(p.status)" variant="subtle" size="sm" :label="proposalStatusLabel(p.status)" />
            <span v-if="stageSummary(p, now)">{{ stageSummary(p, now) }}</span>
            <span>from <span class="text-default">{{ p.instance }}</span> · <TimeAgo :at="p.updated_at" /></span>
            <span class="text-dimmed">{{ [p.kind, p.verdict].filter(Boolean).join(" · ") }}</span>
            <UButton
              v-if="p.merged_commit"
              size="xs"
              color="warning"
              variant="outline"
              icon="i-lucide-undo-2"
              label="Revert"
              class="relative ms-auto"
              @click="revert(p.merged_commit, p.title)"
            />
          </div>
        </li>
      </ul>
    </div>
    <UTable
      :data="shown"
      :columns="columns"
      :loading="loading && !state.proposals"
      sticky
      class="hidden flex-1 xl:block"
      :ui="{ td: 'py-2.5', tr: 'cursor-pointer' }"
      @select="onSelect"
    >
      <template #title-cell="{ row }"><span class="block max-w-sm min-w-48 font-medium break-words whitespace-normal text-highlighted">{{ row.original.title }}</span></template>
      <template #status-cell="{ row }">
        <UBadge
          :color="proposalStatusColor(row.original.status)"
          variant="subtle"
          size="sm"
          :label="proposalStatusLabel(row.original.status)"
          class="whitespace-nowrap"
        />
        <span v-if="stageSummary(row.original, now)" class="ms-2 whitespace-nowrap text-xs text-muted" :title="row.original.stage_detail ?? undefined">
          {{ stageSummary(row.original, now) }}
        </span>
      </template>
      <template #verdict-cell="{ row }"><span class="text-muted">{{ row.original.verdict ?? "" }}</span></template>
      <template #updated_at-cell="{ row }"><TimeAgo :at="row.original.updated_at" class="whitespace-nowrap text-muted" /></template>
      <template #actions-cell="{ row }">
        <UButton
          v-if="row.original.merged_commit"
          size="xs"
          color="warning"
          variant="outline"
          icon="i-lucide-undo-2"
          label="Revert"
          @click.stop="revert(row.original.merged_commit, row.original.title)"
        />
      </template>
      <template #empty>
        <UEmpty class="sticky start-0 max-w-[calc(100vw-2rem)]" v-bind="empty" variant="naked" />
      </template>
    </UTable>

  </ViewPanel>
</template>
