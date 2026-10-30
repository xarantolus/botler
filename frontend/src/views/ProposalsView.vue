<script setup lang="ts">
import {
  NButton,
  NDataTable,
  NDrawer,
  NDrawerContent,
  NSpace,
  NSpin,
  NTag,
  NText,
  type DataTableColumns,
} from "naive-ui";
import { h, onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import ChangeSummary from "../components/ChangeSummary.vue";
import MarkdownText from "../components/MarkdownText.vue";
import type { Proposal, ProposalDetail, ProposalStatus } from "../generated";
import { ago } from "../lib/time";
import { dialog, message, notifyError } from "../notify";
import { refreshStatus } from "../store";

const proposals = shallowRef<Proposal[]>([]);
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

const statusType = (s: ProposalStatus) =>
  s === "merged" || s === "approved"
    ? "success"
    : s === "rejected" || s === "failed"
      ? "error"
      : s === "awaiting_user"
        ? "warning"
        : "info";

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

function revert(commit: string, title: string) {
  dialog.warning({
    title: "Revert this change?",
    content: `A revert commit for "${title}" (${commit.slice(0, 10)}) is applied to main right away.`,
    positiveText: "Revert",
    negativeText: "Cancel",
    onPositiveClick: async () => {
      try {
        const r = await call("revert", { body: { commit } });
        message.success(`Reverted; main is now ${r.commit.slice(0, 10)}`);
        await Promise.all([load(), refreshStatus()]);
      } catch (e) {
        notifyError("Revert failed", e);
      }
    },
  });
}

const columns: DataTableColumns<Proposal> = [
  { title: "Title", key: "title", ellipsis: { tooltip: true } },
  { title: "From", key: "instance", width: 130, ellipsis: { tooltip: true } },
  { title: "Kind", key: "kind", width: 75 },
  {
    title: "Status",
    key: "status",
    width: 120,
    render: (p) => h(NTag, { size: "small", bordered: false, type: statusType(p.status) }, () => p.status.replace("_", " ")),
  },
  { title: "Verdict", key: "verdict", width: 90, render: (p) => p.verdict ?? "" },
  { title: "Updated", key: "updated_at", width: 110, render: (p) => h("span", { title: p.updated_at }, ago(p.updated_at)) },
  {
    title: "",
    key: "actions",
    width: 140,
    render: (p) =>
      h(NSpace, { size: "small" }, () => [
        h(NButton, { size: "tiny", onClick: () => void show(p) }, () => "Details"),
        p.merged_commit
          ? h(NButton, { size: "tiny", type: "warning", ghost: true, onClick: () => revert(p.merged_commit!, p.title) }, () => "Revert")
          : null,
      ]),
  },
];
</script>

<template>
  <div style="height: 100%; padding: 8px 12px; display: flex; flex-direction: column; min-height: 0; gap: 8px">
    <div><NButton size="small" :loading="loading" @click="load">Refresh</NButton></div>
    <NDataTable
      :columns="columns"
      :data="proposals"
      :loading="loading"
      :row-key="(p: Proposal) => p.id"
      size="small"
      flex-height
      style="flex: 1; min-height: 0"
      :scroll-x="820"
    />
    <NDrawer v-model:show="open" :width="640" style="max-width: 100vw">
      <NDrawerContent :title="detail?.title ?? 'Proposal'" closable>
        <NSpin v-if="loadingDetail" />
        <NSpace v-else-if="detail" vertical>
          <NSpace size="small" align="center">
            <NTag :type="statusType(detail.status)" :bordered="false">{{ detail.status.replace("_", " ") }}</NTag>
            <NText v-if="detail.branch" depth="3"><code>{{ detail.branch }}</code></NText>
            <NText v-if="detail.merged_commit" depth="3">merged as <code>{{ detail.merged_commit.slice(0, 10) }}</code></NText>
          </NSpace>
          <template v-if="detail.summary">
            <strong>Deployment summary</strong>
            <ChangeSummary :markdown="detail.summary" />
          </template>
          <template v-if="detail.verdict || detail.notes">
            <strong>Review</strong>
            <NText v-if="detail.verdict">Verdict: {{ detail.verdict }}</NText>
            <MarkdownText v-if="detail.notes" :source="detail.notes" />
          </template>
          <template v-if="detail.gate_report">
            <strong>Gate report</strong>
            <pre class="mono">{{ detail.gate_report }}</pre>
          </template>
          <NButton v-if="detail.merged_commit" type="warning" ghost @click="revert(detail.merged_commit, detail.title)">
            Revert
          </NButton>
        </NSpace>
      </NDrawerContent>
    </NDrawer>
  </div>
</template>
