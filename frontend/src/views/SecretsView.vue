<script setup lang="ts">
import { NAlert, NButton, NCard, NEmpty, NScrollbar, NSpace, NTag, NText } from "naive-ui";
import { onMounted, reactive, ref, shallowRef } from "vue";
import { call } from "../api/client";
import SecretValueInput from "../components/SecretValueInput.vue";
import type { SecretGroupView, SecretInput } from "../generated";
import { message, notifyError } from "../notify";

const groups = shallowRef<SecretGroupView[]>([]);
const loading = ref(false);
const values = reactive<Record<string, SecretInput | null>>({});
const saving = ref<string | null>(null);

async function load() {
  loading.value = true;
  try {
    groups.value = await call("secrets", {});
  } catch (e) {
    notifyError("Could not load secrets", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

async function save(name: string) {
  const value = values[name];
  if (!value) return;
  saving.value = name;
  try {
    await call("set_secret", { params: { name }, body: { value } });
    values[name] = null;
    message.success(`${name} saved`);
    await load();
  } catch (e) {
    notifyError(`Could not save ${name}`, e);
  } finally {
    saving.value = null;
  }
}
</script>

<template>
  <NScrollbar style="height: 100%">
    <div style="max-width: 860px; margin: 0 auto; padding: 12px 16px">
      <NSpace vertical size="large">
        <NAlert type="info" :show-icon="false">
          Groups are declared in the monorepo's <code>secrets.toml</code> (agents add them through a request or a
          proposal). Values are stored encrypted by the hub and never shown again.
        </NAlert>
        <NEmpty v-if="!loading && !groups.length" description="No secret groups declared." />
        <NCard v-for="g in groups" :key="g.name" size="small">
          <template #header>
            <NSpace size="small" align="center">
              <strong>{{ g.name }}</strong>
              <NTag size="small" :bordered="false">{{ g.class }}</NTag>
              <NTag size="small" :bordered="false" :type="g.complete ? 'success' : 'error'">
                {{ g.complete ? "complete" : "incomplete" }}
              </NTag>
            </NSpace>
          </template>
          <NText depth="3" style="font-size: 13px">
            {{ g.description }}
            <template v-if="g.granted_to.length"> · used by {{ g.granted_to.join(", ") }}</template>
            <template v-else> · not granted to anything yet</template>
          </NText>
          <div v-for="f in g.fields" :key="f.name" style="margin-top: 12px">
            <div style="font-size: 13px; margin-bottom: 4px">
              <code>{{ f.name }}</code>
              <NTag size="tiny" :bordered="false" :type="f.has_value ? 'success' : 'error'" style="margin: 0 6px">
                {{ f.has_value ? "set" : "missing" }}
              </NTag>
              <NText depth="3">{{ f.file ? `file ${f.file}` : "environment variable" }}</NText>
              <div v-if="f.description"><NText depth="3">{{ f.description }}</NText></div>
            </div>
            <div style="display: flex; gap: 8px; align-items: flex-start">
              <div style="flex: 1">
                <SecretValueInput
                  v-model="values[f.name]!"
                  :file="!!f.file"
                  :placeholder="f.has_value ? 'replace the value' : undefined"
                />
              </div>
              <NButton :disabled="!values[f.name]" :loading="saving === f.name" @click="save(f.name)">Save</NButton>
            </div>
          </div>
        </NCard>
      </NSpace>
    </div>
  </NScrollbar>
</template>
