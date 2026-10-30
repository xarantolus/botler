<script setup lang="ts">
import { NAlert, NButton, NCard, NInput, NSpace, NText } from "naive-ui";
import { ref } from "vue";
import { login } from "../store";

const token = ref("");
const error = ref("");
const busy = ref(false);

async function submit() {
  const t = token.value.trim();
  if (!t) return;
  busy.value = true;
  error.value = "";
  try {
    await login(t);
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div style="max-width: 440px; margin: 16vh auto; padding: 0 16px">
    <NCard title="botler">
      <NSpace vertical>
        <NText depth="3">Paste your user token (from <code>botler token</code>).</NText>
        <NInput
          v-model:value="token"
          type="password"
          show-password-on="click"
          placeholder="user token"
          :input-props="{ autocomplete: 'off' }"
          @keydown.enter="submit"
        />
        <NAlert v-if="error" type="error" :show-icon="false">{{ error }}</NAlert>
        <NButton type="primary" block :loading="busy" @click="submit">Open</NButton>
      </NSpace>
    </NCard>
  </div>
</template>
