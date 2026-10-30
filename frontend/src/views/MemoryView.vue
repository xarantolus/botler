<script setup lang="ts">
// Agents' durable memory (Markdown files in namespaces, with history): browse, read, edit and
// add files; a save based on an older version shows the newer one and lets the user choose.
import type { TreeItem } from "@nuxt/ui";
import { computed, onMounted, ref, shallowRef } from "vue";
import { ApiError, call } from "../api/client";
import MarkdownText from "../components/common/MarkdownText.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { MemoryCommit, MemoryEntry, MemoryFile } from "../generated";
import { byNamespace } from "../lib/memory";
import { notifyError, notifySuccess, notifyWarning } from "../notify";

const files = shallowRef<MemoryEntry[] | null>(null);
const loading = ref(false);

const file = shallowRef<MemoryFile | null>(null);
const history = shallowRef<MemoryCommit[] | null>(null);
/** The text being edited; `null` when viewing. */
const draft = ref<string | null>(null);
/** For a new file: its path. */
const newPath = ref<string | null>(null);
/** The version the hub has now, when a save was based on an older one. */
const conflict = shallowRef<MemoryFile | null>(null);
const saving = ref(false);

async function load() {
  loading.value = true;
  try {
    files.value = await call("memory_list", { body: {} });
  } catch (e) {
    notifyError("Could not load memory", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

type FileItem = TreeItem & { path?: string };
const tree = computed((): FileItem[] =>
  byNamespace(files.value ?? []).map((g) => ({
    label: g.namespace,
    defaultExpanded: true,
    children: g.files.map((f) => ({
      label: f.path.slice(g.namespace.length + 1),
      icon: "i-lucide-file-text",
      path: f.path,
      onSelect: () => void open(f.path),
    })),
  })),
);

async function open(path: string) {
  draft.value = null;
  newPath.value = null;
  conflict.value = null;
  history.value = null;
  try {
    file.value = await call("memory_read", { body: { path } });
  } catch (e) {
    notifyError(`Could not open ${path}`, e);
  }
}

async function showHistory() {
  if (!file.value) return;
  try {
    history.value = await call("memory_history", { body: { path: file.value.path } });
  } catch (e) {
    notifyError("Could not load the history", e);
  }
}

function startNew() {
  file.value = null;
  history.value = null;
  conflict.value = null;
  newPath.value = "user/";
  draft.value = "";
}

function cancel() {
  draft.value = null;
  conflict.value = null;
  newPath.value = null;
}

/** Save the draft over `base` (the version it was edited from; none for a new file). */
async function save(base: string | undefined) {
  const path = newPath.value ?? file.value?.path;
  if (draft.value === null || !path) return;
  saving.value = true;
  try {
    const written = await call("memory_write", { body: { path, content: draft.value, expected_sha256: base } });
    notifySuccess(`${path} saved`);
    await load();
    file.value = { path, sha256: written.sha256, content: draft.value };
    draft.value = null;
    newPath.value = null;
    conflict.value = null;
    history.value = null;
  } catch (e) {
    if (e instanceof ApiError && e.status === 409 && e.body?.conflict) {
      conflict.value = e.body.conflict.current;
      if (!conflict.value) {
        // Deleted meanwhile: the next save creates it again.
        notifyWarning(`${path} was deleted meanwhile`, "Saving creates it again.");
        file.value = null;
        newPath.value = path;
      }
    } else {
      notifyError(`Could not save ${path}`, e);
    }
  } finally {
    saving.value = false;
  }
}

/** Save mine over the conflicting version. */
const overwrite = () => save(conflict.value?.sha256);
function takeTheirs() {
  if (!conflict.value) return;
  file.value = conflict.value;
  draft.value = null;
  conflict.value = null;
}
</script>

<template>
  <ViewPanel id="memory" title="Memory" icon="i-lucide-brain" body-class="p-0 sm:p-0 lg:flex-row gap-0 sm:gap-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
      <UButton icon="i-lucide-file-plus" label="New file" size="sm" @click="startNew" />
    </template>

    <aside
      class="max-h-[40%] shrink-0 overflow-y-auto border-b border-default p-2 lg:max-h-none lg:w-72 lg:border-e lg:border-b-0"
      aria-label="Memory files"
    >
      <div v-if="!files" class="space-y-2 p-2">
        <USkeleton v-for="n in 5" :key="n" class="h-5 w-full" />
      </div>
      <UEmpty v-else-if="!files.length" icon="i-lucide-brain" title="No memory yet" size="sm" variant="naked" />
      <UTree v-else :items="tree" :get-key="(i: FileItem) => i.path ?? i.label ?? ''" size="sm" />
    </aside>

    <div class="min-w-0 flex-1 overflow-y-auto p-4 sm:p-6">
      <div class="mx-auto max-w-3xl space-y-4">
        <UEmpty
          v-if="!file && draft === null"
          icon="i-lucide-notebook-text"
          title="Pick a file"
          variant="naked"
          class="py-10"
        >
          <template #description>
            Agents keep durable facts here: <code>agents/&lt;definition&gt;/</code> is shared by every instance of a
            definition, <code>projects/&lt;home&gt;/</code> by agents sharing a home, and <code>user/</code> by everyone.
            Every change is recorded with its author.
          </template>
        </UEmpty>
        <template v-else>
          <div class="flex flex-wrap items-center justify-between gap-2">
            <UInput
              v-if="newPath !== null"
              v-model="newPath"
              placeholder="user/notes.md"
              icon="i-lucide-file-plus"
              class="w-full sm:w-80"
              aria-label="Path of the new file"
            />
            <h2 v-else class="flex items-center gap-2 font-mono text-sm font-semibold text-highlighted">
              <UIcon name="i-lucide-file-text" class="size-4 text-muted" />{{ file?.path }}
            </h2>
            <div class="flex gap-2">
              <template v-if="draft === null">
                <UButton size="sm" color="neutral" variant="outline" icon="i-lucide-history" label="History" @click="showHistory" />
                <UButton size="sm" icon="i-lucide-pencil" label="Edit" @click="draft = file?.content ?? ''" />
              </template>
              <template v-else>
                <UButton size="sm" color="neutral" variant="ghost" label="Cancel" @click="cancel" />
                <UButton
                  size="sm"
                  icon="i-lucide-save"
                  label="Save"
                  :loading="saving"
                  :disabled="!!conflict"
                  @click="save(file?.sha256)"
                />
              </template>
            </div>
          </div>

          <UAlert
            v-if="conflict"
            color="warning"
            variant="subtle"
            icon="i-lucide-git-merge"
            title="Changed meanwhile"
            :actions="[
              { label: 'Discard mine', color: 'neutral', variant: 'outline', onClick: takeTheirs },
              { label: 'Save mine over it', color: 'warning', loading: saving, onClick: () => void overwrite() },
            ]"
          >
            <template #description>
              Someone saved a newer version while you edited. Theirs:
              <pre class="mt-2 max-h-60 overflow-auto rounded-md bg-default/60 p-2 font-mono text-xs whitespace-pre-wrap">{{ conflict.content }}</pre>
            </template>
          </UAlert>

          <UTextarea
            v-if="draft !== null"
            v-model="draft"
            autoresize
            :rows="14"
            autofocus
            class="w-full"
            :ui="{ base: 'font-mono text-sm' }"
            aria-label="File content"
          />
          <UCard v-else-if="file" variant="subtle">
            <MarkdownText :source="file.content" />
          </UCard>

          <section v-if="history && draft === null" class="space-y-2">
            <h3 class="text-sm font-semibold text-highlighted">History</h3>
            <UTimeline
              :items="history.map((c) => ({ value: c.commit, title: c.message, description: `${c.author} · ${c.commit.slice(0, 8)}`, date: c.at, icon: 'i-lucide-git-commit-horizontal' }))"
              size="xs"
            >
              <template #date="{ item }"><TimeAgo :at="item.date" /></template>
            </UTimeline>
          </section>
        </template>
      </div>
    </div>
  </ViewPanel>
</template>
