<script setup lang="ts">
// Disk use of the data directory, by chat and by kind, largest first, and ways to free it:
// archive, clear a chat's caches, delete an archived chat, collect garbage.
import { computed, onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm } from "../confirm";
import type { ChatStorage, StorageKind, StorageOverview } from "../generated";
import { bytes } from "../lib/time";
import { notifyError, notifySuccess } from "../notify";
import { refreshInstances } from "../store";

const data = shallowRef<StorageOverview | null>(null);
const loading = ref(false);
const busy = ref<string | null>(null);

async function load(refresh = false) {
  loading.value = true;
  try {
    data.value = await call("storage_usage", { body: { refresh } });
  } catch (e) {
    notifyError("Could not measure storage", e);
  } finally {
    loading.value = false;
  }
}
onMounted(() => void load());

const KIND: Record<StorageKind, string> = {
  shared_home: "Shared home",
  image: "Image",
  exports: "Exports",
  logs: "Run logs",
  search: "Search index",
  memory: "Memory",
  database: "Database",
  monorepo: "Monorepo",
  uploads: "Uploads",
};
const largest = computed(() => Math.max(1, ...(data.value?.chats.map((c) => c.total_bytes) ?? [])));

/** Run `action` after confirming, then measure again. */
async function act(key: string, ask: Parameters<typeof confirm>[0], action: () => Promise<{ freed_bytes: number }>) {
  if (!(await confirm(ask))) return;
  busy.value = key;
  try {
    const r = await action();
    notifySuccess(ask.confirmLabel, `${bytes(r.freed_bytes)} freed`);
    await Promise.all([load(true), refreshInstances()]);
  } catch (e) {
    notifyError(`${ask.confirmLabel} failed`, e);
  } finally {
    busy.value = null;
  }
}

async function archive(c: ChatStorage) {
  if (
    !(await confirm({
      title: `Archive ${c.title ?? c.instance}?`,
      description: "The agent hands over first, then its home is cleaned and packed.",
      confirmLabel: "Archive",
      color: "warning",
    }))
  )
    return;
  try {
    const r = await call("archive_chat", { params: { instance: c.instance }, body: {} });
    notifySuccess("Archiving", r.note);
    await refreshInstances();
  } catch (e) {
    notifyError("Could not archive", e);
  }
}

const clearCaches = (c: ChatStorage) =>
  act(
    `clear:${c.instance}`,
    {
      title: `Clear ${c.title ?? c.instance}'s caches?`,
      description: "Deletes files git ignores in its checkouts (node_modules, build output, ...) and ~/.cache. It rebuilds them when needed.",
      confirmLabel: "Clear caches",
      color: "warning",
    },
    () => call("clear_chat_caches", { body: { chat: c.instance } }),
  );

const remove = (c: ChatStorage) =>
  act(
    `delete:${c.instance}`,
    {
      title: `Delete ${c.title ?? c.instance} for good?`,
      description: "Its home, workspace and session are deleted and cannot be restored. Its events stay in the journal.",
      confirmLabel: "Delete permanently",
      color: "error",
    },
    () => call("delete_chat", { body: { chat: c.instance } }),
  );

const collect = () =>
  act(
    "gc",
    {
      title: "Collect garbage?",
      description: "Removes images and exports nothing uses, and run logs older than 30 days.",
      confirmLabel: "Collect garbage",
    },
    () => call("collect_garbage", { body: {} }),
  );
</script>

<template>
  <ViewPanel id="storage" title="Storage" icon="i-lucide-hard-drive">
    <template #right>
      <UButton icon="i-lucide-trash" color="neutral" variant="ghost" label="Collect garbage" :loading="busy === 'gc'" @click="collect" />
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Measure again" :loading="loading" @click="load(true)" />
    </template>
    <div class="mx-auto w-full max-w-4xl space-y-4">
      <div v-if="!data" class="space-y-3">
        <USkeleton v-for="n in 4" :key="n" class="h-12 w-full" />
      </div>
      <template v-else>
        <p class="text-sm text-muted">
          {{ bytes(data.total_bytes) }} in total · measured <TimeAgo :at="data.computed_at" />. Dormant chats are archived after a while
          (<code>auto_archive_after</code>), which cleans and packs their homes.
        </p>

        <UCard :ui="{ body: 'divide-y divide-default p-0 sm:p-0' }">
          <template #header><span class="font-semibold text-highlighted">Chats</span></template>
          <p v-if="!data.chats.length" class="p-4 text-sm text-muted">No chats yet.</p>
          <div v-for="c in data.chats" :key="c.instance" class="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2.5 text-sm">
            <div class="min-w-0 flex-1">
              <ULink :to="`/chat/${encodeURIComponent(c.instance)}`" class="font-medium text-highlighted hover:underline">
                {{ c.title ?? c.instance }}
              </ULink>
              <p class="text-xs text-muted">
                home {{ bytes(c.home_bytes) }}<span v-if="c.packed"> (packed)</span> · workspace {{ bytes(c.workspace_bytes) }} · session
                {{ bytes(c.session_bytes) }}
              </p>
              <UProgress :model-value="(c.total_bytes / largest) * 100" size="xs" class="mt-1 max-w-64" />
            </div>
            <UBadge color="neutral" variant="subtle" size="sm" :label="c.persistent ? 'persistent' : c.state" />
            <span class="w-20 text-right font-medium tabular-nums">{{ bytes(c.total_bytes) }}</span>
            <div class="flex gap-1">
              <UButton
                v-if="c.state !== 'archived' && !c.persistent"
                size="xs"
                color="neutral"
                variant="ghost"
                icon="i-lucide-archive"
                aria-label="Archive"
                @click="archive(c)"
              />
              <UButton
                v-if="c.state === 'dormant' || (c.state === 'archived' && !c.packed)"
                size="xs"
                color="neutral"
                variant="ghost"
                icon="i-lucide-brush-cleaning"
                aria-label="Clear caches"
                :loading="busy === `clear:${c.instance}`"
                @click="clearCaches(c)"
              />
              <UButton
                v-if="c.state === 'archived'"
                size="xs"
                color="error"
                variant="ghost"
                icon="i-lucide-trash-2"
                aria-label="Delete permanently"
                :loading="busy === `delete:${c.instance}`"
                @click="remove(c)"
              />
            </div>
          </div>
        </UCard>

        <UCard :ui="{ body: 'divide-y divide-default p-0 sm:p-0' }">
          <template #header><span class="font-semibold text-highlighted">Everything else</span></template>
          <div v-for="i in data.items" :key="`${i.kind}:${i.name}`" class="flex items-center gap-3 px-4 py-2 text-sm">
            <span class="w-28 shrink-0 text-muted">{{ KIND[i.kind] }}</span>
            <code class="min-w-0 flex-1 truncate font-mono text-xs">{{ i.name }}</code>
            <span class="w-20 text-right tabular-nums">{{ bytes(i.bytes) }}</span>
          </div>
        </UCard>
      </template>
    </div>
  </ViewPanel>
</template>
