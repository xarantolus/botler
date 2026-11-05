<script setup lang="ts">
// What OpenRouter itself counts for the model key: what it charged, and its limit. botler's own
// figures (the Costs page) are estimates at each model's listed price; these are the bill.
import { onMounted, ref, shallowRef } from "vue";
import { call } from "../../api/client";
import type { ProviderKey } from "../../generated";
import { usd } from "../../lib/time";

const key = shallowRef<ProviderKey | null | undefined>(undefined);
const error = ref<string | null>(null);
const PERIOD: Record<string, string> = { daily: "day", weekly: "week", monthly: "month" };

onMounted(async () => {
  try {
    key.value = await call("openrouter_key", {});
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
});
</script>

<template>
  <UCard v-if="key !== null">
    <template #header>
      <p class="font-semibold text-highlighted">OpenRouter</p>
      <p class="text-sm text-muted">What OpenRouter charged for the model key. The Costs page estimates at listed prices.</p>
    </template>
    <p v-if="error" class="text-sm text-error">Could not ask OpenRouter: {{ error }}</p>
    <USkeleton v-else-if="key === undefined" class="h-12 w-full" />
    <template v-else>
      <dl class="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <div v-for="[label, v] in [['Today', key.usage_daily], ['This week', key.usage_weekly], ['This month', key.usage_monthly], ['In all', key.usage]] as const" :key="label">
          <dt class="text-xs text-muted">{{ label }}</dt>
          <dd class="text-lg font-semibold tabular-nums text-highlighted">{{ usd(v) }}</dd>
        </div>
      </dl>
      <p v-if="key.limit !== null" class="mt-3 text-sm text-muted">
        Limit {{ usd(key.limit) }}<template v-if="key.limit_reset"> a {{ PERIOD[key.limit_reset] ?? key.limit_reset }}</template>,
        {{ usd(key.limit_remaining) }} left.
      </p>
      <p v-else class="mt-3 text-sm text-muted">The key has no limit of its own; a workspace budget on OpenRouter still applies.</p>
    </template>
  </UCard>
</template>
