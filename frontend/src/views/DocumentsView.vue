<script setup lang="ts">
// Documents agents and jobs write for the user (Markdown, by name): the list, and one to read,
// with what its last rewrite changed one click away.
import { computed, onMounted, ref, shallowRef, watch } from "vue";
import { useRouter } from "vue-router";
import { call } from "../api/client";
import CodeBlock from "../components/common/CodeBlock.vue";
import MarkdownText from "../components/common/MarkdownText.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { confirm } from "../confirm";
import type { Document, DocumentSummary } from "../generated";
import { notifyError } from "../notify";

const props = defineProps<{ name?: string }>();
const router = useRouter();

const list = shallowRef<DocumentSummary[] | null>(null);
const doc = shallowRef<Document | null>(null);
const loading = ref(false);
const previous = ref(false);

async function load() {
  loading.value = true;
  try {
    list.value = await call("list_documents", { body: {} });
    doc.value = props.name ? await call("read_document", { body: { name: props.name } }) : null;
  } catch (e) {
    notifyError("Could not load the documents", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch(
  () => props.name,
  () => {
    previous.value = false;
    void load();
  },
);

/** "agent:dev-3f2a" → "dev-3f2a"; the user and jobs as they are. */
const by = (author: string) => author.replace(/^agent:/, "");
const items = computed(() =>
  (list.value ?? []).map((d) => ({
    label: d.title,
    to: `/documents/${encodeURIComponent(d.name)}`,
    active: d.name === props.name,
    icon: "i-lucide-file-text",
  })),
);

async function remove() {
  const d = doc.value;
  if (!d) return;
  const ok = await confirm({ title: `Delete "${d.title}"?`, description: "It is gone for good.", confirmLabel: "Delete", color: "error" });
  if (!ok) return;
  try {
    await call("delete_document", { body: { name: d.name } });
    await router.push("/documents");
  } catch (e) {
    notifyError("Could not delete", e);
  }
}
</script>

<template>
  <ViewPanel id="documents" title="Documents" icon="i-lucide-file-text" body-class="p-0 sm:p-0 lg:flex-row gap-0 sm:gap-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>

    <aside
      class="max-h-[40%] shrink-0 overflow-y-auto border-b border-default p-2 lg:max-h-none lg:w-72 lg:border-e lg:border-b-0"
      aria-label="Documents"
    >
      <div v-if="!list" class="space-y-2 p-2">
        <USkeleton v-for="n in 4" :key="n" class="h-5 w-full" />
      </div>
      <UEmpty v-else-if="!list.length" icon="i-lucide-file-text" title="No documents yet" size="sm" variant="naked" />
      <UNavigationMenu v-else :items="items" orientation="vertical" />
    </aside>

    <div class="min-w-0 flex-1 overflow-y-auto p-4 sm:p-6">
      <div class="mx-auto max-w-3xl space-y-4">
        <UEmpty
          v-if="!doc"
          icon="i-lucide-file-text"
          title="Pick a document"
          description="Agents and jobs write documents for you here: reports, shortlists they keep adding to, plans. Ask a chat for one."
          variant="naked"
          class="py-10"
        />
        <template v-else>
          <div class="flex flex-wrap items-start justify-between gap-2">
            <div>
              <h1 class="text-xl font-semibold text-highlighted">{{ doc.title }}</h1>
              <p class="text-sm text-muted">
                by
                <ULink v-if="doc.author.startsWith('agent:')" :to="`/chat/${encodeURIComponent(by(doc.author))}`" class="text-primary">{{ by(doc.author) }}</ULink>
                <span v-else>{{ by(doc.author) }}</span>
                · updated <TimeAgo :at="doc.updated_at" />
              </p>
            </div>
            <div class="flex gap-1">
              <UButton
                v-if="doc.previous_body !== null"
                size="sm"
                color="neutral"
                variant="ghost"
                icon="i-lucide-history"
                :label="previous ? 'Current version' : 'Previous version'"
                @click="previous = !previous"
              />
              <UButton size="sm" color="error" variant="ghost" icon="i-lucide-trash-2" aria-label="Delete" @click="remove" />
            </div>
          </div>
          <CodeBlock v-if="previous" label="Before the last change" :text="doc.previous_body ?? ''" language="markdown" />
          <MarkdownText v-else :source="doc.body" />
        </template>
      </div>
    </div>
  </ViewPanel>
</template>
