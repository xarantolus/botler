<script setup lang="ts">
// One chat: its title bar and banners, the transcript, and the composer.
import { computed, nextTick, onBeforeUnmount, useTemplateRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import ChatBanners from "../components/chat/ChatBanners.vue";
import ChatComposer from "../components/chat/ChatComposer.vue";
import ChatNavbar from "../components/chat/ChatNavbar.vue";
import ChatTranscript from "../components/chat/ChatTranscript.vue";
import { redirectChat } from "../lib/events";
import { currentStatus, phaseBadge } from "../lib/phase";
import { sessionOptions } from "../lib/sessions";
import { buildTranscript, noLive } from "../lib/transcript";
import { watch as watchLive } from "../live";
import { shell } from "../shell";
import {
  allDialogs,
  clearSession,
  frontDoorName,
  refreshSession,
  showSessionFile,
  state,
  waitingByInstance,
} from "../store";

const props = defineProps<{ instance?: string }>();
const route = useRoute();
const router = useRouter();

// A remembered `main` resolves to the front door right away (no fetch of a missing chat).
const name = computed(() =>
  props.instance
    ? (redirectChat(props.instance, state.instances, frontDoorName.value) ?? props.instance)
    : frontDoorName.value,
);
const session = computed(() => (state.session?.instance.name === name.value ? state.session : null));
const inst = computed(() => session.value?.instance ?? state.instances.find((i) => i.name === name.value) ?? null);

// Open the front door when no chat is named, and for a remembered `main`.
watch(
  [frontDoorName, () => props.instance, () => state.instances.length],
  ([door]) => {
    const to = props.instance ? redirectChat(props.instance, state.instances, door) : door;
    if (to && to !== props.instance) void router.replace(`/chat/${encodeURIComponent(to)}`);
  },
  { immediate: true },
);

/** The session file in the link (`?file=`): an earlier session, read only; none is the current. */
const linkedFile = computed(() => (typeof route.query.file === "string" ? route.query.file : null));
watch(
  [name, linkedFile],
  async ([n, file], [old] = [undefined, undefined]) => {
    if (n !== old) {
      clearSession();
      watchLive(n ?? null);
    }
    if (n) await showSessionFile(n, file);
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
/** Showing an earlier session (before a handover), not the current one. */
const earlier = computed(() => !!session.value && session.value.file !== session.value.instance.session_file);
const sessions = computed(() => (session.value ? sessionOptions(session.value.sessions, (d) => d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })) : []));
function showSession(file: string) {
  const current = file === session.value?.instance.session_file;
  void router.replace({ query: { ...route.query, file: current ? undefined : file } });
}

const cost = computed(() => (name.value ? state.usage?.total_by_instance[name.value]?.cost_usd : undefined));

const transcript = computed(() =>
  session.value
    ? buildTranscript({
        items: session.value.items,
        dialogs: allDialogs.value,
        // An earlier session is history: nothing streams or waits in it.
        queued: earlier.value ? [] : session.value.queued,
        streaming: earlier.value ? noLive() : state.streaming,
        busy: session.value.busy,
      })
    : null,
);
const queuedNote = computed(() =>
  badge.value?.type === "error" ? "the agent cannot start, see above" : "delivered when the agent is ready",
);

// ---- scrolling ----
const body = useTemplateRef<HTMLElement>("body");
/** The panel body, which scrolls the transcript. */
const scroller = () => body.value?.closest<HTMLElement>("[data-slot=body]") ?? null;
let atBottom = true;
function onScroll() {
  const el = scroller();
  if (el) atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
}
watch(body, (el, _, onCleanup) => {
  const s = el?.closest<HTMLElement>("[data-slot=body]");
  if (!s) return;
  s.addEventListener("scroll", onScroll, { passive: true });
  onCleanup(() => s.removeEventListener("scroll", onScroll));
});
async function toBottom(force = false) {
  if (!force && !atBottom) return;
  await nextTick();
  const el = scroller();
  if (el) el.scrollTop = el.scrollHeight;
  atBottom = true;
}
/** A link points at a message further up: `?delivery=` (an event trace) or `?entry=` (a search hit). */
const linked = computed(() => typeof route.query.delivery === "string" || typeof route.query.entry === "string");
// New messages keep the view at the end when it was there.
watch(
  () => transcript.value?.messages.length,
  () => !linked.value && void toBottom(),
);
watch(name, () => (atBottom = true));

/** Bring a message (or request) into view and flash it. */
async function reveal(selector: string): Promise<boolean> {
  await nextTick();
  const el = body.value?.querySelector<HTMLElement>(selector);
  if (!el) return false;
  atBottom = false;
  el.scrollIntoView({ block: "center", behavior: "smooth" });
  const target = el.querySelector<HTMLElement>("[data-slot=container]") ?? el;
  target.classList.remove("flash");
  void target.offsetWidth;
  target.classList.add("flash");
  return true;
}

// Scroll to the linked message once it is loaded.
watch(
  () => [route.query.delivery, route.query.entry, transcript.value?.messages.length] as const,
  async ([delivery, entry]) => {
    const t = transcript.value;
    if (!t) return;
    let selector: string | null = null;
    const call = typeof entry === "string" ? t.toolOf[entry] : undefined;
    if (typeof delivery === "string") selector = `[data-delivery="${CSS.escape(delivery)}"]`;
    else if (call) {
      // A hit in a tool call or its result: that call, opened.
      shell.open[call] = true;
      selector = `[data-tool-call="${CSS.escape(call)}"]`;
    } else if (typeof entry === "string" && t.messageOf[entry]) selector = `[data-message="${CSS.escape(t.messageOf[entry])}"]`;
    if (selector && (await reveal(selector))) void router.replace({ query: {} });
  },
  { immediate: true },
);

function showRequest() {
  void reveal("[data-open-dialog]");
}

const composer = useTemplateRef<{ focus: () => void }>("composer");
watch(name, () => void nextTick(() => composer.value?.focus()));
const loading = computed(() => !!name.value && !session.value && !state.sessionError);
</script>

<template>
  <UDashboardPanel id="chat" :ui="{ body: 'p-0 sm:p-0 gap-0' }">
    <template #header>
      <ChatNavbar
        v-if="name"
        :name="name"
        :instance="inst"
        :badge="badge"
        :cost="cost"
        :pending="session?.pending"
        :stoppable="!!session && (session.busy || session.queued.length > 0)"
      />
      <UDashboardNavbar v-else title="Chat">
        <template #leading><UDashboardSidebarCollapse /></template>
      </UDashboardNavbar>
      <!-- Shown only when a banner is: the banners root is always there, comments make :empty miss. -->
      <div v-if="inst" class="hidden border-b border-default px-4 py-3 has-[>div>*]:block sm:px-6">
        <ChatBanners :instance="inst" :status="status" :badge="badge" class="mx-auto max-w-3xl" @show-request="showRequest" />
      </div>
      <!-- Earlier sessions of this chat (one per handover to a fresh session); `?file=` links one. -->
      <div
        v-if="name && session && session.sessions.length > 1"
        class="flex flex-wrap items-center gap-2 border-b border-default px-4 py-1.5 text-xs sm:px-6"
        :class="earlier ? 'bg-warning/10 text-default' : 'text-muted'"
      >
        <UIcon name="i-lucide-history" class="size-4 shrink-0" />
        <USelect
          :model-value="session.file"
          :items="sessions"
          value-key="value"
          size="xs"
          variant="ghost"
          aria-label="Session of this chat"
          class="min-w-56"
          @update:model-value="(f: string) => showSession(f)"
        />
        <UButton size="xs" color="neutral" variant="ghost" icon="i-lucide-git-branch" label="Tree" @click="shell.sessionTree = true" />
        <template v-if="earlier">
          <span>An earlier session, before a handover: read only.</span>
          <UButton size="xs" variant="link" label="Back to the current session" class="p-0" @click="showSession(session.instance.session_file)" />
        </template>
      </div>
    </template>

    <template #body>
      <div ref="body" class="mx-auto flex w-full max-w-3xl flex-1 flex-col px-4 pt-6 sm:px-6">
        <UEmpty
          v-if="!name"
          icon="i-lucide-messages-square"
          title="No chat yet"
          description="The front door appears once the hub has started it."
          variant="naked"
          class="my-auto"
        />
        <UAlert
          v-else-if="state.sessionError && !session"
          color="error"
          variant="subtle"
          icon="i-lucide-circle-alert"
          title="Could not load this chat"
          :description="state.sessionError"
          :actions="[{ label: 'Retry', color: 'error', variant: 'outline', onClick: () => void refreshSession(name!) }]"
        />
        <div v-else-if="loading" class="space-y-6" aria-busy="true" aria-label="Loading the chat">
          <div v-for="n in 3" :key="n" class="space-y-2">
            <USkeleton class="ms-auto h-10 w-2/5 rounded-lg" />
            <USkeleton class="h-4 w-4/5" />
            <USkeleton class="h-4 w-3/5" />
          </div>
        </div>
        <template v-else-if="transcript">
          <p v-if="transcript.unresolvedLinks" class="mb-4 text-center text-xs text-dimmed">
            {{ transcript.unresolvedLinks }} older request(s) are not in the recent list.
          </p>
          <UEmpty
            v-if="!transcript.messages.length"
            icon="i-lucide-message-circle"
            title="No messages yet"
            description="Say hello: the agent wakes when your message arrives."
            variant="naked"
            class="my-auto"
          />
          <ChatTranscript
            v-else
            :key="name"
            :messages="transcript.messages"
            :status="transcript.status"
            :queued-note="queuedNote"
            :instance="name"
            :start-at-end="!linked"
          />
        </template>
        <div v-if="name" id="chat-composer" class="sticky bottom-0 z-10 mt-auto bg-default pb-4 pt-2 sm:pb-6">
          <ChatComposer
            ref="composer"
            :instance="name"
            :archived="inst?.state === 'archived'"
            :hint="earlier ? 'You are reading an earlier session; messages go to the current one.' : undefined"
            :busy="!!session?.busy || badge?.busy === true"
            @sent="toBottom(true)"
          />
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
