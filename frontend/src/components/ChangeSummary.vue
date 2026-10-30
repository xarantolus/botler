<script setup lang="ts">
import { NAlert, NTag } from "naive-ui";
import { computed } from "vue";
import { looksLikeSummary, parseSummary, type SummarySection } from "../lib/summary";
import MarkdownText from "./MarkdownText.vue";

const props = defineProps<{ markdown: string }>();
const parsed = computed(() => parseSummary(props.markdown));
const structured = computed(() => looksLikeSummary(props.markdown));

const BLOCKS: Partial<Record<SummarySection["kind"], { sign: string; cls: string; title: string }>> = {
  start: { sign: "+", cls: "added", title: "Will start happening" },
  stop: { sign: "−", cls: "removed", title: "Will stop happening" },
  change: { sign: "~", cls: "changed", title: "Will change" },
};
</script>

<template>
  <MarkdownText v-if="!structured" :source="markdown" />
  <div v-else class="summary">
    <MarkdownText v-if="parsed.preamble" :source="parsed.preamble" />
    <template v-for="(s, i) in parsed.sections" :key="i">
      <NAlert v-if="s.kind === 'check'" type="warning" title="Check this (likely mistakes)">
        <ul>
          <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
        </ul>
        <MarkdownText v-if="s.text" :source="s.text" />
      </NAlert>

      <NAlert v-else-if="s.kind === 'escalations'" type="error" title="Needs your approval because it">
        <ul>
          <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
        </ul>
      </NAlert>

      <NAlert v-else-if="s.kind === 'approval'" type="success" :show-icon="false">
        <MarkdownText inline :source="s.inline" />
      </NAlert>

      <div v-else-if="BLOCKS[s.kind]" class="block" :class="BLOCKS[s.kind]!.cls">
        <div class="block-title">{{ BLOCKS[s.kind]!.sign }} {{ BLOCKS[s.kind]!.title }}</div>
        <div v-for="(it, j) in s.items" :key="j" class="line">
          <template v-if="it.change">
            <code>{{ it.change.id }}</code>
            <div class="was"><MarkdownText inline :source="it.change.before" /></div>
            <div class="becomes"><MarkdownText inline :source="it.change.after" /></div>
          </template>
          <MarkdownText v-else inline :source="it.text" />
          <div v-for="(sub, k) in it.sub" :key="k" class="sub"><MarkdownText inline :source="sub" /></div>
        </div>
      </div>

      <div v-else-if="s.kind === 'definitions'" class="defs">
        <span class="block-title">Definitions</span>
        <span v-for="(it, j) in s.items" :key="j" style="display: inline-flex; gap: 6px; align-items: center">
          <NTag
            size="small"
            :type="it.status === 'added' ? 'success' : it.status === 'removed' ? 'error' : 'warning'"
            :bordered="false"
          >
            {{ it.status ?? "" }}
          </NTag>
          <MarkdownText inline :source="it.text" />
        </span>
      </div>

      <div v-else-if="s.kind === 'reviewer'" class="reviewer">
        <div class="block-title">Reviewer</div>
        <MarkdownText :source="[s.inline, s.text].filter(Boolean).join('\n')" />
      </div>

      <div v-else-if="s.kind === 'gate'">
        <span class="block-title">Gate </span>
        <NTag size="small" :type="/pass/i.test(s.inline) ? 'success' : 'error'" :bordered="false">{{ s.inline }}</NTag>
      </div>

      <div v-else>
        <div class="block-title">{{ s.title }}</div>
        <MarkdownText v-if="s.inline" :source="s.inline" />
        <ul v-if="s.items.length">
          <li v-for="(it, j) in s.items" :key="j"><MarkdownText inline :source="it.text" /></li>
        </ul>
        <MarkdownText v-if="s.text" :source="s.text" />
      </div>
    </template>
  </div>
</template>

<style scoped>
.summary {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.summary ul {
  margin: 4px 0;
  padding-left: 20px;
}
.block {
  border-left: 4px solid;
  border-radius: 4px;
  padding: 6px 10px;
}
.block-title {
  font-weight: 600;
  font-size: 13px;
  margin-bottom: 4px;
}
.line {
  padding: 3px 0;
}
.sub {
  font-size: 12px;
  opacity: 0.7;
  padding-left: 12px;
}
.added {
  border-color: #18a058;
  background: rgba(24, 160, 88, 0.1);
}
.removed {
  border-color: #d03050;
  background: rgba(208, 48, 80, 0.09);
}
.changed {
  border-color: #f0a020;
  background: rgba(240, 160, 32, 0.1);
}
.was {
  text-decoration: line-through;
  opacity: 0.7;
  padding-left: 12px;
}
.was::before {
  content: "was ";
  text-decoration: none;
  display: inline-block;
  margin-right: 4px;
}
.becomes {
  padding-left: 12px;
}
.becomes::before {
  content: "now ";
  margin-right: 4px;
}
.defs {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}
.reviewer {
  border-left: 3px solid rgba(127, 127, 127, 0.4);
  padding-left: 10px;
}
</style>
