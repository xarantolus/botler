<script setup lang="ts">
// A chat's session as a tree (lib/sessionTree.ts): one list per chain, the other branches nested
// where they split, the active branch highlighted. Pick an entry (arrows, Enter) to see it and
// act on it: continue from here, label it, or fork the conversation up to it into a new chat.
import { computed, ref, shallowRef, watch } from "vue";
import { useRouter } from "vue-router";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import type { SessionTree } from "../../generated";
import { forkable, kindLabel, shapeTree, type TreeRow } from "../../lib/sessionTree";
import { notifyError, notifySuccess } from "../../notify";
import { refreshInstances, refreshSession } from "../../store";
import TimeAgo from "../common/TimeAgo.vue";

const props = defineProps<{ name: string }>();
const open = defineModel<boolean>("open", { default: false });
const router = useRouter();
const tree = shallowRef<SessionTree | null>(null);
const selected = ref<TreeRow | undefined>(undefined);
const busy = ref<string | null>(null);

async function load() {
  try {
    tree.value = await call("session_tree", { body: { chat: props.name } });
  } catch (e) {
    notifyError("Could not load the session tree", e);
  }
}
watch([open, () => props.name], ([o]) => o && void load(), { immediate: true });

const rows = computed(() => (tree.value ? shapeTree(tree.value) : []));
/** The selected entry as it is now (after a reload). */
const entry = computed(() => {
  const find = (rs: TreeRow[]): TreeRow | undefined => {
    for (const r of rs) {
      if (r.id === selected.value?.id) return r;
      const inner = r.children && find(r.children);
      if (inner) return inner;
    }
    return undefined;
  };
  const r = find(rows.value);
  return r?.node ? r : undefined;
});

const ICON: Record<string, string> = {
  user: "i-lucide-user",
  assistant: "i-lucide-bot",
  tool_result: "i-lucide-wrench",
  note: "i-lucide-sticky-note",
  compaction: "i-lucide-fold-vertical",
};
const icon = (r: TreeRow) => (r.children ? "i-lucide-git-branch" : (ICON[r.node?.kind ?? ""] ?? "i-lucide-dot"));

async function act(what: string, f: () => Promise<void>) {
  busy.value = what;
  try {
    await f();
  } finally {
    busy.value = null;
  }
}

async function continueHere(r: TreeRow) {
  const ok = await confirm({
    title: "Continue from here?",
    description:
      "The agent's next turn starts from this entry. What came after it is not lost: it stays in the tree as another branch, and you can switch back to it the same way. Only while the agent is idle.",
    confirmLabel: "Continue from here",
  });
  if (!ok) return;
  await act("goto", async () => {
    try {
      await call("goto_entry", { body: { chat: props.name, entry: r.id } });
      notifySuccess("Continuing from here", r.label);
      await Promise.all([load(), refreshSession(props.name)]);
    } catch (e) {
      notifyError("Could not move there", e);
    }
  });
}

async function label(r: TreeRow) {
  const ok = await confirm({
    title: r.node?.label ? "Change the label" : "Label this entry",
    description: "A name for this point of the conversation. Leave it empty to remove the label.",
    confirmLabel: "Save",
    reason: { label: "Label", placeholder: r.node?.label ?? "e.g. before the refactor" },
  });
  if (!ok) return;
  await act("label", async () => {
    try {
      await call("label_entry", { body: { chat: props.name, entry: r.id, label: ok.reason || undefined } });
      await load();
    } catch (e) {
      notifyError("Could not label the entry", e);
    }
  });
}

async function fork(r: TreeRow) {
  const ok = await confirm({
    title: "Fork into a new chat?",
    description: "A new chat of the same kind starts with this conversation up to and including this answer. This chat is not changed.",
    confirmLabel: "Fork",
  });
  if (!ok) return;
  await act("fork", async () => {
    try {
      const f = await call("fork_chat", { body: { chat: props.name, entry: r.id } });
      notifySuccess("Forked", f.instance);
      await refreshInstances();
      open.value = false;
      await router.push(`/chat/${encodeURIComponent(f.instance)}`);
    } catch (e) {
      notifyError("Could not fork", e);
    }
  });
}
</script>

<template>
  <USlideover v-model:open="open" title="Session tree" :description="name" :ui="{ content: 'sm:max-w-xl', body: 'flex flex-col gap-3 p-0 sm:p-0' }">
    <template #body>
      <div v-if="!tree" class="space-y-2 p-4">
        <USkeleton v-for="n in 6" :key="n" class="h-7 w-full" />
      </div>
      <p v-else-if="!rows.length" class="p-4 text-sm text-muted">This session has no entries yet.</p>
      <template v-else>
        <p class="px-4 pt-3 text-xs text-muted">
          Highlighted: what the agent sees now. Choose an entry (arrow keys, Enter) to continue from it, label it or fork it.
        </p>
        <UTree
          v-model="selected"
          :items="rows"
          :get-key="(r: TreeRow) => r.id"
          label-key="label"
          size="sm"
          aria-label="Session entries"
          class="min-h-0 flex-1 overflow-y-auto px-2"
        >
          <template #item-leading="{ item }">
            <UIcon :name="icon(item)" class="size-4 shrink-0" :class="item.active ? 'text-primary' : 'text-dimmed'" />
          </template>
          <template #item-label="{ item }">
            <span class="truncate" :class="item.active ? 'font-medium text-highlighted' : 'text-muted'">{{ item.label }}</span>
          </template>
          <template #item-trailing="{ item }">
            <UBadge v-if="item.node?.label" color="info" variant="subtle" size="sm" icon="i-lucide-tag" :label="item.node.label" />
            <UBadge v-if="item.leaf" color="primary" variant="subtle" size="sm" label="now" />
            <span v-if="item.children" class="text-xs text-dimmed">{{ item.size }} entries</span>
          </template>
        </UTree>

        <!-- The chosen entry and what can be done with it. -->
        <div v-if="entry?.node" class="border-t border-default bg-elevated/40 p-4" aria-live="polite">
          <div class="mb-1 flex flex-wrap items-center gap-1.5 text-xs text-muted">
            <UBadge color="neutral" variant="subtle" size="sm" :label="kindLabel(entry.node.kind)" />
            <UBadge v-if="entry.active" color="primary" variant="subtle" size="sm" label="on the active branch" />
            <span v-if="entry.node.at"><TimeAgo :at="entry.node.at" /></span>
          </div>
          <p class="mb-3 max-h-32 overflow-y-auto text-sm whitespace-pre-wrap break-words">{{ entry.node.text || "(empty)" }}</p>
          <div class="flex flex-wrap gap-2">
            <UButton
              icon="i-lucide-corner-down-right"
              label="Continue from here"
              size="sm"
              :disabled="entry.leaf || !!busy"
              :loading="busy === 'goto'"
              @click="continueHere(entry)"
            />
            <UButton icon="i-lucide-tag" label="Label…" size="sm" color="neutral" variant="outline" :disabled="!!busy" :loading="busy === 'label'" @click="label(entry)" />
            <UButton
              v-if="forkable(entry.node)"
              icon="i-lucide-git-fork"
              label="Fork into new chat"
              size="sm"
              color="neutral"
              variant="outline"
              :disabled="!!busy"
              :loading="busy === 'fork'"
              @click="fork(entry)"
            />
          </div>
        </div>
      </template>
    </template>
  </USlideover>
</template>
