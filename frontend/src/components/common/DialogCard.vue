<script setup lang="ts">
// One dialog (a question, a credentials request, an access request or an approval), shown inline in its chat and in
// the requests inbox. Open dialogs are interactive; answered, aborted and expired ones show their
// outcome read-only.
import { computed, reactive, ref } from "vue";
import { call } from "../../api/client";
import { confirmDecision } from "../../confirm";
import type { Dialog, DialogView, SecretInput } from "../../generated";
import { dialogStatusColor } from "../../lib/runs";
import { notifyError, notifySuccess } from "../../notify";
import { refreshDialogs, refreshStatus } from "../../store";
import ChangeSummary from "./ChangeSummary.vue";
import MarkdownText from "./MarkdownText.vue";
import SecretValueInput from "./SecretValueInput.vue";
import TimeAgo from "./TimeAgo.vue";

const props = defineProps<{ dialog: Dialog | DialogView; showChat?: boolean }>();

const d = computed(() => props.dialog);
const open = computed(() => d.value.status === "open");
const view = computed(() => d.value as DialogView);
const busy = ref<string | null>(null);
const free = ref("");
const values = reactive<Record<string, SecretInput | null | undefined>>({});

const KIND = {
  question: { label: "Question", icon: "i-lucide-message-circle-question", color: "info" },
  approval: { label: "Approval", icon: "i-lucide-shield-question", color: "primary" },
  credentials: { label: "Secrets", icon: "i-lucide-key-round", color: "info" },
  access: { label: "Access", icon: "i-lucide-shield-plus", color: "warning" },
  visit: { label: "Visit", icon: "i-lucide-external-link", color: "info" },
} as const satisfies Record<DialogView["kind"], unknown>;
const kind = computed(() => KIND[d.value.kind]);

async function done() {
  await Promise.all([refreshDialogs(), refreshStatus()]);
}

async function answer(text: string, reason?: string) {
  const answer = text.trim();
  if (!answer || busy.value) return;
  busy.value = answer;
  try {
    await call("answer_dialog", { params: { id: d.value.id }, body: { answer, reason } });
    notifySuccess("Answered", `${d.value.question} → ${answer}`);
    free.value = "";
    await done();
  } catch (e) {
    notifyError("Could not answer", e);
  } finally {
    busy.value = null;
  }
}

async function submitSecrets(decline: boolean) {
  busy.value = decline ? "decline" : "save";
  try {
    const filled: Record<string, SecretInput> = {};
    for (const [k, v] of Object.entries(values)) if (v !== null && v !== undefined) filled[k] = v;
    await call("submit_secrets", { params: { id: d.value.id }, body: { values: filled, decline } });
    notifySuccess(decline ? "Declined" : "Secrets saved", d.value.question);
    await done();
  } catch (e) {
    notifyError(decline ? "Could not decline" : "Could not save the secrets", e);
  } finally {
    busy.value = null;
  }
}

/** How a secret is delivered; keys under `~/.ssh` are called what they are. */
function fieldKind(file: string | null | undefined): string {
  if (!file) return "Environment variable";
  return file.includes("/.ssh/") ? `SSH private key at ${file}` : `File ${file}`;
}

/** Approving a change takes an optional comment, rejecting it a reason (the agent reads both). */
async function onCta(c: string) {
  if (d.value.kind !== "approval" || (c !== "approve" && c !== "reject")) return answer(c);
  const ok = await confirmDecision(c === "approve");
  if (ok) await answer(c, ok.reason || undefined);
}

/** A visit request's page, on the published port of this host (as the browser reaches it). */
const visitUrl = computed(() => {
  const v = view.value.visit;
  return v ? `${window.location.protocol}//${window.location.hostname}:${v.host_port}${v.path}` : null;
});

const formComplete = computed(() => (view.value.form ?? []).every((f) => values[f.name]));

function ctaButton(c: string) {
  if (c === "approve" || c === "allow for this chat")
    return { color: "primary" as const, variant: "solid" as const, icon: "i-lucide-check" };
  if (c === "reject" || c === "deny") return { color: "error" as const, variant: "soft" as const, icon: "i-lucide-x" };
  return { color: "neutral" as const, variant: "outline" as const, icon: undefined };
}

function onFreeKey(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void answer(free.value);
  }
}
</script>

<template>
  <UCard
    :variant="open ? 'outline' : 'subtle'"
    :class="[!open && 'opacity-75', open && d.priority === 'blocking' && 'ring-warning/50']"
    :ui="{ header: 'p-3 sm:px-4', body: 'p-3 sm:p-4', footer: 'p-3 sm:px-4' }"
    :aria-label="`${kind.label}: ${d.question}`"
  >
    <template #header>
      <div class="flex flex-wrap items-center gap-1.5 text-xs text-muted">
        <UBadge :color="kind.color" variant="subtle" size="sm" :icon="kind.icon" :label="kind.label" />
        <UBadge v-if="d.priority === 'blocking' && open" color="warning" variant="subtle" size="sm" label="blocking" />
        <UBadge v-if="!open" :color="dialogStatusColor(d.status)" variant="subtle" size="sm" :label="d.status" />
        <span class="ms-1">
          <template v-if="showChat">
            from
            <ULink :to="`/chat/${encodeURIComponent(d.instance)}`" class="font-medium text-default hover:underline">
              {{ d.instance }}
            </ULink>
            ·
          </template>
          <TimeAgo :at="d.created_at" />
        </span>
      </div>
      <h3 class="mt-1.5 text-sm font-semibold text-highlighted">{{ d.question }}</h3>
    </template>

    <div class="flex flex-col gap-3">
      <template v-if="d.detail">
        <ChangeSummary v-if="d.kind === 'approval'" :markdown="d.detail" />
        <MarkdownText v-else :source="d.detail" class="text-sm" />
      </template>

      <!-- Outcome of a closed dialog. -->
      <template v-if="!open">
        <p v-if="d.answer" class="text-sm">
          <span class="text-muted">Answer:</span> <strong class="text-highlighted">{{ d.answer }}</strong>
          <ULink v-if="d.proposal_id" :to="`/proposals/${d.proposal_id}`" class="ms-2 text-primary hover:underline">
            Review
          </ULink>
        </p>
        <p v-if="d.abort_reason" class="text-sm text-muted">
          {{ d.status === "expired" ? "Expired" : "Withdrawn" }}: {{ d.abort_reason }}
        </p>
      </template>

      <!-- Credentials form. -->
      <form
        v-else-if="d.kind === 'credentials'"
        class="flex flex-col gap-4"
        @submit.prevent="formComplete && submitSecrets(false)"
      >
        <div v-if="view.group" class="flex items-center gap-2 text-sm text-muted">
          Group <code class="font-mono text-default">{{ view.group }}</code>
          <UBadge v-if="view.new_group" color="info" variant="subtle" size="sm" label="new" />
        </div>
        <UFormField
          v-for="f in view.form ?? []"
          :key="f.name"
          :name="f.name"
          :description="[fieldKind(f.file), f.description].filter(Boolean).join(' · ')"
          :ui="{ label: 'font-mono' }"
          :label="f.name"
          required
        >
          <SecretValueInput :id="`${d.id}-${f.name}`" v-model="values[f.name]" :file="!!f.file" />
        </UFormField>
        <UAlert v-if="view.remove?.length" color="warning" variant="subtle" icon="i-lucide-trash-2" title="Also removes">
          <template #description>
            <code v-for="n in view.remove" :key="n" class="me-2 font-mono">{{ n }}</code> and deletes their values.
          </template>
        </UAlert>
        <div class="flex flex-wrap justify-end gap-2">
          <UButton
            color="neutral"
            variant="ghost"
            label="Decline"
            :loading="busy === 'decline'"
            :disabled="!!busy"
            @click="submitSecrets(true)"
          />
          <UButton
            type="submit"
            icon="i-lucide-lock"
            label="Save secrets"
            :loading="busy === 'save'"
            :disabled="!formComplete || !!busy"
          />
        </div>
      </form>

      <!-- A page to open (e.g. a login), then done or cancel. -->
      <template v-else-if="d.kind === 'visit' && view.visit">
        <MarkdownText v-if="view.visit.instructions" :source="view.visit.instructions" class="text-sm" />
        <div class="flex flex-wrap items-center justify-end gap-2">
          <UButton
            :to="visitUrl ?? undefined"
            target="_blank"
            rel="noopener noreferrer"
            icon="i-lucide-external-link"
            :label="`Open ${view.visit.title}`"
            class="me-auto"
          />
          <UButton color="neutral" variant="ghost" label="Cancel" :loading="busy === 'cancel'" :disabled="!!busy" @click="answer('cancel')" />
          <UButton icon="i-lucide-check" label="Done" :loading="busy === 'done'" :disabled="!!busy" @click="answer('done')" />
        </div>
        <p class="text-xs text-dimmed">
          <code class="font-mono">{{ visitUrl }}</code> · service {{ view.visit.service }}
        </p>
      </template>

      <!-- Approvals and questions with choices. -->
      <div v-else-if="d.ctas.length" class="flex flex-wrap justify-end gap-2">
        <UButton
          v-if="d.proposal_id"
          :to="`/proposals/${d.proposal_id}`"
          icon="i-lucide-file-diff"
          label="Review changes"
          color="neutral"
          variant="outline"
          class="me-auto"
        />
        <UButton
          v-for="c in d.ctas"
          :key="c"
          v-bind="ctaButton(c)"
          :label="c"
          class="capitalize"
          :loading="busy === c"
          :disabled="!!busy && busy !== c"
          @click="onCta(c)"
        />
      </div>

      <!-- Free-text question. -->
      <form v-else class="flex items-end gap-2" @submit.prevent="answer(free)">
        <UTextarea
          v-model="free"
          :aria-label="`Answer: ${d.question}`"
          placeholder="Your answer"
          autoresize
          :rows="1"
          :maxrows="6"
          class="flex-1"
          @keydown="onFreeKey"
        />
        <UButton type="submit" icon="i-lucide-send-horizontal" label="Answer" :loading="!!busy" :disabled="!free.trim()" />
      </form>
    </div>
  </UCard>
</template>
