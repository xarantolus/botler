<script setup lang="ts">
// Changes agents proposed to the monorepo: their status, and per proposal the deployment summary,
// the review and the gate report. Merged ones can be reverted.
import type { TableColumn, TableRow } from "@nuxt/ui";
import { onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import ChangeSummary from "../components/common/ChangeSummary.vue";
import CodeBlock from "../components/common/CodeBlock.vue";
import MarkdownText from "../components/common/MarkdownText.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm } from "../confirm";
import type { Proposal, ProposalDetail } from "../generated";
import { proposalStatusColor, proposalStatusLabel } from "../lib/runs";
import { notifyError, notifySuccess } from "../notify";
import { refreshStatus } from "../store";

const proposals = shallowRef<Proposal[] | null>(null);
const loading = ref(false);

async function load() {
  loading.value = true;
  try {
    proposals.value = await call("proposals", {});
  } catch (e) {
    notifyError("Could not load proposals", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

const detail = shallowRef<ProposalDetail | null>(null);
const open = ref(false);
const loadingDetail = ref(false);

async function show(p: Proposal) {
  open.value = true;
  loadingDetail.value = true;
  detail.value = null;
  try {
    detail.value = await call("proposal", { body: { id: p.id } });
  } catch (e) {
    notifyError("Could not load the proposal", e);
    open.value = false;
  } finally {
    loadingDetail.value = false;
  }
}

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
  void show(row.original);
}
</script>

<template>
  <ViewPanel id="proposals" title="Proposals" icon="i-lucide-git-pull-request-arrow" body-class="p-0 sm:p-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <UTable
      :data="proposals ?? []"
      :columns="columns"
      :loading="loading && !proposals"
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
        <UEmpty
          icon="i-lucide-git-pull-request-arrow"
          title="No proposals yet"
          description="When an agent proposes a change to the monorepo, it shows up here for review."
          variant="naked"
        />
      </template>
    </UTable>

    <USlideover
      v-model:open="open"
      :title="detail?.title ?? 'Proposal'"
      :description="detail?.branch ?? undefined"
      :ui="{ content: 'sm:max-w-2xl' }"
    >
      <template #body>
        <div v-if="loadingDetail" class="space-y-3">
          <USkeleton class="h-6 w-1/3" />
          <USkeleton class="h-40 w-full" />
        </div>
        <div v-else-if="detail" class="space-y-6">
          <div class="flex flex-wrap items-center gap-2 text-sm">
            <UBadge :color="proposalStatusColor(detail.status)" variant="subtle" :label="proposalStatusLabel(detail.status)" />
            <span v-if="detail.merged_commit" class="text-muted">
              merged as <code class="font-mono">{{ detail.merged_commit.slice(0, 10) }}</code>
            </span>
          </div>
          <section v-if="detail.summary" class="space-y-2">
            <h3 class="text-sm font-semibold text-highlighted">Deployment summary</h3>
            <ChangeSummary :markdown="detail.summary" />
          </section>
          <section v-if="detail.verdict || detail.notes" class="space-y-2">
            <h3 class="text-sm font-semibold text-highlighted">Review</h3>
            <p v-if="detail.verdict" class="text-sm">Verdict: <strong>{{ detail.verdict }}</strong></p>
            <MarkdownText v-if="detail.notes" :source="detail.notes" class="text-sm" />
          </section>
          <section v-if="detail.gate_report" class="space-y-2">
            <h3 class="text-sm font-semibold text-highlighted">Gate report</h3>
            <CodeBlock :text="detail.gate_report" max-height="24rem" />
          </section>
        </div>
      </template>
      <template v-if="detail?.merged_commit" #footer>
        <UButton
          color="warning"
          variant="outline"
          icon="i-lucide-undo-2"
          label="Revert"
          @click="revert(detail.merged_commit, detail.title)"
        />
      </template>
    </USlideover>
  </ViewPanel>
</template>
