<script setup lang="ts">
// Secret groups declared in the monorepo, which values are set, and setting or replacing them.
import { onMounted, reactive, ref, shallowRef } from "vue";
import { call } from "../api/client";
import SecretValueInput from "../components/common/SecretValueInput.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { SecretGroupView, SecretInput } from "../generated";
import { notifyError, notifySuccess } from "../notify";

const groups = shallowRef<SecretGroupView[] | null>(null);
const loading = ref(false);
const values = reactive<Record<string, SecretInput | null | undefined>>({});
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
    notifySuccess(`${name} saved`, "Stored encrypted; agents granted the group get it on their next start.");
    await load();
  } catch (e) {
    notifyError(`Could not save ${name}`, e);
  } finally {
    saving.value = null;
  }
}
</script>

<template>
  <ViewPanel id="secrets" title="Secrets" icon="i-lucide-key-round">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <div class="mx-auto w-full max-w-3xl space-y-4">
      <UAlert
        color="neutral"
        variant="subtle"
        icon="i-lucide-lock"
        description="Groups are declared in the monorepo's secrets.toml (agents add them through a request or a proposal). Values are stored encrypted by the hub and never shown again."
      />
      <div v-if="!groups" class="space-y-4">
        <USkeleton v-for="n in 2" :key="n" class="h-40 w-full rounded-lg" />
      </div>
      <UEmpty
        v-else-if="!groups.length"
        icon="i-lucide-key-round"
        title="No secret groups declared"
        description="When an agent needs a credential, it asks for one and the group appears here."
        variant="naked"
        class="py-12"
      />
      <UCard v-for="g in groups ?? []" :key="g.name" :ui="{ body: 'divide-y divide-default p-0 sm:p-0' }">
        <template #header>
          <div class="flex flex-wrap items-center gap-2">
            <UIcon name="i-lucide-folder-key" class="size-5 text-muted" />
            <span class="font-semibold text-highlighted">{{ g.name }}</span>
            <UBadge color="neutral" variant="subtle" size="sm" :label="g.class" />
            <UBadge
              :color="g.complete ? 'success' : 'error'"
              variant="subtle"
              size="sm"
              :icon="g.complete ? 'i-lucide-check' : 'i-lucide-circle-alert'"
              :label="g.complete ? 'complete' : 'incomplete'"
            />
          </div>
          <p class="mt-1.5 text-sm text-muted">
            {{ g.description }}
            <template v-if="g.granted_to.length"> · used by {{ g.granted_to.join(", ") }}</template>
            <template v-else-if="!g.held_by.length"> · not granted to anything yet</template>
          </p>
          <div v-if="g.held_by.length" class="mt-1.5 flex flex-wrap items-center gap-1.5 text-sm text-muted">
            Held by chats:
            <ULink
              v-for="c in g.held_by"
              :key="c"
              :to="`/chat/${encodeURIComponent(c)}`"
              class="font-medium text-default hover:underline"
            >
              {{ c }}
            </ULink>
          </div>
        </template>
        <form
          v-for="f in g.fields"
          :key="f.name"
          class="flex flex-col gap-2 px-4 py-4 sm:flex-row sm:items-start sm:px-6"
          @submit.prevent="save(f.name)"
        >
          <UFormField
            :label="f.name"
            :name="f.name"
            :description="[f.file ? `File ${f.file}` : 'Environment variable', f.description].filter(Boolean).join(' · ')"
            class="min-w-0 flex-1"
            :ui="{ label: 'font-mono' }"
          >
            <template #hint>
              <UBadge
                :color="f.has_value ? 'success' : 'error'"
                variant="subtle"
                size="sm"
                :label="f.has_value ? 'set' : 'missing'"
              />
            </template>
            <SecretValueInput
              :id="`secret-${f.name}`"
              v-model="values[f.name]"
              :file="!!f.file"
              :placeholder="f.has_value ? 'Replace the value' : undefined"
            />
          </UFormField>
          <UButton
            type="submit"
            label="Save"
            icon="i-lucide-lock"
            :disabled="!values[f.name]"
            :loading="saving === f.name"
            class="self-end sm:mt-6 sm:self-start"
          />
        </form>
      </UCard>
    </div>
  </ViewPanel>
</template>
