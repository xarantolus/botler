<script setup lang="ts">
import { NAlert, NButton, NCollapse, NCollapseItem, NEmpty, NInput, NSpace, NSpin, NText } from "naive-ui";
import { computed, h, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import DialogCard from "../components/DialogCard.vue";
import EnvelopeCard from "../components/EnvelopeCard.vue";
import MarkdownText from "../components/MarkdownText.vue";
import PhaseTag from "../components/PhaseTag.vue";
import ToolCallView from "../components/ToolCallView.vue";
import BreakerResume from "../components/BreakerResume.vue";
import WakeProgress from "../components/WakeProgress.vue";
import type { Dialog, SessionItem } from "../generated";
import { envelopeOf, isUserMessage } from "../lib/envelope";
import { heldFor } from "../lib/breakers";
import { chatTitle, linkedDialog, redirectChat } from "../lib/events";
import { currentStatus, phaseBadge } from "../lib/phase";
import { ago, usd } from "../lib/time";
import { watch as watchLive } from "../live";
import { dialog as confirmDialog, notifyError } from "../notify";
import {
  allDialogs,
  clearSession,
  frontDoorName,
  queueSent,
  refreshInstances,
  refreshSession,
  state,
  waitingByInstance,
} from "../store";

const props = defineProps<{ instance?: string }>();
const route = useRoute();
const router = useRouter();

const name = computed(() => props.instance ?? frontDoorName.value);
const session = computed(() => (state.session?.instance.name === name.value ? state.session : null));
const inst = computed(() => session.value?.instance ?? null);

// Open the front door when no chat is named, and for a remembered `main`.
watch(
  [frontDoorName, () => props.instance, () => state.instances.length],
  ([door]) => {
    const to = props.instance ? redirectChat(props.instance, state.instances, door) : door;
    if (to && to !== props.instance) void router.replace(`/chat/${encodeURIComponent(to)}`);
  },
  { immediate: true },
);
const held = computed(() => (name.value ? heldFor(name.value, state.status?.breakers ?? []) : []));

watch(
  name,
  async (n, old) => {
    if (n === old) return;
    clearSession();
    watchLive(n ?? null);
    if (n) await refreshSession(n);
  },
  { immediate: true },
);
onBeforeUnmount(() => watchLive(null));

const status = computed(() =>
  inst.value ? currentStatus(session.value?.phase ?? null, state.liveStatus[inst.value.name] ?? null) : null,
);
const waiting = computed(() => (name.value ? (waitingByInstance.value[name.value] ?? 0) : 0));
const badge = computed(() =>
  inst.value
    ? phaseBadge({
        state: inst.value.state,
        status: status.value,
        busy: session.value?.busy,
        pending: session.value?.pending,
        waitingOnUser: waiting.value,
        archiveRequested: inst.value.archive_requested_at !== null,
      })
    : null,
);
const cost = computed(() => (name.value ? state.usage?.total_by_instance[name.value]?.cost_usd : undefined));

type ToolResult = Extract<SessionItem, { kind: "tool_result" }>;

/** Tool results by the call they answer, so each call shows its result in place. */
const results = computed(() => {
  const m = new Map<string, ToolResult>();
  for (const it of session.value?.items ?? []) if (it.kind === "tool_result" && it.tool_call_id) m.set(it.tool_call_id, it);
  return m;
});
const paired = computed(() => {
  const ids = new Set<string>();
  for (const it of session.value?.items ?? []) {
    if (it.kind !== "assistant") continue;
    for (const c of it.tool_calls) {
      const r = c.id ? results.value.get(c.id) : undefined;
      if (r) ids.add(r.id);
    }
  }
  return ids;
});

/** Dialogs opened by the tool calls of an item (shown right after it). */
function dialogsOf(it: SessionItem): Dialog[] {
  const rs: ToolResult[] =
    it.kind === "assistant"
      ? it.tool_calls.flatMap((c) => (c.id && results.value.get(c.id) ? [results.value.get(c.id)!] : []))
      : it.kind === "tool_result" && !paired.value.has(it.id)
        ? [it]
        : [];
  const out: Dialog[] = [];
  for (const r of rs) {
    const d = linkedDialog(r, allDialogs.value);
    if (d) out.push(d);
  }
  return out;
}

/** Linked dialogs that are older than what the dialogs list holds are fetched with it. */
const unresolvedLinks = computed(() =>
  (session.value?.items ?? []).filter(
    (it) => it.kind === "tool_result" && (it.dialog_id || it.proposal_id) && !linkedDialog(it, allDialogs.value),
  ).length,
);

// ---- scrolling ----
const log = ref<HTMLElement | null>(null);
let stickToBottom = true;
function onScroll() {
  const el = log.value;
  if (el) stickToBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
}
async function scrollToEnd() {
  await nextTick();
  if (log.value && stickToBottom) log.value.scrollTop = log.value.scrollHeight;
}
watch(() => [session.value?.items.length, state.streaming, name.value && state.outbox[name.value]?.length], scrollToEnd);
watch(
  () => [route.query.delivery, session.value?.items.length] as const,
  async ([delivery]) => {
    if (typeof delivery !== "string" || !session.value) return;
    await nextTick();
    const el = log.value?.querySelector(`[data-delivery="${CSS.escape(delivery)}"]`);
    if (!el) return;
    stickToBottom = false;
    el.scrollIntoView({ block: "center" });
    el.classList.remove("flash");
    void (el as HTMLElement).offsetWidth;
    el.classList.add("flash");
    void router.replace({ query: {} });
  },
);
watch(name, () => (stickToBottom = true));

// ---- sending ----
const draft = ref("");
const sending = ref(false);
async function send() {
  const text = draft.value.trim();
  if (!text || !name.value) return;
  sending.value = true;
  try {
    await call("send_message", { body: { to: name.value, text } });
    queueSent(name.value, text);
    draft.value = "";
    stickToBottom = true;
    window.setTimeout(() => name.value && refreshSession(name.value), 400);
  } catch (e) {
    notifyError("Could not send", e);
  } finally {
    sending.value = false;
  }
}
function onKey(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send();
  }
}

// ---- archive ----
const archiveReason = ref("");
async function chatAction(kind: "archive" | "force" | "restore") {
  if (!name.value) return;
  try {
    if (kind === "restore") await call("restore_chat", { params: { instance: name.value } });
    else {
      const r = await call("archive_chat", {
        params: { instance: name.value },
        body: { force: kind === "force", reason: archiveReason.value.trim() || undefined },
      });
      notifyNote(r.note);
    }
    await Promise.all([refreshSession(name.value), refreshInstances()]);
  } catch (e) {
    notifyError(kind === "restore" ? "Could not restore" : "Could not archive", e);
  }
}
function notifyNote(note: string) {
  confirmDialog.info({ title: "Archive", content: note, positiveText: "OK" });
}
function confirmArchive(force: boolean) {
  archiveReason.value = "";
  const text = force
    ? "The chat is archived now, without waiting for the agent's handover report."
    : "The agent writes a handover report (summary, outstanding work, where its state lives), then archives itself. Nothing is deleted.";
  confirmDialog.warning({
    title: force ? "Archive without handover?" : "Archive this chat?",
    content: () =>
      h("div", [
        h("p", text),
        h(NInput, {
          value: archiveReason.value,
          placeholder: "reason (optional)",
          "onUpdate:value": (v: string) => (archiveReason.value = v),
        }),
      ]),
    positiveText: force ? "Force archive" : "Archive",
    negativeText: "Cancel",
    onPositiveClick: () => chatAction(force ? "force" : "archive"),
  });
}
const isFrontDoor = computed(() => name.value === frontDoorName.value);
</script>

<template>
  <div style="display: flex; flex-direction: column; height: 100%; min-height: 0">
    <NEmpty v-if="!name" description="No chat selected." style="margin-top: 20vh" />
    <template v-else>
      <!-- Header -->
      <div class="chat-head">
        <div style="min-width: 0; flex: 1">
          <div style="display: flex; align-items: center; gap: 8px; flex-wrap: wrap">
            <strong style="font-size: 16px">{{ inst ? chatTitle(inst, frontDoorName) : name }}</strong>
            <PhaseTag v-if="badge" :badge="badge" size="medium" />
          </div>
          <NText depth="3" style="font-size: 12px">
            {{ name }}<template v-if="inst"> · {{ inst.definition }}</template>
            <template v-if="cost"> · {{ usd(cost) }}</template>
            <template v-if="session?.pending"> · {{ session.pending }} queued</template>
            <template v-if="badge?.label === 'waiting for you'"> · {{ badge.detail }}</template>
          </NText>
        </div>
        <NSpace v-if="inst" size="small">
          <NButton v-if="inst.state === 'archived'" size="small" @click="chatAction('restore')">Restore</NButton>
          <template v-else-if="!isFrontDoor">
            <NButton v-if="inst.archive_requested_at" size="small" type="warning" ghost @click="confirmArchive(true)">
              Force archive
            </NButton>
            <NButton v-else size="small" @click="confirmArchive(false)">Archive</NButton>
          </template>
        </NSpace>
      </div>

      <!-- Archive state -->
      <NAlert v-if="inst?.archive_report" type="default" :title="inst.archive_report.confirmed_by_agent ? 'Archived' : 'Archived without the agent\'s handover'" class="banner">
        <template v-if="inst.archive_report.report">
          <MarkdownText :source="inst.archive_report.report.summary" />
          <div v-if="inst.archive_report.report.outstanding.length" style="margin-top: 6px">
            <strong>Outstanding</strong>
            <ul style="margin: 2px 0"><li v-for="(o, i) in inst.archive_report.report.outstanding" :key="i">{{ o }}</li></ul>
          </div>
          <div v-if="inst.archive_report.report.state.length">
            <strong>Where its state lives</strong>
            <ul style="margin: 2px 0"><li v-for="(o, i) in inst.archive_report.report.state" :key="i">{{ o }}</li></ul>
          </div>
        </template>
        <div v-if="inst.archive_report.reason">Reason: {{ inst.archive_report.reason }}</div>
        <div v-if="inst.archive_report.handed_to">
          Handed to <RouterLink :to="`/chat/${encodeURIComponent(inst.archive_report.handed_to)}`">{{ inst.archive_report.handed_to }}</RouterLink>
        </div>
        <NText depth="3" style="font-size: 12px">{{ ago(inst.archive_report.at) }} · messaging it restores it</NText>
      </NAlert>
      <NAlert v-else-if="inst?.archive_requested_at" type="warning" class="banner" :show-icon="true">
        Waiting for the agent to hand over before archiving (asked {{ ago(inst.archive_requested_at) }}).
      </NAlert>

      <NAlert v-for="h in held" :key="h.breaker.subject" type="warning" class="banner" title="Messages are held">
        Messages between this chat and <strong>{{ h.other }}</strong> are held by a breaker: {{ h.breaker.reason }}
        <div style="margin-top: 6px"><BreakerResume :subject="h.breaker.subject" /></div>
      </NAlert>

      <WakeProgress
        v-if="inst"
        :instance="inst.name"
        :status="status"
        :starting="inst.state === 'starting'"
      />
      <NAlert v-if="state.sessionError && !session" type="error" class="banner">{{ state.sessionError }}</NAlert>

      <!-- Transcript -->
      <div ref="log" class="log" @scroll="onScroll">
        <div v-if="!session && !state.sessionError" style="text-align: center; padding: 40px"><NSpin /></div>
        <NEmpty v-else-if="session && !session.items.length" description="No messages yet." style="margin-top: 40px" />
        <div v-if="unresolvedLinks" style="font-size: 12px; opacity: 0.6; text-align: center">
          {{ unresolvedLinks }} older request(s) are not in the recent list.
        </div>
        <template v-for="it in session?.items ?? []" :key="it.id">
          <template v-if="it.kind === 'user'">
            <div v-if="envelopeOf(it) && !isUserMessage(envelopeOf(it)!)" class="msg" :data-delivery="it.delivery ?? ''">
              <EnvelopeCard :env="envelopeOf(it)!" :at="it.at" />
            </div>
            <div v-else class="msg user" :data-delivery="it.delivery ?? ''">
              <div class="bubble"><MarkdownText :source="(it.message_text ?? envelopeOf(it)?.body ?? it.text).trim()" /></div>
              <div class="meta" :title="it.at ?? ''">you · {{ ago(it.at) }}</div>
            </div>
          </template>

          <div v-else-if="it.kind === 'assistant'" class="msg">
            <NCollapse v-if="it.thinking">
              <NCollapseItem title="thinking" :name="1">
                <pre class="mono">{{ it.thinking }}</pre>
              </NCollapseItem>
            </NCollapse>
            <MarkdownText v-if="it.text" :source="it.text" />
            <ToolCallView v-for="(c, i) in it.tool_calls" :key="i" :call="c" :result="c.id ? results.get(c.id) : undefined" />
            <NAlert v-if="it.error" type="error" :show-icon="false" style="margin-top: 6px">{{ it.error }}</NAlert>
            <div class="meta" :title="it.at ?? ''">
              agent<template v-if="it.model"> · {{ it.model }}</template>
              <template v-if="it.cost_usd !== null"> · {{ usd(it.cost_usd, 4) }}</template> · {{ ago(it.at) }}
            </div>
          </div>

          <div v-else-if="it.kind === 'tool_result' && !paired.has(it.id)" class="msg">
            <ToolCallView :result="it" />
          </div>

          <div v-else-if="it.kind === 'note'" class="msg note" :data-delivery="it.delivery">
            <span class="note-label">note from {{ it.from }}<template v-if="it.event_kind !== 'message'"> · {{ it.event_kind }}</template> · {{ ago(it.at) }}</span>
            <MarkdownText :source="it.text.trim()" />
          </div>

          <div v-else-if="it.kind === 'compaction'" class="msg">
            <NCollapse>
              <NCollapseItem title="earlier conversation compacted" :name="1">
                <MarkdownText :source="it.summary" />
              </NCollapseItem>
            </NCollapse>
          </div>

          <div v-for="d in dialogsOf(it)" :key="d.id" class="msg">
            <DialogCard :dialog="d" />
          </div>
        </template>

        <div v-for="(q, i) in name ? (state.outbox[name] ?? []) : []" :key="`q${i}`" class="msg user queued">
          <div class="bubble"><MarkdownText :source="q.text" /></div>
          <div class="meta">
            you · queued<template v-if="badge && badge.type === 'error'">: the agent cannot start, see above</template
            ><template v-else>, delivered when the agent is ready</template>
          </div>
        </div>

        <div v-if="state.streaming" class="msg streaming">
          <MarkdownText :source="state.streaming" />
        </div>
        <div v-else-if="session?.busy" class="msg"><NSpin :size="14" /> <NText depth="3">working…</NText></div>
      </div>

      <!-- Composer -->
      <div class="composer">
        <NInput
          v-model:value="draft"
          type="textarea"
          :autosize="{ minRows: 1, maxRows: 8 }"
          :placeholder="inst?.state === 'archived' ? 'Message to restore this chat' : 'Message (Enter sends, Shift+Enter for a new line)'"
          @keydown="onKey"
        />
        <NButton type="primary" :loading="sending" :disabled="!draft.trim()" @click="send">Send</NButton>
      </div>
    </template>
  </div>
</template>

<style scoped>
.chat-head {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 8px 16px;
  border-bottom: 1px solid rgba(127, 127, 127, 0.2);
}
.banner {
  margin: 10px 16px 0;
}
.log {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 16px;
}
.msg {
  max-width: 860px;
  margin: 0 auto 14px;
}
.msg.user {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
}
.bubble {
  background: rgba(47, 91, 211, 0.12);
  padding: 8px 12px;
  border-radius: 12px;
  max-width: 85%;
}
.meta {
  font-size: 12px;
  opacity: 0.55;
  margin-top: 3px;
}
.note {
  font-size: 13px;
  opacity: 0.6;
  border-left: 2px dashed rgba(127, 127, 127, 0.45);
  padding-left: 10px;
}
.note-label {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.03em;
}
.queued {
  opacity: 0.6;
}
.streaming {
  opacity: 0.8;
  border-left: 2px solid rgba(127, 127, 127, 0.4);
  padding-left: 10px;
}
.composer {
  display: flex;
  gap: 8px;
  align-items: flex-end;
  padding: 10px 16px;
  border-top: 1px solid rgba(127, 127, 127, 0.2);
}
</style>
