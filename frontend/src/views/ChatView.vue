<script setup lang="ts">
// One chat: its title bar and banners, the transcript, and the composer.
import { computed, nextTick, onBeforeUnmount, ref, useTemplateRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import ChatBanners from "../components/chat/ChatBanners.vue";
import ChatComposer from "../components/chat/ChatComposer.vue";
import ChatNavbar from "../components/chat/ChatNavbar.vue";
import ChatTranscript from "../components/chat/ChatTranscript.vue";
import MarkdownText from "../components/common/MarkdownText.vue";
import { chatName, chatPath } from "../lib/events";
import { currentStatus, phaseBadge } from "../lib/phase";
import { sessionOptions } from "../lib/sessions";
import { noLive, sessionTranscript, withInFlight } from "../lib/transcript";
import { watch as watchLive } from "../live";
import { shell } from "../shell";
import {
  allDialogs,
  clearSession,
  frontDoorName,
  refreshSession,
  showEarlier,
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
    ? chatName(props.instance, state.instances, frontDoorName.value)
    : frontDoorName.value,
);
const session = computed(() => (state.session?.instance.name === name.value ? state.session : null));
const inst = computed(() => session.value?.instance ?? state.instances.find((i) => i.name === name.value) ?? null);

// Open the front door when no chat is named, and for a remembered `main`.
watch(
  [frontDoorName, () => props.instance, () => state.instances.map((i) => i.slug ?? i.name).join()],
  ([door]) => {
    // Every chat's address is its slug once it has one; its name and `main` lead there.
    const target = props.instance ? chatName(props.instance, state.instances, door) : door;
    if (!target || !state.instances.some((i) => i.name === target)) return;
    const to = chatPath(target, state.instances);
    if (to !== `/chat/${encodeURIComponent(props.instance ?? "")}`) void router.replace({ path: to, query: route.query });
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

// Two steps: a streamed chunk rebuilds only the second, which shares the first's messages.
const sessionMessages = computed(() =>
  session.value
    ? sessionTranscript(session.value.items, allDialogs.value, { instance: session.value.instance.name, file: session.value.file })
    : null,
);
const transcript = computed(() =>
  session.value && sessionMessages.value
    ? withInFlight(sessionMessages.value, session.value.items, {
        // An earlier session is history: nothing streams or waits in it.
        queued: earlier.value ? [] : session.value.queued,
        streaming: earlier.value ? noLive() : state.streaming,
        busy: session.value.busy,
      })
    : null,
);
/** Why held messages wait, when it is not the usual (shown under them). */
const queuedNote = computed(() => (badge.value?.type === "error" ? "the agent cannot start, see above" : null));

// ---- scrolling ----
// Only this follows new content, and only while the view is at the end: when the transcript
// grows, an end stays the end; anywhere else the position stays put (the virtual list corrects it
// when messages above get their real height; the browser's scroll anchoring is off, or both would
// move it). The kit's own scrolling is off (ChatTranscript).
const body = useTemplateRef<HTMLElement>("body");
const transcriptView = useTemplateRef<{ scrollToMessage: (id: string) => Promise<boolean> }>("transcriptView");
/** The panel body, which scrolls the transcript. */
const scroller = () => body.value?.closest<HTMLElement>("[data-slot=body]") ?? null;
/** Within this of the end counts as at the end. */
const NEAR_END = 48;
const atBottom = ref(true);
/** The transcript grew while the user read further up. */
const unseen = ref(false);
/** When the user last scrolled themselves (wheel, touch, keys, the scrollbar). */
let userScrolled = 0;
const USER_SCROLL = ["wheel", "touchmove", "keydown", "pointerdown"] as const;
function onUserScroll() {
  userScrolled = Date.now();
}
function onScroll() {
  const el = scroller();
  if (!el) return;
  const near = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_END;
  if (near) {
    atBottom.value = true;
    unseen.value = false;
  } else if (Date.now() - userScrolled < 1000) {
    atBottom.value = false;
  } else if (atBottom.value) {
    // Not the user: rendered messages got their real height. The end stays the end.
    el.scrollTop = el.scrollHeight;
  }
}
watch(body, (el, _, onCleanup) => {
  const s = el?.closest<HTMLElement>("[data-slot=body]");
  if (!el || !s) return;
  let height = 0;
  s.style.overflowAnchor = "none";
  const grown = new ResizeObserver(() => {
    // Not while the user scrolls: messages measured above would pull them back to the end.
    if (atBottom.value && Date.now() - userScrolled >= 1000) s.scrollTop = s.scrollHeight;
    else if (s.scrollHeight > height) unseen.value = true;
    height = s.scrollHeight;
  });
  grown.observe(el);
  s.addEventListener("scroll", onScroll, { passive: true });
  for (const e of USER_SCROLL) s.addEventListener(e, onUserScroll, { passive: true });
  onCleanup(() => {
    grown.disconnect();
    s.removeEventListener("scroll", onScroll);
    for (const e of USER_SCROLL) s.removeEventListener(e, onUserScroll);
  });
});
async function toBottom(smooth = false) {
  await nextTick();
  scroller()?.scrollTo({ top: scroller()!.scrollHeight, behavior: smooth ? "smooth" : "instant" });
  atBottom.value = true;
  unseen.value = false;
}
/** A link points at a message further up: `?delivery=` (an event trace) or `?entry=` (a search hit). */
const linked = computed(() => typeof route.query.delivery === "string" || typeof route.query.entry === "string");
// A chat opens at its end, unless a link points further up.
watch(name, () => (atBottom.value = !linked.value), { immediate: true });

/** Bring a message (or request) into view and flash it. */
async function reveal(selector: string, behavior: ScrollBehavior = "smooth"): Promise<boolean> {
  await nextTick();
  const el = body.value?.querySelector<HTMLElement>(selector);
  if (!el) return false;
  atBottom.value = false;
  el.scrollIntoView({ block: "center", behavior });
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
    let message: string | undefined;
    const call = typeof entry === "string" ? t.toolOf[entry] : undefined;
    if (typeof delivery === "string") {
      selector = `[data-delivery="${CSS.escape(delivery)}"]`;
      message = t.messages.find((m) => m.metadata?.delivery === delivery)?.id;
    } else if (call) {
      // A hit in a tool call or its result: that call, opened.
      shell.open[call] = true;
      selector = `[data-tool-call="${CSS.escape(call)}"]`;
      message = t.messageOf[entry as string];
    } else if (typeof entry === "string" && t.messageOf[entry]) {
      message = t.messageOf[entry];
      selector = `[data-message="${CSS.escape(message)}"]`;
    }
    if (!selector) return;
    if (!message) {
      // Further back than what is loaded: load the whole chat, and this runs again.
      if (session.value?.earlier) void showEarlier(session.value.instance.name, true);
      return;
    }
    // Only messages near the viewport are rendered: scroll it there first, until it stays put.
    atBottom.value = false;
    if (!(await transcriptView.value?.scrollToMessage(message))) return;
    // Settled in place: no smooth scroll, which measuring rows on its way would throw off.
    if (await reveal(selector, "instant")) void router.replace({ query: {} });
  },
  { immediate: true },
);

function showRequest() {
  void reveal("[data-open-dialog]");
}

const composer = useTemplateRef<{ focus: () => void }>("composer");
// Not on touch screens: focusing there pops up the keyboard over the chat.
const coarse = window.matchMedia("(pointer: coarse)");
watch(name, () => void nextTick(() => coarse.matches || composer.value?.focus()));
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
      <!-- Shown only when a banner is: the banners root is always there, comments make :empty miss.
           At most a third of the screen, so the chat below always has room to scroll. -->
      <div v-if="inst" class="hidden max-h-[33dvh] overflow-y-auto border-b border-default px-4 py-3 has-[>div>*]:block sm:px-6">
        <ChatBanners :instance="inst" :status="status" :badge="badge" class="mx-auto max-w-3xl" @show-request="showRequest" />
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
          <div v-if="!transcript.messages.length && !earlier && session?.handover" class="my-auto space-y-3 text-sm">
            <p class="flex items-center gap-2 font-medium text-highlighted">
              <UIcon name="i-lucide-history" class="size-4" /> Continued in a fresh session
            </p>
            <p class="text-muted">
              The agent starts it with this handover report from
              <ULink class="underline" @click="showSession(session.handover.previous)">the previous session</ULink>; it reads it when it next wakes.
            </p>
            <div class="rounded-md border border-default p-3">
              <MarkdownText :source="session.handover.summary" />
              <template v-for="[label, list] in ([['Still open', session.handover.outstanding], ['State', session.handover.state]] as const)" :key="label">
                <template v-if="list.length">
                  <p class="mt-3 text-xs font-medium text-muted">{{ label }}</p>
                  <ul class="ms-4 list-disc">
                    <li v-for="(x, i) in list" :key="i">{{ x }}</li>
                  </ul>
                </template>
              </template>
            </div>
          </div>
          <UEmpty
            v-else-if="!transcript.messages.length"
            icon="i-lucide-message-circle"
            title="No messages yet"
            description="Say hello: the agent wakes when your message arrives."
            variant="naked"
            class="my-auto"
          />
          <ChatTranscript
            v-else
            ref="transcriptView"
            :key="name"
            :earlier="session?.earlier ?? 0"
            :following="atBottom"
            :messages="transcript.messages"
            :status="transcript.status"
            :queued-note="queuedNote"
            :instance="name"
          />
        </template>
        <div v-if="name" id="chat-composer" class="sticky bottom-0 z-10 mt-auto bg-default pb-4 pt-2 sm:pb-6">
          <!-- Just above the composer; the dot: more arrived below. -->
          <UChip
            v-if="!atBottom && transcript?.messages.length"
            :show="unseen"
            inset
            class="absolute -top-10 left-1/2 -translate-x-1/2"
          >
            <UButton
              icon="i-lucide-arrow-down"
              color="neutral"
              variant="outline"
              size="sm"
              aria-label="Scroll to the latest message"
              class="rounded-full bg-default shadow-sm"
              @click="toBottom(true)"
            />
          </UChip>
          <ChatComposer
            ref="composer"
            :instance="name"
            :archived="inst?.state === 'archived'"
            :hint="earlier ? 'You are reading an earlier session; messages go to the current one.' : undefined"
            :busy="!!session?.busy || badge?.busy === true"
            @sent="toBottom()"
          />
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
