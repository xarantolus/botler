<script setup lang="ts">
// A tool result read as a table (lib/toolResult.ts): the first rows, "show all" for the rest; the
// result's other fields as badges.
import type { TableColumn } from "@nuxt/ui";
import { computed, ref } from "vue";
import type { Meta } from "../../lib/toolResult";

const props = defineProps<{ columns: string[]; rows: Record<string, string>[]; meta: Meta }>();
const CAP = 8;
const all = ref(false);
// Ids, not accessor keys: a key with a dot would read as a nested path.
const cols = computed((): TableColumn<Record<string, string>>[] =>
  props.columns.map((c) => ({ id: c, header: c, accessorFn: (r) => r[c] })),
);
const shown = computed(() => (all.value ? props.rows : props.rows.slice(0, CAP)));
</script>

<template>
  <div class="space-y-1.5">
    <div v-if="meta.length" class="flex flex-wrap gap-1">
      <UBadge v-for="[k, v] in meta" :key="k" color="neutral" variant="subtle" size="sm" class="font-mono">{{ k }}: {{ v }}</UBadge>
    </div>
    <UTable
      :data="shown"
      :columns="cols"
      class="max-h-96 rounded-md border border-muted"
      :ui="{ th: 'px-2.5 py-1.5 text-xs font-mono', td: 'px-2.5 py-1.5 text-xs whitespace-normal break-words' }"
    />
    <UButton
      v-if="rows.length > CAP"
      size="xs"
      color="neutral"
      variant="link"
      :label="all ? 'Show fewer' : `Show all ${rows.length} rows`"
      @click="all = !all"
    />
  </div>
</template>
