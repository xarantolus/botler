<script setup lang="ts">
// Form fields generated from a JSON Schema: the hub's schemars output for its config types, and
// the forms agents send with questions. Strings, numbers and switches, choices and multi-select,
// dates, long text and files (uploaded right away; the value is the upload), nested objects as
// indented groups, nullable fields cleared to null when emptied.
import { computed, ref } from "vue";
import { upload } from "../../api/client";
import type { FieldError } from "../../generated";
import { notifyError } from "../../notify";
import { errorsAt, fieldKind, humanize, resolve, type Schema } from "../../lib/settings";

defineOptions({ name: "SchemaFields" });
const props = defineProps<{ root: Schema; node: Schema; path: string; errors: FieldError[] }>();
const value = defineModel<Record<string, unknown>>({ required: true });
const uploading = ref<string | null>(null);

const fields = computed(() => {
  const parent = resolve(props.root, props.node).node;
  return Object.entries(parent.properties ?? {}).map(([key, s]) => {
    const { node, nullable } = resolve(props.root, s);
    const required = parent.required?.includes(key) ?? false;
    const options = (node.enum ?? node.items?.enum ?? []).map((v) => ({ label: String(v), value: v as string }));
    return { key, node, nullable, required, options, kind: fieldKind(node), label: node.title ?? humanize(key) };
  });
});

const INPUT_TYPE = { integer: "number", number: "number", date: "date", "date-time": "datetime-local", time: "time" } as const;

function setText(key: string, v: string | number, numeric: boolean, nullable: boolean) {
  const s = String(v).trim();
  value.value[key] = s === "" && nullable ? null : numeric && s !== "" ? Number(s) : s;
}

async function pick(key: string, e: Event, many: boolean) {
  const list = Array.from((e.target as HTMLInputElement).files ?? []);
  if (!list.length) return;
  uploading.value = key;
  try {
    const done = await Promise.all(list.map((f) => upload(f, () => {})));
    value.value[key] = many ? [...((value.value[key] as string[]) ?? []), ...done.map((u) => u.file)] : done[0]!.file;
  } catch (err) {
    notifyError("Could not upload", err);
  } finally {
    uploading.value = null;
  }
}

function unpick(key: string, file: string) {
  const v = value.value[key];
  value.value[key] = Array.isArray(v) ? v.filter((f) => f !== file) : undefined;
}

const picked = (key: string) => {
  const v = value.value[key];
  return (Array.isArray(v) ? v : v ? [v] : []) as string[];
};
</script>

<template>
  <div class="space-y-4">
    <template v-for="f in fields" :key="f.key">
      <div v-if="f.kind === 'object'" class="space-y-3 border-s-2 border-default ps-4">
        <p class="text-sm font-medium text-highlighted">{{ f.label }}</p>
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
        :label="f.label"
        :description="f.node.description"
        :required="f.required"
        :error="errorsAt(errors, `${path}.${f.key}`).join('; ') || undefined"
        :hint="f.nullable ? 'optional' : undefined"
      >
        <USwitch
          v-if="f.kind === 'boolean'"
          :model-value="!!value[f.key]"
          @update:model-value="(v: boolean) => (value[f.key] = v)"
        />
        <USelect
          v-else-if="f.kind === 'choice'"
          :model-value="value[f.key] as string | undefined"
          :items="f.options"
          class="w-full max-w-xs"
          @update:model-value="(v: unknown) => (value[f.key] = v)"
        />
        <USelectMenu
          v-else-if="f.kind === 'multi'"
          :model-value="(value[f.key] as string[] | undefined) ?? []"
          :items="f.options"
          value-key="value"
          multiple
          class="w-full max-w-md"
          @update:model-value="(v: unknown) => (value[f.key] = v)"
        />
        <UTextarea
          v-else-if="f.kind === 'multiline'"
          :model-value="(value[f.key] as string | undefined) ?? ''"
          autoresize
          :rows="3"
          class="w-full"
          @update:model-value="(v: string) => setText(f.key, v, false, f.nullable)"
        />
        <div v-else-if="f.kind === 'file' || f.kind === 'files'" class="flex flex-col gap-1.5">
          <div v-for="file in picked(f.key)" :key="file" class="flex items-center gap-2 text-sm">
            <UIcon name="i-lucide-paperclip" class="size-4 shrink-0 text-muted" />
            <span class="min-w-0 truncate">{{ file.split("/").pop() }}</span>
            <UButton icon="i-lucide-x" size="xs" color="neutral" variant="ghost" :aria-label="`Remove ${file}`" @click="unpick(f.key, file)" />
          </div>
          <label v-if="f.kind === 'files' || !picked(f.key).length" class="w-fit">
            <input
              type="file"
              class="sr-only"
              :multiple="f.kind === 'files'"
              :aria-label="f.label"
              @change="pick(f.key, $event, f.kind === 'files')"
            />
            <span
              class="inline-flex cursor-pointer items-center gap-1.5 rounded-md border border-default px-2.5 py-1.5 text-sm hover:bg-elevated"
            >
              <UIcon :name="uploading === f.key ? 'i-lucide-loader-circle' : 'i-lucide-upload'" :class="['size-4', uploading === f.key && 'animate-spin']" />
              {{ f.kind === "files" ? "Add files" : "Choose a file" }}
            </span>
          </label>
        </div>
        <UInput
          v-else
          :model-value="value[f.key] === null || value[f.key] === undefined ? '' : String(value[f.key])"
          :type="INPUT_TYPE[f.kind as keyof typeof INPUT_TYPE] ?? 'text'"
          :step="f.kind === 'number' ? 'any' : undefined"
          class="w-full max-w-xs"
          @update:model-value="(v: string | number) => setText(f.key, v, f.kind === 'integer' || f.kind === 'number', f.nullable)"
        />
      </UFormField>
    </template>
  </div>
</template>
