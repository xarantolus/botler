<script setup lang="ts">
// Any model id as free text, with suggestions: the typically used models first, then OpenRouter's
// catalog, each with its price, context and image support.
import { computed } from "vue";
import type { CatalogModel, ModelEntry } from "../../generated";
import { contextLabel, priceLabel, searchModels } from "../../lib/settings";

const props = defineProps<{
  catalog: CatalogModel[];
  typical: ModelEntry[];
  /** Shown when empty, e.g. "Default: deepseek/…" (empty means the default). */
  placeholder?: string;
}>();
const model = defineModel<string | null>({ required: true });

const byId = computed(() => new Map(props.catalog.map((m) => [m.id, m])));
const notes = computed(() => new Map(props.typical.map((t) => [t.id, t.note])));
const text = computed({
  get: () => model.value ?? "",
  set: (v: string) => (model.value = v.trim() || null),
});
const items = computed(() => searchModels(props.catalog, props.typical, text.value).map((o) => o.id));
const current = computed(() => (model.value ? byId.value.get(model.value) : undefined));
</script>

<template>
  <div class="space-y-1">
    <UInputMenu
      v-model="text"
      mode="autocomplete"
      :items="items"
      ignore-filter
      :placeholder="placeholder ?? 'vendor/model'"
      icon="i-lucide-cpu"
      class="w-full"
      :ui="{ base: 'font-mono', content: 'min-w-80' }"
    >
      <template #item-label="{ item }">
        <span class="font-mono">{{ item }}</span>
        <span v-if="notes.get(item)" class="ms-2 text-xs text-muted">{{ notes.get(item) }}</span>
      </template>
      <template #item-trailing="{ item }">
        <span v-if="byId.get(item)" class="flex items-center gap-1.5 text-xs text-muted">
          <UIcon v-if="byId.get(item)?.image" name="i-lucide-image" class="size-3.5" />
          <span>{{ contextLabel(byId.get(item)?.context_length ?? null) }}</span>
          <span class="whitespace-nowrap">{{ priceLabel(byId.get(item)!) }}</span>
        </span>
      </template>
    </UInputMenu>
    <p v-if="current" class="flex flex-wrap items-center gap-x-2 text-xs text-muted">
      <span>{{ current.name }}</span>
      <span v-if="priceLabel(current)">· {{ priceLabel(current) }}</span>
      <span v-if="contextLabel(current.context_length)">· {{ contextLabel(current.context_length) }}</span>
      <span>· {{ current.image ? "sees images" : "text only" }}</span>
    </p>
    <p v-else-if="model && catalog.length" class="text-xs text-warning">Not in OpenRouter's model list.</p>
  </div>
</template>
