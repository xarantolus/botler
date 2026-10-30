<script setup lang="ts">
import { NAlert, NButton, NEmpty, NInput, NScrollbar, NSpace, NText } from "naive-ui";
import { computed, onMounted, ref, shallowRef } from "vue";
import { ApiError, call } from "../api/client";
import MarkdownText from "../components/MarkdownText.vue";
import type { MemoryCommit, MemoryEntry, MemoryFile } from "../generated";
import { byNamespace } from "../lib/memory";
import { ago } from "../lib/time";
import { message, notifyError } from "../notify";

const files = shallowRef<MemoryEntry[]>([]);
const loading = ref(false);
const groups = computed(() => byNamespace(files.value));

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

/** Save the draft over `base` (the version it was edited from; none for a new file). */
async function save(base: string | undefined) {
  const path = newPath.value ?? file.value?.path;
  if (draft.value === null || !path) return;
  saving.value = true;
  try {
    const written = await call("memory_write", { body: { path, content: draft.value, expected_sha256: base } });
    message.success(`${path} saved`);
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
        message.warning(`${path} was deleted meanwhile; saving creates it again.`);
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

/** Save mine over the conflicting version, or create it again if it was deleted. */
const overwrite = () => save(conflict.value?.sha256);
function takeTheirs() {
  if (!conflict.value) return;
  file.value = conflict.value;
  draft.value = null;
  conflict.value = null;
}
</script>

<template>
  <div class="memory">
    <aside class="list">
      <NSpace size="small" style="padding: 8px">
        <NButton size="small" :loading="loading" @click="load">Refresh</NButton>
        <NButton size="small" @click="startNew">New file</NButton>
      </NSpace>
      <NScrollbar style="flex: 1; min-height: 0">
        <NEmpty v-if="!loading && !files.length" description="No memory yet." style="margin-top: 24px" />
        <div v-for="g in groups" :key="g.namespace" class="group">
          <NText depth="3" class="ns">{{ g.namespace }}</NText>
          <div
            v-for="f in g.files"
            :key="f.path"
            class="entry"
            :class="{ active: file?.path === f.path }"
            @click="open(f.path)"
          >
            {{ f.path.slice(g.namespace.length + 1) }}
          </div>
        </div>
      </NScrollbar>
    </aside>
    <NScrollbar style="flex: 1; min-width: 0">
      <div style="max-width: 860px; padding: 12px 16px">
        <NAlert v-if="!file && draft === null" type="info" :show-icon="false">
          Agents keep durable facts here: <code>agents/&lt;definition&gt;/</code> is shared by every instance of a
          definition, <code>projects/&lt;home&gt;/</code> by agents sharing a home, and <code>user/</code> by everyone.
          Every change is recorded with its author.
        </NAlert>
        <NSpace v-else vertical>
          <NSpace align="center" justify="space-between">
            <NInput v-if="newPath !== null" v-model:value="newPath" size="small" placeholder="user/notes.md" style="width: 320px" />
            <strong v-else>{{ file?.path }}</strong>
            <NSpace size="small">
              <template v-if="draft === null">
                <NButton size="small" @click="draft = file?.content ?? ''">Edit</NButton>
                <NButton size="small" @click="showHistory">History</NButton>
              </template>
              <template v-else>
                <NButton size="small" type="primary" :loading="saving" :disabled="!!conflict" @click="save(file?.sha256)">
                  Save
                </NButton>
                <NButton size="small" @click="draft = null; conflict = null; newPath = null">Cancel</NButton>
              </template>
            </NSpace>
          </NSpace>
          <NAlert v-if="conflict" type="warning" title="Changed meanwhile">
            Someone saved a newer version while you edited. Their version:
            <pre class="theirs">{{ conflict.content }}</pre>
            <NSpace size="small">
              <NButton size="small" @click="takeTheirs">Discard mine</NButton>
              <NButton size="small" type="warning" :loading="saving" @click="overwrite">Save mine over it</NButton>
            </NSpace>
          </NAlert>
          <NInput
            v-if="draft !== null"
            v-model:value="draft"
            type="textarea"
            :autosize="{ minRows: 12 }"
            style="font-family: monospace"
          />
          <MarkdownText v-else-if="file" :source="file.content" />
          <div v-if="history && draft === null">
            <NText depth="3">History</NText>
            <div v-for="c in history" :key="c.commit" style="font-size: 13px">
              <code>{{ c.commit.slice(0, 8) }}</code> {{ c.message }} · {{ c.author }} ·
              <span :title="c.at">{{ ago(c.at) }}</span>
            </div>
          </div>
        </NSpace>
      </div>
    </NScrollbar>
  </div>
</template>

<style scoped>
.memory {
  display: flex;
  height: 100%;
  min-height: 0;
}
.list {
  width: 260px;
  flex: none;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--n-border-color, rgba(128, 128, 128, 0.2));
}
.group {
  padding: 4px 8px 8px;
}
.ns {
  font-size: 12px;
}
.entry {
  padding: 2px 8px;
  cursor: pointer;
  border-radius: 4px;
  font-size: 14px;
  overflow-wrap: anywhere;
}
.entry:hover,
.entry.active {
  background: rgba(128, 128, 128, 0.15);
}
.theirs {
  white-space: pre-wrap;
  max-height: 240px;
  overflow: auto;
  font-size: 12px;
}
@media (max-width: 860px) {
  .memory {
    flex-direction: column;
  }
  .list {
    width: auto;
    max-height: 40%;
    border-right: none;
  }
}
</style>
