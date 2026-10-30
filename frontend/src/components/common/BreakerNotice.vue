<script setup lang="ts">
// A `breaker.tripped` event: what was paused, the last messages of a runaway pair, and Resume
// while the breaker is still tripped.
import { computed, onMounted, shallowRef } from "vue";
import { call } from "../../api/client";
import type { BotlerEvent, BreakerTrippedPayload } from "../../generated";
import { isTripped } from "../../lib/breakers";
import { asBuiltin } from "../../lib/events";
import { notifyError } from "../../notify";
import { state } from "../../store";
import BreakerResume from "./BreakerResume.vue";
import TimeAgo from "./TimeAgo.vue";

const props = defineProps<{ eventId: string }>();

const fetched = shallowRef<BotlerEvent | null>(null);
const event = computed(() => state.events.find((e) => e.id === props.eventId) ?? fetched.value);
const payload = computed((): BreakerTrippedPayload | null => {
  const b = event.value ? asBuiltin(event.value) : null;
  return b?.type === "breaker.tripped" ? b.payload : null;
});
const tripped = computed(() => (payload.value ? isTripped(payload.value.subject, state.status?.breakers ?? []) : false));

onMounted(async () => {
  if (event.value) return;
  try {
    fetched.value = (await call("trace", { params: { id: props.eventId } })).event;
  } catch (e) {
    notifyError("Could not load the breaker event", e);
  }
});
</script>

<template>
  <UAlert
    v-if="payload"
    :color="tripped ? 'warning' : 'neutral'"
    variant="subtle"
    :icon="tripped ? 'i-lucide-octagon-pause' : 'i-lucide-history'"
    :title="tripped ? 'Paused by a breaker' : 'A breaker paused this (resumed since)'"
  >
    <template #description>
      <p>{{ payload.reason }}</p>
      <p v-if="payload.pair" class="mt-1">
        Messages between <strong>{{ payload.pair[0] }}</strong> and <strong>{{ payload.pair[1] }}</strong>
        {{ tripped ? "are held until you resume." : "flow again." }}
      </p>
      <div v-if="payload.recent?.length" class="mt-2 space-y-1">
        <div class="text-xs font-medium text-muted">Their last messages</div>
        <div v-for="(m, i) in payload.recent" :key="i" class="text-sm">
          <strong>{{ m.from }}</strong> → {{ m.to }}
          <span class="text-xs text-dimmed">(<TimeAgo :at="m.at" />)</span>: {{ m.text }}
        </div>
      </div>
    </template>
    <template v-if="tripped" #actions>
      <BreakerResume :subject="payload.subject" size="xs" />
    </template>
  </UAlert>
</template>
