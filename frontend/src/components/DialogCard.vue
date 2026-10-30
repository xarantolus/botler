<script setup lang="ts">
// One dialog (question, credentials or approval). Open dialogs are interactive; answered,
// aborted and expired ones show their outcome read-only.
import { NAlert, NButton, NCard, NInput, NSpace, NTag, NText } from "naive-ui";
import { computed, reactive, ref } from "vue";
import { call } from "../api/client";
import type { Dialog, DialogView, SecretInput } from "../generated";
import { ago } from "../lib/time";
import { message, notifyError } from "../notify";
import { refreshDialogs, refreshStatus } from "../store";
import ChangeSummary from "./ChangeSummary.vue";
import MarkdownText from "./MarkdownText.vue";
import SecretValueInput from "./SecretValueInput.vue";

const props = defineProps<{ dialog: Dialog | DialogView; showChat?: boolean }>();

const d = computed(() => props.dialog);
const open = computed(() => d.value.status === "open");
const view = computed(() => d.value as DialogView);
const busy = ref(false);
const free = ref("");
const values = reactive<Record<string, SecretInput | null>>({});

const kindLabel = computed(() => ({ question: "question", approval: "approval", credentials: "secrets" })[d.value.kind]);
const statusType = computed(() =>
  d.value.status === "answered" ? "success" : d.value.status === "open" ? "warning" : "default",
);

async function done() {
  await Promise.all([refreshDialogs(), refreshStatus()]);
}

async function answer(text: string) {
  if (!text.trim()) return;
  busy.value = true;
  try {
    await call("answer_dialog", { params: { id: d.value.id }, body: { answer: text.trim() } });
    message.success("Answered");
    free.value = "";
    await done();
  } catch (e) {
    notifyError("Could not answer", e);
  } finally {
    busy.value = false;
  }
}

async function submitSecrets(decline: boolean) {
  busy.value = true;
  try {
    const filled: Record<string, SecretInput> = {};
    for (const [k, v] of Object.entries(values)) if (v !== null) filled[k] = v;
    await call("submit_secrets", { params: { id: d.value.id }, body: { values: filled, decline } });
    message.success(decline ? "Declined" : "Saved");
    await done();
  } catch (e) {
    notifyError(decline ? "Could not decline" : "Could not save the secrets", e);
  } finally {
    busy.value = false;
  }
}

const formComplete = computed(() => (view.value.form ?? []).every((f) => values[f.name]));
</script>

<template>
  <NCard size="small" :class="{ closed: !open }" :segmented="{ content: true }">
    <template #header>
      <div style="display: flex; flex-direction: column; gap: 2px">
        <NSpace size="small" align="center">
          <NTag size="small" :type="d.kind === 'approval' ? 'error' : 'info'" :bordered="false">{{ kindLabel }}</NTag>
          <NTag v-if="d.priority === 'blocking' && open" size="small" type="warning" :bordered="false">blocking</NTag>
          <NTag v-if="!open" size="small" :type="statusType" :bordered="false">{{ d.status }}</NTag>
          <NText depth="3" style="font-size: 12px">
            <template v-if="showChat">
              from <RouterLink :to="`/chat/${encodeURIComponent(d.instance)}`">{{ d.instance }}</RouterLink> ·
            </template>
            {{ ago(d.created_at) }}
          </NText>
        </NSpace>
        <strong style="font-size: 14px">{{ d.question }}</strong>
      </div>
    </template>

    <NSpace vertical>
      <template v-if="d.detail">
        <ChangeSummary v-if="d.kind === 'approval'" :markdown="d.detail" />
        <MarkdownText v-else :source="d.detail" />
      </template>

      <!-- Outcome of a closed dialog. -->
      <template v-if="!open">
        <NText v-if="d.answer"><strong>Answer:</strong> {{ d.answer }}</NText>
        <NText v-if="d.abort_reason" depth="3">{{ d.status === "expired" ? "Expired" : "Withdrawn" }}: {{ d.abort_reason }}</NText>
      </template>

      <!-- Credentials form. -->
      <template v-else-if="d.kind === 'credentials'">
        <NTag v-if="view.new_group" size="small" :bordered="false">new group {{ view.group }}</NTag>
        <div v-for="f in view.form ?? []" :key="f.name">
          <div style="font-size: 13px; margin-bottom: 4px">
            <code>{{ f.name }}</code>
            <NText depth="3"> · {{ f.file ? `file ${f.file}` : "environment variable" }}</NText>
            <div v-if="f.description"><NText depth="3">{{ f.description }}</NText></div>
          </div>
          <SecretValueInput v-model="values[f.name]!" :file="!!f.file" />
        </div>
        <NAlert v-if="view.remove?.length" type="warning" :show-icon="false">
          Will remove <code v-for="n in view.remove" :key="n" style="margin-right: 6px">{{ n }}</code> and delete
          their values.
        </NAlert>
        <NSpace>
          <NButton type="primary" :loading="busy" :disabled="!formComplete" @click="submitSecrets(false)">Save</NButton>
          <NButton :disabled="busy" @click="submitSecrets(true)">Decline</NButton>
        </NSpace>
      </template>

      <!-- Approvals and questions with choices. -->
      <NSpace v-else-if="d.ctas.length">
        <NButton
          v-for="c in d.ctas"
          :key="c"
          :type="c === 'approve' ? 'primary' : c === 'reject' ? 'error' : 'default'"
          :ghost="c === 'reject'"
          :loading="busy"
          @click="answer(c)"
        >
          {{ c }}
        </NButton>
      </NSpace>

      <!-- Free-text question. -->
      <div v-else style="display: flex; gap: 8px">
        <NInput
          v-model:value="free"
          type="textarea"
          :autosize="{ minRows: 1, maxRows: 6 }"
          placeholder="Your answer"
          @keydown.enter.exact.prevent="answer(free)"
        />
        <NButton type="primary" :loading="busy" :disabled="!free.trim()" @click="answer(free)">Answer</NButton>
      </div>
    </NSpace>
  </NCard>
</template>

<style scoped>
.closed {
  opacity: 0.6;
}
</style>
