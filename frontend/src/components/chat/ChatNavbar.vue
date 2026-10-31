<script setup lang="ts">
// The chat's title bar: its name and what it is doing, what it cost, and its actions.
import type { DropdownMenuItem } from "@nuxt/ui";
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import type { Instance } from "../../generated";
import { chatTitle } from "../../lib/events";
import type { Badge } from "../../lib/phase";
import { usd } from "../../lib/time";
import { notifyError, notifySuccess } from "../../notify";
import { inFlight, stageLabel } from "../../lib/proposals";
import { frontDoorName, refreshInstances, refreshSession, state } from "../../store";
import { handoverChat, stopChat } from "../../chatActions";
import PhaseBadge from "../common/PhaseBadge.vue";
import { shell } from "../../shell";
import ConnectionsSlideover from "./ConnectionsSlideover.vue";
import SessionTreeSlideover from "./SessionTreeSlideover.vue";

const props = defineProps<{
  name: string;
  instance: Instance | null;
  badge: Badge | null;
  cost?: number;
  pending?: number;
  /** The agent is working, or messages wait for it: it can be stopped. */
  stoppable?: boolean;
}>();
const router = useRouter();

const title = computed(() => (props.instance ? chatTitle(props.instance, frontDoorName.value) : props.name));
const isFrontDoor = computed(() => props.name === frontDoorName.value);
const connections = ref(false);
/** This chat's proposals still in flight, linked to their review pages. */
const proposals = computed(() => (state.proposals ?? []).filter((p) => p.instance === props.name && inFlight(p)));

async function archive(force: boolean) {
  const ok = await confirm({
    title: force ? "Archive without handover?" : "Archive this chat?",
    description: force
      ? "The chat is archived now, without waiting for the agent's handover report."
      : "The agent writes a handover report (summary, outstanding work, where its state lives), then archives itself. Nothing is deleted.",
    confirmLabel: force ? "Force archive" : "Archive",
    color: force ? "error" : "warning",
    reason: { label: "Reason", placeholder: "Why it is done (shown in the archive)" },
  });
  if (!ok) return;
  try {
    const r = await call("archive_chat", {
      params: { instance: props.name },
      body: { force, reason: ok.reason || undefined },
    });
    notifySuccess(force ? "Archived" : "Archiving", r.note);
    await Promise.all([refreshSession(props.name), refreshInstances()]);
  } catch (e) {
    notifyError("Could not archive", e);
  }
}

async function restore(asGeneric = false): Promise<void> {
  try {
    const r = await call("restore_chat", { params: { instance: props.name }, body: { as_generic: asGeneric } });
    if (r.definition_gone) return await definitionGone(r.definition_gone);
    notifySuccess("Restored", r.changes.length ? `Changed while archived: ${r.changes.join("; ")}` : title.value);
    await Promise.all([refreshSession(props.name), refreshInstances()]);
  } catch (e) {
    notifyError("Could not restore", e);
  }
}

/** Its agent definition is gone: restore it as a generic chat, or delete it. */
async function definitionGone(definition: string): Promise<void> {
  const generic = await confirm({
    title: "Restore as a generic chat?",
    description: `Its agent definition "${definition}" no longer exists. It can come back as a generic chat agent with its history, home and workspace, or be deleted.`,
    confirmLabel: "Restore as chat",
  });
  if (generic) return await restore(true);
  const del = await confirm({
    title: "Delete it instead?",
    description: "Its home, workspace and session are deleted for good. Its events stay in the journal.",
    confirmLabel: "Delete permanently",
    color: "error",
  });
  if (!del) return;
  try {
    await call("delete_chat", { body: { chat: props.name } });
    notifySuccess("Deleted", props.name);
    await refreshInstances();
    await router.push("/");
  } catch (e) {
    notifyError("Could not delete", e);
  }
}

const menu = computed((): DropdownMenuItem[][] => {
  const i = props.instance;
  const info: DropdownMenuItem[] = [
    { type: "label", label: props.name, icon: "i-lucide-at-sign" },
    ...(i ? [{ type: "label" as const, label: `definition ${i.definition}`, icon: "i-lucide-file-code-2" }] : []),
  ];
  const go: DropdownMenuItem[] = [
    {
      label: "Events of this chat",
      icon: "i-lucide-activity",
      onSelect: () => void router.push({ name: "events", query: { chat: props.name } }),
    },
    {
      label: "Copy name",
      icon: "i-lucide-copy",
      onSelect: () =>
        void navigator.clipboard.writeText(props.name).then(
          () => notifySuccess("Copied", props.name),
          (e: unknown) => notifyError("Could not copy", e),
        ),
    },
  ];
  const actions: DropdownMenuItem[] = [];
  if (i?.persistent && i.state !== "archived") {
    actions.push({ label: "Start fresh session (handover)…", icon: "i-lucide-refresh-ccw-dot", onSelect: () => void handoverChat(props.name) });
  }
  if (i?.state === "archived") actions.push({ label: "Restore", icon: "i-lucide-archive-restore", onSelect: () => void restore() });
  else if (i && !isFrontDoor.value) {
    if (i.archive_requested_at) {
      actions.push({ label: "Force archive", icon: "i-lucide-archive-x", color: "error", onSelect: () => void archive(true) });
    } else {
      actions.push({ label: "Archive…", icon: "i-lucide-archive", onSelect: () => void archive(false) });
    }
  }
  return [info, go, ...(actions.length ? [actions] : [])];
});
</script>

<template>
  <UDashboardNavbar :ui="{ title: 'min-w-0', right: 'gap-2' }">
    <template #leading>
      <UDashboardSidebarCollapse />
    </template>
    <template #title>
      <UIcon v-if="isFrontDoor" name="i-lucide-star" class="size-4 shrink-0 text-warning" />
      <span class="truncate" :title="name">{{ title }}</span>
      <PhaseBadge v-if="badge" :badge="badge" />
    </template>
    <template #right>
      <UButton
        v-for="(p, i) in proposals.slice(0, 2)"
        :key="p.id"
        :to="`/proposals/${p.id}`"
        :color="p.status === 'awaiting_user' ? 'warning' : 'info'"
        variant="subtle"
        size="xs"
        icon="i-lucide-git-pull-request-arrow"
        :title="`Proposal: ${p.title}`"
        :class="['max-w-56 shrink-0', i > 0 && 'hidden sm:inline-flex']"
      >
        <span class="truncate">
          {{ stageLabel(p) }}<span class="hidden sm:inline">: {{ p.title }}</span>
        </span>
      </UButton>
      <UTooltip v-if="pending" :text="`${pending} message(s) waiting to be delivered`">
        <UBadge color="info" variant="subtle" icon="i-lucide-inbox" :label="String(pending)" class="hidden sm:inline-flex" />
      </UTooltip>
      <UTooltip v-if="cost" text="What this chat has cost so far">
        <UBadge color="neutral" variant="outline" :label="usd(cost)" class="hidden tabular-nums sm:inline-flex" />
      </UTooltip>
      <UButton
        v-if="instance?.state === 'archived'"
        icon="i-lucide-archive-restore"
        label="Restore"
        color="neutral"
        variant="outline"
        size="sm"
        class="hidden sm:inline-flex"
        @click="restore()"
      />
      <UTooltip v-if="stoppable" text="Stop: abort the turn and cancel what is queued">
        <UButton icon="i-lucide-octagon-x" color="error" variant="soft" size="sm" label="Stop" @click="stopChat(name)" />
      </UTooltip>
      <UTooltip text="Connections: access, subscriptions, related chats">
        <UButton icon="i-lucide-waypoints" color="neutral" variant="ghost" aria-label="Connections" @click="connections = true" />
      </UTooltip>
      <ConnectionsSlideover v-model:open="connections" :name="name" />
      <UTooltip text="Session tree: branches, continue from an earlier point, fork">
        <UButton icon="i-lucide-git-branch" color="neutral" variant="ghost" aria-label="Session tree" @click="shell.sessionTree = true" />
      </UTooltip>
      <SessionTreeSlideover v-model:open="shell.sessionTree" :name="name" />
      <UDropdownMenu :items="menu" :content="{ align: 'end' }">
        <UButton icon="i-lucide-ellipsis-vertical" color="neutral" variant="ghost" aria-label="Chat actions" />
      </UDropdownMenu>
    </template>
  </UDashboardNavbar>
</template>
