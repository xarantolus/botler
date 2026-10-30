<script setup lang="ts">
// A secret value: typed or pasted, or (for file secrets) uploaded. Text files are sent as text,
// binary files as base64 of their exact bytes. The content of an uploaded file is never shown.
import { NButton, NIcon, NInput, NTag, NText, NUpload, NUploadDragger, type UploadFileInfo } from "naive-ui";
import { CloudUploadOutline } from "@vicons/ionicons5";
import { ref, watch } from "vue";
import type { SecretInput } from "../generated";
import { loadFile, type LoadedFile } from "../lib/files";
import { bytes } from "../lib/time";
import { notifyError } from "../notify";

const props = defineProps<{ file: boolean; placeholder?: string }>();
const model = defineModel<SecretInput | null>({ required: true });

const typed = ref("");
const uploaded = ref<LoadedFile | null>(null);

watch(typed, (v) => {
  if (!uploaded.value) model.value = v ? v : null;
});
watch(model, (v) => {
  // Cleared by the parent (after saving).
  if (v === null) {
    typed.value = "";
    uploaded.value = null;
  }
});

async function onFile(info: { file: UploadFileInfo }) {
  const f = info.file.file;
  if (!f) return false;
  try {
    uploaded.value = await loadFile(f);
    model.value = uploaded.value.value;
  } catch (e) {
    notifyError(`Reading ${f.name}`, e);
  }
  return false; // nothing is uploaded anywhere; the value goes with the form
}

function clearFile() {
  uploaded.value = null;
  model.value = typed.value || null;
}
</script>

<template>
  <div v-if="!props.file">
    <NInput
      v-model:value="typed"
      type="password"
      show-password-on="click"
      :placeholder="placeholder ?? 'value'"
      :input-props="{ autocomplete: 'off' }"
    />
  </div>
  <div v-else class="file-input">
    <div v-if="uploaded" class="loaded">
      <NTag size="small" :bordered="false" type="info">{{ uploaded.name }}</NTag>
      <NText depth="3" style="font-size: 12px">{{ bytes(uploaded.size) }}{{ uploaded.binary ? " · binary" : "" }}</NText>
      <NButton size="tiny" quaternary @click="clearFile">remove</NButton>
    </div>
    <template v-else>
      <NInput
        v-model:value="typed"
        type="textarea"
        :autosize="{ minRows: 2, maxRows: 8 }"
        :placeholder="placeholder ?? 'paste the file contents, or upload it'"
        :input-props="{ autocomplete: 'off', spellcheck: false }"
        style="font-family: ui-monospace, monospace"
      />
      <NUpload :show-file-list="false" :max="1" :default-upload="false" @before-upload="onFile">
        <NUploadDragger style="padding: 8px">
          <NIcon :size="20" :depth="3"><CloudUploadOutline /></NIcon>
          <NText depth="3" style="font-size: 12px; display: block">Drop a file here or click to upload</NText>
        </NUploadDragger>
      </NUpload>
    </template>
  </div>
</template>

<style scoped>
.file-input {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.loaded {
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
