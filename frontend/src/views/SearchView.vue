<script setup lang="ts">
import { NAlert, NCheckbox, NEmpty, NSpin, NTag, NText } from "naive-ui";
import { computed, ref, shallowRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { call } from "../api/client";
import type { SearchHit } from "../generated";
import { groupHits, highlight, hitLocation, SOURCE_LABELS } from "../lib/search";
import { ago } from "../lib/time";

const route = useRoute();
const router = useRouter();

const q = computed(() => (typeof route.query.q === "string" ? route.query.q : ""));
const raw = computed(() => route.query.raw === "1");
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
      error.value = e instanceof Error ? e.message : String(e);
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

const groups = computed(() => groupHits(hits.value));

function setRaw(v: boolean) {
  void router.replace({ query: { ...route.query, raw: v ? "1" : undefined } });
}

function open(hit: SearchHit) {
  const to = hitLocation(hit);
  if (to) void router.push(to);
}
</script>

<template>
  <div style="height: 100%; overflow-y: auto; padding: 10px 16px">
    <div style="display: flex; gap: 12px; align-items: center; flex-wrap: wrap; margin-bottom: 10px">
      <NText depth="2" style="flex: 1">
        <template v-if="q">Results for <strong>{{ q }}</strong></template>
        <template v-else>Type in the search box above.</template>
      </NText>
      <NCheckbox :checked="raw" size="small" @update:checked="setRaw">Query syntax (OR, NOT, "phrases", prefix*)</NCheckbox>
    </div>
    <NAlert v-if="error" type="error" :show-icon="false">{{ error }}</NAlert>
    <div v-if="loading" style="text-align: center; padding: 30px"><NSpin /></div>
    <NEmpty v-else-if="q && !error && !hits.length" description="Nothing found." style="margin-top: 40px" />
    <section v-for="g in groups" :key="g.source" style="margin-bottom: 18px">
      <h3 style="margin: 6px 0">
        {{ SOURCE_LABELS[g.source] }} <NTag size="small" round :bordered="false">{{ g.hits.length }}</NTag>
      </h3>
      <div
        v-for="h in g.hits"
        :key="`${h.owner}/${h.ref}`"
        class="hit"
        :class="{ link: hitLocation(h) !== null }"
        @click="open(h)"
      >
        <div class="head">
          <strong>{{ h.title }}</strong>
          <NText v-if="h.owner" depth="3">· {{ h.owner }}</NText>
          <NText v-if="h.at" depth="3" :title="h.at">· {{ ago(h.at) }}</NText>
        </div>
        <div class="snippet">
          <template v-for="(p, i) in highlight(h.snippet)" :key="i"
            ><mark v-if="p.mark">{{ p.text }}</mark><span v-else>{{ p.text }}</span></template
          >
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.hit {
  padding: 8px 10px;
  border-radius: 6px;
  margin-bottom: 4px;
}
.hit.link {
  cursor: pointer;
}
.hit.link:hover {
  background: rgba(127, 127, 127, 0.12);
}
.head {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  font-size: 13px;
}
.snippet {
  font-size: 13px;
  opacity: 0.85;
  overflow-wrap: anywhere;
}
mark {
  background: rgba(240, 190, 40, 0.45);
  color: inherit;
  border-radius: 2px;
}
</style>
