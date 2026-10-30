<script setup lang="ts">
import { NCollapse, NCollapseItem, NTag } from "naive-ui";
import { computed } from "vue";
import type { SessionItem, ToolCall } from "../generated";

type ToolResult = Extract<SessionItem, { kind: "tool_result" }>;

const props = defineProps<{ call?: ToolCall; result?: ToolResult }>();

const name = computed(() => props.call?.name ?? props.result?.tool ?? "tool");
const args = computed(() => {
  const a = props.call?.arguments as Record<string, unknown> | null | undefined;
  if (a && typeof a === "object" && typeof a.code === "string" && Object.keys(a).length === 1) return a.code;
  return JSON.stringify(a ?? null, null, 2);
});
</script>

<template>
  <NCollapse class="tool" :default-expanded-names="[]">
    <NCollapseItem :name="1">
      <template #header>
        <span style="font-size: 13px">
          <code>{{ name }}</code>
          <NTag v-if="result?.is_error" size="tiny" type="error" :bordered="false" style="margin-left: 6px">error</NTag>
          <NTag v-else-if="call && !result" size="tiny" :bordered="false" style="margin-left: 6px">running</NTag>
        </span>
      </template>
      <div v-if="call" style="font-size: 12px; opacity: 0.7">arguments</div>
      <pre v-if="call" class="mono">{{ args }}</pre>
      <template v-if="result">
        <div style="font-size: 12px; opacity: 0.7">result</div>
        <pre class="mono" :style="result.is_error ? 'color: #d03050' : ''">{{ result.text || "(empty)" }}</pre>
      </template>
    </NCollapseItem>
  </NCollapse>
</template>

<style scoped>
.tool :deep(.n-collapse-item__header) {
  padding-top: 2px !important;
}
.tool :deep(.n-collapse-item) {
  margin: 2px 0 !important;
}
</style>
