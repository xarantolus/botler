<script setup lang="ts">
// A chat's context size, in thousands of tokens, opened by setChatContext. Emits `close` with
// the size (null: its agent's default) or false when cancelled. Lists what each size would have
// cost on the chat's last week of calls, when it made enough of them to say; a row picks it.
import { ref } from "vue";
import type { ContextAdvice } from "../../generated/ContextAdvice";

const props = defineProps<{ current: number | null; advice: ContextAdvice | null }>();
const emit = defineEmits<{ close: [result: { compactAt: number | null } | false] }>();
const k = ref<number | undefined>(props.current ? props.current / 1000 : undefined);
const save = (thousands: number | undefined) =>
  emit("close", { compactAt: thousands ? Math.round(thousands) * 1000 : null });
</script>

<template>
  <UModal
    title="Context size"
    description="The chat is compacted (older messages summarised) once its context passes this size. More keeps more detail at hand; every model call resends it."
    :ui="{ footer: 'justify-between' }"
  >
    <template #body>
      <UFormField label="Thousand tokens" hint="Empty: its agent's default">
        <UInputNumber v-model="k" :min="10" :max="2000" :step="10" placeholder="e.g. 120" class="w-40" @keydown.enter="save(k)" />
      </UFormField>
      <div v-if="props.advice" class="mt-4 text-sm">
        <p class="text-muted mb-2">
          Its {{ props.advice.calls }} model calls of the last {{ Math.max(1, Math.round(props.advice.days)) }} days, replayed at each size:
        </p>
        <button
          v-for="s in props.advice.sizes"
          :key="s.compact_at"
          type="button"
          class="flex w-full cursor-pointer justify-between rounded px-2 py-1 hover:bg-elevated"
          :class="{ 'bg-elevated': k === s.compact_at / 1000 }"
          @click="k = s.compact_at / 1000"
        >
          <span>
            {{ s.compact_at / 1000 }}k
            <UBadge v-if="s.compact_at === props.advice.best" label="best" color="success" variant="subtle" size="sm" />
            <UBadge v-if="s.compact_at === props.advice.current" label="now" color="neutral" variant="subtle" size="sm" />
          </span>
          <span class="tabular-nums">${{ s.cost_usd.toFixed(2) }}</span>
        </button>
      </div>
    </template>
    <template #footer>
      <UButton color="neutral" variant="ghost" label="Cancel" @click="emit('close', false)" />
      <UButton label="Save" @click="save(k)" />
    </template>
  </UModal>
</template>
