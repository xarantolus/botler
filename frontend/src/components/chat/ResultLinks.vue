<script setup lang="ts">
// A tool result read as links (lib/toolResult.ts): one card per link, the title linking out (new
// tab, no referrer), its host and snippet; the result's other fields as badges.
import type { LinkItem, Meta } from "../../lib/toolResult";

defineProps<{ items: LinkItem[]; meta: Meta }>();
</script>

<template>
  <div class="space-y-1.5">
    <div v-if="meta.length" class="flex flex-wrap gap-1">
      <UBadge v-for="[k, v] in meta" :key="k" color="neutral" variant="subtle" size="sm" class="font-mono">{{ k }}: {{ v }}</UBadge>
    </div>
    <ul class="divide-y divide-default overflow-hidden rounded-md border border-muted">
      <li v-for="(it, i) in items" :key="i" class="min-w-0 px-3 py-2" data-link>
        <ULink :to="it.url" target="_blank" rel="noopener noreferrer" class="line-clamp-1 text-sm font-medium text-primary hover:underline">
          {{ it.title }}
        </ULink>
        <p class="truncate text-xs text-dimmed">{{ it.host }}</p>
        <p v-if="it.snippet" class="mt-0.5 line-clamp-2 text-xs text-muted">{{ it.snippet }}</p>
      </li>
    </ul>
  </div>
</template>
