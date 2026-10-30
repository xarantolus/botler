<script setup lang="ts">
// A `breaker.tripped` event: what was paused, the last messages of a runaway pair, and Resume
// while the breaker is still tripped.
import { NAlert, NText } from "naive-ui";
import { computed, onMounted, shallowRef } from "vue";
import { call } from "../api/client";
import type { BotlerEvent, BreakerTrippedPayload } from "../generated";
import { isTripped } from "../lib/breakers";
import { asBuiltin } from "../lib/events";
import { ago } from "../lib/time";
import { notifyError } from "../notify";
import { state } from "../store";
import BreakerResume from "./BreakerResume.vue";

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
  <NAlert v-if="payload" :type="tripped ? 'warning' : 'default'" :title="tripped ? 'Paused by a breaker' : 'A breaker paused this (resumed since)'">
    <div>{{ payload.reason }}</div>
    <div v-if="payload.pair" style="margin-top: 4px">
      Messages between <strong>{{ payload.pair[0] }}</strong> and <strong>{{ payload.pair[1] }}</strong>
      {{ tripped ? "are held until you resume." : "flow again." }}
    </div>
    <div v-if="payload.recent?.length" style="margin-top: 6px">
      <NText depth="3" style="font-size: 12px">Their last messages</NText>
      <div v-for="(m, i) in payload.recent" :key="i" style="font-size: 13px; margin: 2px 0">
        <strong>{{ m.from }}</strong> → {{ m.to }} <NText depth="3">({{ ago(m.at) }})</NText>: {{ m.text }}
      </div>
    </div>
    <div v-if="tripped" style="margin-top: 8px"><BreakerResume :subject="payload.subject" /></div>
  </NAlert>
</template>
