<script setup lang="ts">
// A delivered event (not the user typing): who sent it, how far it is trusted, what kind.
import { NCard, NSpace, NTag, NText } from "naive-ui";
import { computed } from "vue";
import { prettyJsonBody, type Envelope } from "../lib/envelope";
import { ago } from "../lib/time";
import BreakerNotice from "./BreakerNotice.vue";
import MarkdownText from "./MarkdownText.vue";

const props = defineProps<{ env: Envelope; at: string | null }>();
const json = computed(() => prettyJsonBody(props.env.body));
const provType = computed(() =>
  props.env.provenance === "trusted" ? "success" : props.env.provenance === "external" ? "warning" : "error",
);
</script>

<template>
  <NCard size="small" embedded :bordered="true" class="envelope">
    <NSpace size="small" align="center" style="margin-bottom: 6px">
      <NText depth="3" style="font-size: 12px; text-transform: uppercase; letter-spacing: 0.04em">incoming event</NText>
      <NTag size="small" :bordered="false" type="info">{{ env.kind ?? "message" }}</NTag>
      <NText style="font-size: 13px">from <strong>{{ env.from }}</strong></NText>
      <NTag size="small" :bordered="false" :type="provType">{{ env.provenance }}</NTag>
      <NTag v-if="env.malformed" size="small" type="error">malformed</NTag>
      <NText depth="3" style="font-size: 12px" :title="at ?? ''">{{ env.localTime ?? ago(at) }}</NText>
      <NTag v-if="env.gap" size="small" :bordered="false">{{ env.gap }}</NTag>
      <RouterLink v-if="env.event" :to="{ name: 'events', query: { event: env.event } }" style="font-size: 12px">
        trace
      </RouterLink>
    </NSpace>
    <BreakerNotice v-if="env.kind === 'breaker.tripped' && env.event" :event-id="env.event" style="margin-bottom: 6px" />
    <pre v-if="json" class="mono">{{ json }}</pre>
    <MarkdownText v-else-if="env.body.trim()" :source="env.body.trim()" />
  </NCard>
</template>

<style scoped>
.envelope {
  border-left: 3px solid #2f5bd3;
}
</style>
