<script setup lang="ts">
// One file of a proposal's diff: its header (status, protection, counts) and its hunks, unified
// or side by side.
import { computed, ref } from "vue";
import type { FileDiff } from "../../generated";
import { FILE_STATUS, splitRows, unifiedRows, type DiffRow } from "../../lib/proposals";

const props = defineProps<{ file: FileDiff; split: boolean }>();
const open = ref(true);
const rows = computed(() => unifiedRows(props.file.hunks));
const pairs = computed(() => (props.split ? splitRows(rows.value) : []));
const status = computed(() => FILE_STATUS[props.file.status]);

// Full class names, so Tailwind sees them.
const BG: Record<string, string> = { add: "bg-success/10", del: "bg-error/10", hunk: "bg-info/5 text-muted", note: "text-dimmed italic" };
const MARK: Record<string, string> = { add: "+", del: "-" };
const cls = (r: DiffRow | null) => (r ? (BG[r.kind] ?? "") : "bg-elevated/40");
</script>

<template>
  <section class="overflow-hidden rounded-md border border-default">
    <header class="flex flex-wrap items-center gap-2 bg-elevated/50 px-3 py-2 text-sm">
      <UButton
        :icon="open ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'"
        color="neutral"
        variant="ghost"
        size="xs"
        :aria-label="open ? 'Collapse file' : 'Expand file'"
        @click="open = !open"
      />
      <UIcon :name="status.icon" class="size-4 shrink-0" :class="status.text" />
      <span class="min-w-0 flex-1 break-all font-mono text-xs text-highlighted">
        <template v-if="file.old_path">{{ file.old_path }} → </template>{{ file.path }}
      </span>
      <UBadge v-if="file.protected" color="warning" variant="subtle" size="sm" icon="i-lucide-shield-alert" label="protected" />
      <UBadge v-if="file.binary" color="neutral" variant="subtle" size="sm" label="binary" />
      <span class="font-mono text-xs tabular-nums">
        <span class="text-success">+{{ file.additions }}</span>
        <span class="ms-1 text-error">−{{ file.deletions }}</span>
      </span>
    </header>
    <template v-if="open">
      <p v-if="file.binary" class="px-3 py-2 text-sm text-muted">Binary file, not shown.</p>
      <p v-else-if="!file.hunks.length && !file.omitted" class="px-3 py-2 text-sm text-muted">
        {{ file.status === "renamed" ? "Renamed without changes." : "No content changes (mode only)." }}
      </p>
      <div v-else class="overflow-x-auto font-mono text-xs leading-5">
        <table class="w-full border-collapse" :class="split && 'table-fixed'">
          <colgroup v-if="split">
            <col class="w-12" />
            <col />
            <col class="w-12" />
            <col />
          </colgroup>
          <tbody v-if="!split">
            <tr v-for="(r, i) in rows" :key="i" :class="cls(r)">
              <template v-if="r.kind === 'hunk'">
                <td colspan="3" class="px-3 py-0.5">{{ r.text }}</td>
              </template>
              <template v-else>
                <td class="w-10 select-none px-2 text-right text-dimmed">{{ r.old ?? "" }}</td>
                <td class="w-10 select-none px-2 text-right text-dimmed">{{ r.new ?? "" }}</td>
                <td class="whitespace-pre pe-3"><span class="inline-block w-4 select-none text-dimmed">{{ MARK[r.kind] ?? "" }}</span>{{ r.text }}</td>
              </template>
            </tr>
          </tbody>
          <tbody v-else>
            <tr v-for="(p, i) in pairs" :key="i">
              <td v-if="p.kind === 'hunk'" colspan="4" class="truncate px-3 py-0.5" :class="BG.hunk">{{ p.text }}</td>
              <template v-else>
                <td class="select-none px-2 text-right align-top text-dimmed" :class="cls(p.left)">{{ p.left?.old ?? "" }}</td>
                <td class="whitespace-pre-wrap break-words pe-3" :class="cls(p.left)">{{ p.left?.text ?? "" }}</td>
                <td class="select-none border-s border-default px-2 text-right align-top text-dimmed" :class="cls(p.right)">
                  {{ p.right?.new ?? "" }}
                </td>
                <td class="whitespace-pre-wrap break-words pe-3" :class="cls(p.right)">{{ p.right?.text ?? "" }}</td>
              </template>
            </tr>
          </tbody>
        </table>
      </div>
      <p v-if="file.omitted" class="border-t border-default px-3 py-2 text-sm text-muted">
        <UIcon name="i-lucide-scissors" class="me-1 size-4 align-text-bottom" />
        Too large: {{ file.omitted }} lines omitted.
      </p>
    </template>
  </section>
</template>
