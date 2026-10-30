<script setup lang="ts">
// A change summary (lib/summary.ts): what will start, stop and change as distinct blocks, likely
// mistakes and escalations as alerts, definitions, the reviewer's notes and the gate's verdict.
import { computed } from "vue";
import { looksLikeSummary, parseSummary, type SectionKind } from "../../lib/summary";
import MarkdownText from "./MarkdownText.vue";

const props = defineProps<{ markdown: string }>();
const parsed = computed(() => parseSummary(props.markdown));
const structured = computed(() => looksLikeSummary(props.markdown));

type Tone = "success" | "error" | "warning";
const BLOCKS: Partial<Record<SectionKind, { title: string; icon: string; tone: Tone }>> = {
  start: { title: "Will start happening", icon: "i-lucide-circle-plus", tone: "success" },
  stop: { title: "Will stop happening", icon: "i-lucide-circle-minus", tone: "error" },
  change: { title: "Will change", icon: "i-lucide-arrow-right-left", tone: "warning" },
};
// Full class names, so Tailwind sees them.
const TONE: Record<Tone, { bar: string; icon: string }> = {
  success: { bar: "border-s-success", icon: "text-success" },
  error: { bar: "border-s-error", icon: "text-error" },
  warning: { bar: "border-s-warning", icon: "text-warning" },
};
const DEF_COLOR = { added: "success", removed: "error", changed: "warning" } as const;
</script>

<template>
  <MarkdownText v-if="!structured" :source="markdown" />
  <div v-else class="flex flex-col gap-3 text-sm">
    <MarkdownText v-if="parsed.preamble" :source="parsed.preamble" />
    <template v-for="(s, i) in parsed.sections" :key="i">
      <UAlert
        v-if="s.kind === 'check'"
        color="warning"
        variant="subtle"
        icon="i-lucide-triangle-alert"
        title="Check this: likely mistakes"
      >
        <template #description>
          <ul class="mt-1 list-disc ps-4 space-y-0.5">
            <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
          </ul>
          <MarkdownText v-if="s.text" :source="s.text" class="mt-1" />
        </template>
      </UAlert>

      <UAlert
        v-else-if="s.kind === 'escalations'"
        color="error"
        variant="subtle"
        icon="i-lucide-shield-alert"
        title="Needs your approval because it"
      >
        <template #description>
          <ul class="mt-1 list-disc ps-4 space-y-0.5">
            <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
          </ul>
        </template>
      </UAlert>

      <UAlert v-else-if="s.kind === 'approval'" color="success" variant="subtle" icon="i-lucide-shield-check">
        <template #description><MarkdownText inline :source="s.inline" /></template>
      </UAlert>

      <section
        v-else-if="BLOCKS[s.kind]"
        class="rounded-md border border-default border-s-4 bg-elevated/40"
        :class="TONE[BLOCKS[s.kind]!.tone].bar"
      >
        <header class="flex items-center gap-2 px-3 pt-2.5 pb-1.5 font-medium text-highlighted">
          <UIcon :name="BLOCKS[s.kind]!.icon" class="size-4 shrink-0" :class="TONE[BLOCKS[s.kind]!.tone].icon" />
          {{ BLOCKS[s.kind]!.title }}
          <UBadge :label="String(s.items.length)" color="neutral" variant="soft" size="sm" class="rounded-full" />
        </header>
        <ul class="divide-y divide-default">
          <li v-for="(it, j) in s.items" :key="j" class="px-3 py-2">
            <template v-if="it.change">
              <code class="font-mono text-xs text-muted">{{ it.change.id }}</code>
              <div class="mt-1 grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5">
                <span class="text-xs text-dimmed uppercase tracking-wide pt-0.5">was</span>
                <MarkdownText inline :source="it.change.before" class="text-muted line-through decoration-error/50" />
                <span class="text-xs text-dimmed uppercase tracking-wide pt-0.5">now</span>
                <MarkdownText inline :source="it.change.after" />
              </div>
            </template>
            <MarkdownText v-else inline :source="it.text" />
            <div v-for="(sub, k) in it.sub" :key="k" class="mt-0.5 ps-3 text-xs text-muted">
              <MarkdownText inline :source="sub" />
            </div>
          </li>
        </ul>
      </section>

      <div v-else-if="s.kind === 'definitions'" class="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span class="font-medium text-highlighted">Definitions</span>
        <span v-for="(it, j) in s.items" :key="j" class="inline-flex items-center gap-1.5">
          <UBadge
            :color="it.status ? DEF_COLOR[it.status] : 'neutral'"
            variant="subtle"
            size="sm"
            :label="it.status ?? 'other'"
          />
          <MarkdownText inline :source="it.text" />
        </span>
      </div>

      <div v-else-if="s.kind === 'reviewer'" class="flex gap-2.5 rounded-md bg-elevated/50 px-3 py-2.5">
        <UIcon name="i-lucide-user-check" class="mt-0.5 size-4 shrink-0 text-muted" />
        <div class="min-w-0">
          <div class="font-medium text-highlighted">Reviewer</div>
          <MarkdownText :source="[s.inline, s.text].filter(Boolean).join('\n')" class="text-muted" />
        </div>
      </div>

      <div v-else-if="s.kind === 'gate'" class="flex items-center gap-2">
        <span class="font-medium text-highlighted">Gate</span>
        <UBadge
          :color="/pass/i.test(s.inline) ? 'success' : 'error'"
          :icon="/pass/i.test(s.inline) ? 'i-lucide-check' : 'i-lucide-x'"
          variant="subtle"
          size="sm"
          :label="s.inline"
        />
      </div>

      <div v-else>
        <div class="font-medium text-highlighted">{{ s.title }}</div>
        <MarkdownText v-if="s.inline" :source="s.inline" />
        <ul v-if="s.items.length" class="list-disc ps-5">
          <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
        </ul>
        <MarkdownText v-if="s.text" :source="s.text" />
      </div>
    </template>
  </div>
</template>
