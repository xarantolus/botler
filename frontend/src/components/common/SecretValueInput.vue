<script setup lang="ts">
// A secret value: typed or pasted, or (for file secrets) uploaded. Text files are sent as text,
// binary files as base64 of their exact bytes. The content of an uploaded file is never shown.
import { ref, watch } from "vue";
import type { SecretInput } from "../../generated";
import { loadFile, type LoadedFile } from "../../lib/files";
import { bytes } from "../../lib/time";
import { notifyError } from "../../notify";

const props = defineProps<{ file: boolean; placeholder?: string; id?: string }>();
const model = defineModel<SecretInput | null | undefined>({ required: true });

const typed = ref("");
const shown = ref(false);
const picked = ref<File | null | undefined>(null);
const loaded = ref<LoadedFile | null>(null);

watch(typed, (v) => {
  if (!loaded.value) model.value = v ? v : null;
});
watch(model, (v) => {
  // Cleared by the parent (after saving).
  if (v === null || v === undefined) {
    typed.value = "";
    picked.value = null;
    loaded.value = null;
  }
});
watch(picked, async (f) => {
  if (!f) {
    loaded.value = null;
    model.value = typed.value || null;
    return;
  }
  try {
    loaded.value = await loadFile(f);
    model.value = loaded.value.value;
  } catch (e) {
    picked.value = null;
    notifyError(`Could not read ${f.name}`, e);
  }
});
</script>

<template>
  <UInput
    v-if="!props.file"
    :id="id"
    v-model="typed"
    :type="shown ? 'text' : 'password'"
    :placeholder="placeholder ?? 'Value'"
    autocomplete="off"
    class="w-full"
    :ui="{ trailing: 'pe-1' }"
  >
    <template #trailing>
      <UButton
        color="neutral"
        variant="link"
        size="sm"
        :icon="shown ? 'i-lucide-eye-off' : 'i-lucide-eye'"
        :aria-label="shown ? 'Hide the value' : 'Show the value'"
        :aria-pressed="shown"
        @click="shown = !shown"
      />
    </template>
  </UInput>
  <div v-else class="flex flex-col gap-2">
    <UTextarea
      v-if="!picked"
      :id="id"
      v-model="typed"
      :placeholder="placeholder ?? 'Paste the file contents, or upload the file below'"
      autoresize
      :rows="2"
      :maxrows="8"
      autocomplete="off"
      spellcheck="false"
      class="w-full"
      :ui="{ base: 'font-mono text-xs' }"
    />
    <UFileUpload
      v-model="picked"
      label="Drop a file here"
      description="or click to choose one; it is stored encrypted and never shown again"
      icon="i-lucide-file-key-2"
      layout="list"
      :preview="false"
      class="w-full"
      :ui="{ base: 'min-h-20' }"
    >
      <template #file-size="{ file }">
        {{ bytes(file.size) }}<template v-if="loaded"> · {{ loaded.binary ? "binary, sent as base64" : "text" }}</template>
      </template>
    </UFileUpload>
  </div>
</template>
