<script setup lang="ts">
// Changes agents proposed to the monorepo and their status (live). A row opens its review page;
// merged ones can be reverted.
import type { TableColumn, TableRow } from "@nuxt/ui";
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../api/client";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm } from "../confirm";
import type { Proposal } from "../generated";
import { stageSummary } from "../lib/proposals";
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
    <UTable
      :data="state.proposals ?? []"
      :columns="columns"
      :loading="loading && !state.proposals"
      sticky
      class="flex-1"
      :ui="{ td: 'py-2.5', tr: 'cursor-pointer' }"
      @select="onSelect"
    >
      <template #title-cell="{ row }"><span class="font-medium text-highlighted">{{ row.original.title }}</span></template>
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
        <UEmpty class="sticky start-0 max-w-[calc(100vw-2rem)]"
          icon="i-lucide-git-pull-request-arrow"
          title="No proposals yet"
          description="When an agent proposes a change to the monorepo, it shows up here for review."
          variant="naked"
        />
      </template>
    </UTable>

  </ViewPanel>
</template>
