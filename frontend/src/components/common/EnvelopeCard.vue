<script setup lang="ts">
// A delivered event (not the user typing): what kind, who sent it, how far it is trusted, when.
import { computed } from "vue";
import { payloadOf, prettyJsonBody, type Envelope } from "../../lib/envelope";
import { eventIcon } from "../../lib/events";
import { provenanceColor } from "../../lib/runs";
import BreakerNotice from "./BreakerNotice.vue";
import CodeBlock from "./CodeBlock.vue";
import MarkdownText from "./MarkdownText.vue";
import TimeAgo from "./TimeAgo.vue";

const props = defineProps<{ env: Envelope; at: string | null }>();
const json = computed(() => prettyJsonBody(props.env.body));
const body = computed(() => props.env.body.trim());
/** An action's outcome, shown as itself (not the agent-facing text). */
const action = computed(() => {
  if (props.env.kind !== "action.result") return null;
  const p = payloadOf(props.env.body);
  if (!p || typeof p.action !== "string") return null;
  const r = p.result;
  const declined = r && typeof r === "object" && "declined" in r ? String((r as { declined: unknown }).declined) : null;
  return { name: p.action, ok: p.ok === true, declined, result: declined === null ? JSON.stringify(r, null, 2) : null };
});
</script>

<template>
  <UCard
    variant="subtle"
    class="border-s-4 border-s-primary/60"
    :ui="{ root: 'rounded-md', body: 'p-3 sm:p-3.5 space-y-2.5' }"
    :aria-label="`Incoming ${env.kind ?? 'message'} from ${env.from}`"
  >
    <div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
      <UIcon :name="eventIcon(env.kind)" class="size-4 shrink-0 text-primary" />
      <span class="font-medium text-highlighted">{{ env.kind ?? "message" }}</span>
      <span class="text-muted">from <strong class="font-medium text-default">{{ env.from }}</strong></span>
      <UBadge :color="provenanceColor(env.provenance)" variant="subtle" size="sm" :label="env.provenance" />
      <UBadge v-if="env.malformed" color="error" variant="solid" size="sm" label="malformed" />
      <span class="text-dimmed">
        <template v-if="env.localTime"><span :title="at ?? undefined">{{ env.localTime }}</span></template>
        <TimeAgo v-else :at="at" />
      </span>
      <UBadge v-if="env.gap" color="neutral" variant="soft" size="sm" icon="i-lucide-hourglass" :label="env.gap" />
      <UButton
        v-if="env.event"
        :to="{ name: 'events', query: { event: env.event } }"
        size="xs"
        color="neutral"
        variant="link"
        icon="i-lucide-route"
        label="Trace"
        class="ms-auto px-0"
      />
    </div>
    <BreakerNotice v-if="env.kind === 'breaker.tripped' && env.event" :event-id="env.event" />
    <template v-if="action">
      <div class="flex flex-wrap items-center gap-2 text-sm">
        <span>Action <code class="font-mono text-highlighted">{{ action.name }}</code></span>
        <UBadge
          :color="action.declined !== null ? 'neutral' : action.ok ? 'success' : 'error'"
          variant="subtle"
          size="sm"
          :label="action.declined !== null ? 'declined' : action.ok ? 'succeeded' : 'failed'"
        />
      </div>
      <p v-if="action.declined !== null" class="text-sm text-muted">Your reason: {{ action.declined }}</p>
      <CodeBlock v-else-if="action.result" :label="action.ok ? 'Result' : 'Error'" :text="action.result" language="json" max-height="16rem" />
    </template>
    <CodeBlock v-else-if="json" :text="json" max-height="16rem" />
    <MarkdownText v-else-if="body" :source="body" class="text-sm" />
  </UCard>
</template>
