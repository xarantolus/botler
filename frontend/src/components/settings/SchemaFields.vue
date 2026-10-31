<script setup lang="ts">
// Form fields generated from a JSON Schema (the hub's schemars output for its config types):
// strings, numbers and switches, nested objects as indented groups, nullable fields cleared to
// null when emptied.
import { computed } from "vue";
import type { FieldError } from "../../generated";
import { errorsAt, humanize, resolve, type Schema } from "../../lib/settings";

defineOptions({ name: "SchemaFields" });
const props = defineProps<{ root: Schema; node: Schema; path: string; errors: FieldError[] }>();
const value = defineModel<Record<string, unknown>>({ required: true });

const fields = computed(() =>
  Object.entries(resolve(props.root, props.node).node.properties ?? {}).map(([key, s]) => {
    const { node, nullable } = resolve(props.root, s);
    return { key, node, nullable, kind: node.properties ? "object" : (node.type ?? "string") };
  }),
);

function setText(key: string, v: string | number, numeric: boolean, nullable: boolean) {
  const s = String(v).trim();
  value.value[key] = s === "" && nullable ? null : numeric && s !== "" ? Number(s) : s;
}
</script>

<template>
  <div class="space-y-4">
    <template v-for="f in fields" :key="f.key">
      <div v-if="f.kind === 'object'" class="space-y-3 border-s-2 border-default ps-4">
        <p class="text-sm font-medium text-highlighted">{{ humanize(f.key) }}</p>
        <p v-if="f.node.description" class="-mt-2 text-xs text-muted">{{ f.node.description }}</p>
        <SchemaFields
          v-model="value[f.key] as Record<string, unknown>"
          :root="root"
          :node="f.node"
          :path="`${path}.${f.key}`"
          :errors="errors"
        />
      </div>
      <UFormField
        v-else
        :label="humanize(f.key)"
        :description="f.node.description"
        :error="errorsAt(errors, `${path}.${f.key}`).join('; ') || undefined"
        :hint="f.nullable ? 'optional' : undefined"
      >
        <USwitch
          v-if="f.kind === 'boolean'"
          :model-value="!!value[f.key]"
          @update:model-value="(v: boolean) => (value[f.key] = v)"
        />
        <UInput
          v-else
          :model-value="value[f.key] === null || value[f.key] === undefined ? '' : String(value[f.key])"
          :type="f.kind === 'integer' || f.kind === 'number' ? 'number' : 'text'"
          :step="f.kind === 'number' ? 'any' : undefined"
          class="w-full max-w-xs"
          @update:model-value="(v: string | number) => setText(f.key, v, f.kind === 'integer' || f.kind === 'number', f.nullable)"
        />
      </UFormField>
    </template>
  </div>
</template>
