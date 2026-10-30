<script setup lang="ts">
// One search over conversations, memory, events and the monorepo; hits open where they are.
import { computed, nextTick, ref, shallowRef, useTemplateRef, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { SearchHit, SearchSource } from "../generated";
import { groupHits, highlight, hitLocation, SOURCE_LABELS } from "../lib/search";
import { errorText } from "../notify";

const route = useRoute();
const router = useRouter();

const q = computed(() => (typeof route.query.q === "string" ? route.query.q : ""));
const raw = computed(() => route.query.raw === "1");
const draft = ref(q.value);
watch(q, (v) => (draft.value = v));

const hits = shallowRef<SearchHit[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);

watch(
  [q, raw],
  async ([query, isRaw]) => {
    hits.value = [];
    error.value = null;
    if (!query.trim()) return;
    loading.value = true;
    try {
      hits.value = (await call("search", { query: { q: query, limit: 30, raw: isRaw || undefined } })).results;
    } catch (e) {
      error.value = errorText(e);
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

const groups = computed(() => groupHits(hits.value));

function submit() {
  const v = draft.value.trim();
  void router.replace({ query: { ...route.query, q: v || undefined } });
}
function setRaw(v: boolean | "indeterminate") {
  void router.replace({ query: { ...route.query, raw: v === true ? "1" : undefined } });
}

const ICONS: Record<SearchSource, string> = {
  session: "i-lucide-messages-square",
  memory: "i-lucide-brain",
  event: "i-lucide-activity",
  repo: "i-lucide-folder-git-2",
};

const input = useTemplateRef<{ inputRef: HTMLInputElement | null }>("input");
void nextTick(() => input.value?.inputRef?.focus());
</script>

<template>
  <ViewPanel id="search" title="Search" icon="i-lucide-search">
    <div class="mx-auto w-full max-w-3xl space-y-5">
      <form class="space-y-2" role="search" @submit.prevent="submit">
        <UInput
          ref="input"
          v-model="draft"
          icon="i-lucide-search"
          size="xl"
          placeholder="Search conversations, memory, events and the monorepo"
          aria-label="Search"
          class="w-full"
          :loading="loading"
        >
          <template #trailing>
            <UButton type="submit" size="sm" label="Search" :disabled="!draft.trim()" />
          </template>
        </UInput>
        <UCheckbox
          :model-value="raw"
          label='Query syntax (OR, NOT, "phrases", prefix*)'
          size="sm"
          @update:model-value="setRaw"
        />
      </form>

      <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-circle-alert" title="Search failed" :description="error" />
      <div v-else-if="loading" class="space-y-3">
        <USkeleton v-for="n in 4" :key="n" class="h-16 w-full rounded-lg" />
      </div>
      <UEmpty
        v-else-if="!q"
        icon="i-lucide-search"
        title="Search everything"
        description="Conversations, memory files, events and the monorepo, in one index."
        variant="naked"
        class="py-10"
      />
      <UEmpty
        v-else-if="!hits.length"
        icon="i-lucide-search-x"
        title="Nothing found"
        :description="`No results for “${q}”.`"
        variant="naked"
        class="py-10"
      />

      <section v-for="g in groups" :key="g.source" class="space-y-2">
        <h2 class="flex items-center gap-2 text-sm font-semibold text-highlighted">
          <UIcon :name="ICONS[g.source]" class="size-4 text-muted" />
          {{ SOURCE_LABELS[g.source] }}
          <UBadge :label="String(g.hits.length)" color="neutral" variant="soft" size="sm" class="rounded-full" />
        </h2>
        <ul class="divide-y divide-default overflow-hidden rounded-lg border border-default">
          <li v-for="h in g.hits" :key="`${h.owner}/${h.ref}`">
            <component
              :is="hitLocation(h) ? RouterLink : 'div'"
              :to="hitLocation(h) ?? undefined"
              class="block px-4 py-3"
              :class="hitLocation(h) && 'hover:bg-elevated/60 focus-visible:bg-elevated/60 focus-visible:outline-none'"
            >
              <div class="flex flex-wrap items-center gap-x-1.5 text-sm">
                <span class="font-medium text-highlighted">{{ h.title }}</span>
                <span v-if="h.owner" class="text-muted">· {{ h.owner }}</span>
                <span v-if="h.at" class="text-xs text-dimmed">· <TimeAgo :at="h.at" /></span>
                <UIcon v-if="hitLocation(h)" name="i-lucide-arrow-up-right" class="ms-auto size-4 text-dimmed" />
              </div>
              <p class="mt-1 text-sm text-muted [overflow-wrap:anywhere]">
                <template v-for="(p, i) in highlight(h.snippet)" :key="i"
                  ><mark v-if="p.mark" class="rounded-sm bg-warning/25 px-0.5 text-highlighted">{{ p.text }}</mark
                  ><span v-else>{{ p.text }}</span></template
                >
              </p>
            </component>
          </li>
        </ul>
      </section>
    </div>
  </ViewPanel>
</template>
