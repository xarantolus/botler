<script setup lang="ts">
// One proposal under review: where its pipeline is, why it went the way it did, its deployment
// summary, gate report and diff, and the user's decision (approve once it awaits them, reject
// with a reason at any point before the merge). Updates live as the proposal moves.
import { computed, onMounted, onScopeDispose, ref, shallowRef, watch } from "vue";
import { call } from "../api/client";
import ChangeSummary from "../components/common/ChangeSummary.vue";
import CodeBlock from "../components/common/CodeBlock.vue";
import FileDiffView from "../components/common/FileDiffView.vue";
import MarkdownText from "../components/common/MarkdownText.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm, confirmDecision } from "../confirm";
import type { ProposalDiff, ProposalView } from "../generated";
import { FILE_STATUS, gateVerdict, inFlight, pipeline, rejectable } from "../lib/proposals";
import { proposalStatusColor, proposalStatusLabel } from "../lib/runs";
import { notifyError, notifySuccess } from "../notify";
import { useNow } from "../now";
import { refreshDialogs, refreshStatus, state, upsertProposal } from "../store";

const props = defineProps<{ id: string }>();
const view = shallowRef<ProposalView | null>(null);
const diff = shallowRef<ProposalDiff | null>(null);
const error = ref<string | null>(null);
const busy = ref<"approve" | "reject" | null>(null);
const now = useNow(1000);

const p = computed(() => view.value?.proposal ?? null);
const steps = computed(() => (p.value ? pipeline(p.value, now.value) : null));
const gate = computed(() => (p.value ? gateVerdict(p.value) : null));

async function loadView() {
  try {
    view.value = await call("proposal_view", { params: { id: props.id } });
    error.value = null;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
}

async function loadDiff() {
  try {
    diff.value = await call("proposal_diff", { params: { id: props.id } });
  } catch (e) {
    notifyError("Could not load the diff", e);
  }
}

onMounted(() => void Promise.all([loadView(), loadDiff()]));
watch(
  () => props.id,
  () => {
    view.value = diff.value = null;
    void Promise.all([loadView(), loadDiff()]);
  },
);

// Live: the proposal moved. The diff changes only when the head does (a rebase).
const live = computed(() => state.proposals?.find((x) => x.id === props.id) ?? null);
watch(live, (fresh, old) => {
  if (!fresh || !view.value || fresh.updated_at === view.value.proposal.updated_at) return;
  void loadView();
  if (old && fresh.head_commit !== old.head_commit) void loadDiff();
});

// Split view only where there is room for it.
const wide = ref(false);
const media = window.matchMedia("(min-width: 1024px)");
const onMedia = () => (wide.value = media.matches);
onMedia();
media.addEventListener("change", onMedia);
onScopeDispose(() => media.removeEventListener("change", onMedia));
const layout = ref<"unified" | "split">("split");
const split = computed(() => wide.value && layout.value === "split");

const totals = computed(() => {
  const f = diff.value?.files ?? [];
  return { files: f.length, add: f.reduce((n, x) => n + x.additions, 0), del: f.reduce((n, x) => n + x.deletions, 0) };
});

async function decide(approve: boolean) {
  const pr = p.value;
  if (!pr) return;
  const ok = await confirmDecision(approve, pr.title);
  if (!ok) return;
  busy.value = approve ? "approve" : "reject";
  try {
    view.value = await call("decide_proposal", {
      params: { id: pr.id },
      body: { approve, reason: ok.reason || undefined },
    });
    upsertProposal(view.value.proposal);
    notifySuccess(approve ? "Approved" : "Rejected", pr.title);
    await Promise.all([refreshDialogs(), refreshStatus()]);
  } catch (e) {
    notifyError(approve ? "Could not approve" : "Could not reject", e);
  } finally {
    busy.value = null;
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
    await refreshStatus();
  } catch (e) {
    notifyError("Revert failed", e);
  }
}

const stepColor = computed(() =>
  p.value?.status === "merged" ? "success" : p.value?.status === "rejected" || p.value?.status === "failed" ? "error" : "primary",
);
const fileId = (path: string) => `file-${path.replace(/[^a-zA-Z0-9]/g, "-")}`;
</script>

<template>
  <ViewPanel id="proposal" title="Proposal" icon="i-lucide-file-diff" body-class="p-0 sm:p-0">
    <template #right>
      <UButton to="/proposals" icon="i-lucide-list" color="neutral" variant="ghost" label="All proposals" class="hidden sm:inline-flex" />
    </template>

    <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-circle-alert" :title="error" class="m-4" />
    <div v-else-if="!p" class="space-y-3 p-4">
      <USkeleton class="h-8 w-1/2" />
      <USkeleton class="h-24 w-full" />
      <USkeleton class="h-64 w-full" />
    </div>

    <div v-else class="mx-auto flex w-full max-w-6xl flex-col gap-5 p-4 sm:p-6">
      <!-- Header -->
      <header class="space-y-2">
        <h1 class="text-lg font-semibold text-highlighted">{{ p.title }}</h1>
        <div class="flex flex-wrap items-center gap-2 text-sm text-muted">
          <UBadge :color="proposalStatusColor(p.status)" variant="subtle" :label="proposalStatusLabel(p.status)" />
          <UBadge v-if="gate" :color="gate.color" variant="outline" size="sm" icon="i-lucide-shield-check" :label="`gate ${gate.label}`" />
          <UBadge
            v-if="p.verdict"
            :color="p.verdict === 'approve' ? 'success' : p.verdict === 'reject' ? 'error' : 'warning'"
            variant="outline"
            size="sm"
            icon="i-lucide-user-check"
            :label="`reviewer: ${p.verdict}`"
          />
          <span>
            from
            <ULink :to="`/chat/${encodeURIComponent(p.instance)}`" class="font-medium text-default hover:underline">{{ p.instance }}</ULink>
            · <TimeAgo :at="p.created_at" />
          </span>
          <code v-if="p.branch" class="font-mono text-xs">{{ p.branch }}</code>
          <UButton
            v-if="p.merged_commit"
            size="xs"
            color="warning"
            variant="outline"
            icon="i-lucide-undo-2"
            label="Revert"
            @click="revert(p.merged_commit, p.title)"
          />
        </div>
      </header>

      <!-- Progress -->
      <UStepper
        v-if="steps"
        :items="steps.steps"
        :model-value="steps.current"
        :color="stepColor"
        size="sm"
        disabled
        class="w-full"
        :ui="{ description: 'text-xs' }"
      />

      <!-- Why it went the way it did -->
      <UAlert
        v-if="p.decision"
        :color="p.status === 'rejected' ? 'error' : p.status === 'merged' ? 'success' : 'info'"
        variant="subtle"
        :icon="p.decided_by === 'user' ? 'i-lucide-user' : 'i-lucide-scale'"
        :title="p.decision.charAt(0).toUpperCase() + p.decision.slice(1)"
      >
        <template #description>
          <span v-if="p.decided_at">
            {{ p.decided_by === "user" ? "You" : p.decided_by }} · <TimeAgo :at="p.decided_at" />
          </span>
          <blockquote v-if="p.decision_reason" class="mt-1 border-s-2 border-current/30 ps-2 whitespace-pre-wrap">
            {{ p.decision_reason }}
          </blockquote>
        </template>
      </UAlert>

      <section v-if="p.review_notes" class="flex gap-2.5 rounded-md bg-elevated/50 px-3 py-2.5 text-sm">
        <UIcon name="i-lucide-user-check" class="mt-0.5 size-4 shrink-0 text-muted" />
        <div class="min-w-0">
          <div class="font-medium text-highlighted">Review notes<span v-if="p.reviewer" class="font-normal text-muted"> · {{ p.reviewer }}</span></div>
          <MarkdownText :source="p.review_notes" class="text-muted" />
        </div>
      </section>

      <UCollapsible v-if="view?.summary" :default-open="inFlight(p)" class="rounded-md border border-default">
        <UButton color="neutral" variant="ghost" block class="justify-between px-3" trailing-icon="i-lucide-chevron-down" label="Deployment summary" />
        <template #content>
          <div class="border-t border-default p-3"><ChangeSummary :markdown="view.summary" /></div>
        </template>
      </UCollapsible>

      <UCollapsible v-if="p.gate_report" class="rounded-md border border-default">
        <UButton color="neutral" variant="ghost" block class="justify-between px-3" trailing-icon="i-lucide-chevron-down" label="Gate report" />
        <template #content>
          <div class="border-t border-default p-3"><CodeBlock :text="p.gate_report" max-height="24rem" /></div>
        </template>
      </UCollapsible>

      <!-- Diff -->
      <section class="space-y-3">
        <div class="flex flex-wrap items-center gap-2">
          <h2 class="text-sm font-semibold text-highlighted">Changes</h2>
          <span v-if="diff" class="font-mono text-xs text-muted">
            {{ totals.files }} file(s) <span class="text-success">+{{ totals.add }}</span> <span class="text-error">−{{ totals.del }}</span>
          </span>
          <UFieldGroup v-if="wide" size="xs" class="ms-auto">
            <UButton label="Split" :variant="layout === 'split' ? 'subtle' : 'outline'" color="neutral" @click="layout = 'split'" />
            <UButton label="Unified" :variant="layout === 'unified' ? 'subtle' : 'outline'" color="neutral" @click="layout = 'unified'" />
          </UFieldGroup>
        </div>
        <USkeleton v-if="!diff" class="h-48 w-full" />
        <template v-else>
          <ul class="divide-y divide-default rounded-md border border-default text-sm">
            <li v-for="f in diff.files" :key="f.path">
              <a :href="`#${fileId(f.path)}`" class="flex items-center gap-2 px-3 py-1.5 hover:bg-elevated/50">
                <UIcon :name="FILE_STATUS[f.status].icon" class="size-4 shrink-0" :class="FILE_STATUS[f.status].text" />
                <span class="min-w-0 flex-1 truncate font-mono text-xs">{{ f.path }}</span>
                <UBadge v-if="f.protected" color="warning" variant="subtle" size="sm" label="protected" />
                <span class="font-mono text-xs tabular-nums"><span class="text-success">+{{ f.additions }}</span> <span class="text-error">−{{ f.deletions }}</span></span>
              </a>
            </li>
          </ul>
          <FileDiffView v-for="f in diff.files" :id="fileId(f.path)" :key="f.path" :file="f" :split="split" />
        </template>
      </section>
    </div>

    <!-- Decision bar, while the user can still decide. -->
    <div
      v-if="p && rejectable(p)"
      class="sticky bottom-0 z-10 flex flex-wrap items-center gap-2 border-t border-default bg-default/95 px-4 py-3 backdrop-blur sm:px-6"
    >
      <span class="me-auto text-sm text-muted">
        <span class="hidden sm:inline">{{ p.status === "awaiting_user" ? "Waiting for your decision." : "Checks are running; you can reject it now." }}</span>
      </span>
      <UButton
        color="error"
        variant="soft"
        icon="i-lucide-x"
        label="Reject"
        :loading="busy === 'reject'"
        :disabled="!!busy"
        @click="decide(false)"
      />
      <UButton
        icon="i-lucide-check"
        :label="p.status === 'awaiting_user' ? 'Approve' : 'Approve once checks finish'"
        :loading="busy === 'approve'"
        :disabled="!!busy || p.status !== 'awaiting_user'"
        @click="decide(true)"
      />
    </div>
  </ViewPanel>
</template>
