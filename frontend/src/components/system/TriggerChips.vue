<script setup lang="ts">
// What wakes an agent, compactly: each schedule on its own line with when it fires next, and the
// event types it listens for as chips (to those events). The prompt each sends is in its tooltip.
import { computed } from "vue";
import type { TriggerPreview } from "../../generated";
import { fireLabel, plain } from "../../lib/system";

const props = defineProps<{ triggers: TriggerPreview[]; timezone: string; now: number }>();

const events = computed(() => props.triggers.filter((t) => t.event !== null));
const others = computed(() => props.triggers.filter((t) => t.event === null));
/** The tooltip: the condition when it has more than the type, and what it sends. */
const tip = (t: TriggerPreview) => [t.when.includes(" where ") ? plain(t.when) : null, t.prompt ?? plain(t.then)].filter(Boolean).join("\n\n");
</script>

<template>
  <div class="space-y-1 text-xs text-muted">
    <UTooltip v-for="t in others" :key="t.id" :text="tip(t)" :content="{ side: 'top', align: 'start' }" :ui="{ content: 'max-w-md h-auto whitespace-pre-line' }">
      <div class="flex items-center gap-1.5">
        <UIcon :name="t.upcoming.length ? 'i-lucide-clock' : 'i-lucide-webhook'" class="size-3.5 shrink-0" />
        <span class="truncate">{{ plain(t.when) }}</span>
        <span v-if="t.upcoming[0]" class="shrink-0 tabular-nums">· next {{ fireLabel(t.upcoming[0], timezone, now) }}</span>
        <UBadge v-if="t.shadow" color="neutral" variant="outline" size="sm" label="shadow" />
      </div>
    </UTooltip>
    <div v-if="events.length" class="flex flex-wrap items-center gap-1">
      <UIcon name="i-lucide-bell-ring" class="size-3.5 shrink-0" />
      <span>On</span>
      <UTooltip v-for="t in events" :key="t.id" :text="tip(t)" :ui="{ content: 'max-w-md h-auto whitespace-pre-line' }">
        <ULink
          :to="t.event!.includes('*') ? { name: 'events' } : { name: 'events', query: { type: t.event } }"
          class="rounded border border-default px-1 font-mono hover:bg-elevated"
          :class="{ 'opacity-60': t.shadow }"
        >
          {{ t.event }}<template v-if="t.when.includes(' where ')"> …</template>
        </ULink>
      </UTooltip>
    </div>
  </div>
</template>
