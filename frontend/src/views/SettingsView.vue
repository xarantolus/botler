<script setup lang="ts">
// Instance settings from the monorepo: models (the default and the ones we typically use), each
// agent's model, tools, MCP services and idle timeout, and deployment defaults and push. Saving
// commits only what changed on main, as the user; problems come back per field.
import type { TabsItem } from "@nuxt/ui";
import { computed, onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import ViewPanel from "../components/common/ViewPanel.vue";
import ChannelsPanel from "../components/settings/ChannelsPanel.vue";
import ModelPicker from "../components/settings/ModelPicker.vue";
import SchemaFields from "../components/settings/SchemaFields.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import type { CatalogView, FieldError, Settings } from "../generated";
import { changes, draftOf, errorsAt, generalErrors, usedBy, type Draft, type Schema } from "../lib/settings";
import { notifyError, notifySuccess } from "../notify";

const loaded = shallowRef<Settings | null>(null);
const catalog = shallowRef<CatalogView | null>(null);
const draft = ref<Draft | null>(null);
const errors = ref<FieldError[]>([]);
const warnings = ref<FieldError[]>([]);
const loading = ref(false);
const saving = ref(false);
const tab = ref("models");
const tabs: TabsItem[] = [
  { label: "Models", value: "models", icon: "i-lucide-cpu" },
  { label: "Agents", value: "agents", icon: "i-lucide-bot" },
  { label: "Deployment", value: "deployment", icon: "i-lucide-sliders-horizontal" },
  { label: "Channels", value: "channels", icon: "i-lucide-radio-tower" },
];

async function load() {
  loading.value = true;
  try {
    const [s, c] = await Promise.all([call("settings", {}), catalog.value ?? call("models", {})]);
    loaded.value = s;
    catalog.value = c;
    draft.value = draftOf(s);
    errors.value = [];
    warnings.value = s.warnings;
  } catch (e) {
    notifyError("Could not load the settings", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

const edit = computed(() => (loaded.value && draft.value ? changes(loaded.value, draft.value) : null));
const models = computed(() => catalog.value?.models ?? []);
const typical = computed(() => draft.value?.models.models ?? []);
const users = computed(() => usedBy(loaded.value?.agents ?? []));
const general = computed(() => generalErrors(errors.value, ["models", "agents", "defaults", "push"]));
const defaults = computed(() => draft.value?.defaults ?? {});

async function save() {
  if (!edit.value) return;
  saving.value = true;
  try {
    const r = await call("save_settings", { body: edit.value });
    if (!r.saved) {
      errors.value = r.errors;
      notifyError("Settings not saved", new Error(`${r.errors.length} problem(s); see the marked fields`));
      return;
    }
    notifySuccess("Settings saved", `Committed on main as ${r.commit.slice(0, 8)}.`);
    await load();
    warnings.value = r.warnings;
  } catch (e) {
    notifyError("Could not save the settings", e);
  } finally {
    saving.value = false;
  }
}

function addModel() {
  draft.value?.models.models.push({ id: "", note: "" });
}

function setPush(on: boolean) {
  if (draft.value) draft.value.push = on ? { ntfy_url: "", topic_secret: "" } : null;
}
</script>

<template>
  <ViewPanel id="settings" title="Settings" icon="i-lucide-settings">
    <template #right>
      <UButton
        v-if="edit"
        label="Discard"
        color="neutral"
        variant="ghost"
        @click="loaded && ((draft = draftOf(loaded)), (errors = []))"
      />
      <UButton label="Save" icon="i-lucide-save" :disabled="!edit" :loading="saving" @click="save" />
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <template #toolbar>
      <UTabs v-model="tab" :items="tabs" :content="false" variant="link" size="sm" />
    </template>

    <div v-if="!draft || !loaded" class="mx-auto w-full max-w-3xl space-y-4">
      <USkeleton v-for="n in 3" :key="n" class="h-32 w-full rounded-lg" />
    </div>
    <div v-else class="mx-auto w-full max-w-3xl space-y-4">
      <UAlert
        v-for="(e, i) in general"
        :key="`e${i}`"
        color="error"
        variant="subtle"
        icon="i-lucide-circle-alert"
        :title="e.field"
        :description="e.message"
      />
      <UAlert
        v-for="(w, i) in warnings"
        :key="`w${i}`"
        color="warning"
        variant="subtle"
        icon="i-lucide-triangle-alert"
        :title="w.field"
        :description="w.message"
      />

      <template v-if="tab === 'models'">
        <UAlert
          v-if="loaded.routes.length"
          color="neutral"
          variant="subtle"
          icon="i-lucide-info"
          :description="`botler.toml still has the deprecated routes ${loaded.routes.join(', ')}; they keep working by name, but agents and the default should name model ids.`"
        />
        <UCard>
          <template #header>
            <p class="font-semibold text-highlighted">Default model</p>
            <p class="text-sm text-muted">What agents without their own model run on. Any OpenRouter model id works.</p>
          </template>
          <UFormField :error="errorsAt(errors, 'models.default').join('; ') || undefined">
            <ModelPicker v-model="draft.models.default" :catalog="models" :typical="typical" />
          </UFormField>
          <p v-if="draft.models.default && users.get(draft.models.default)" class="mt-2 text-xs text-muted">
            Used by {{ users.get(draft.models.default)?.join(", ") }}
          </p>
        </UCard>
        <UCard :ui="{ body: 'divide-y divide-default p-0 sm:p-0' }">
          <template #header>
            <div class="flex items-center gap-2">
              <div class="min-w-0 flex-1">
                <p class="font-semibold text-highlighted">Typically used</p>
                <p class="text-sm text-muted">
                  Suggestions for agents and these pickers, with what each is good for; not a whitelist.
                </p>
              </div>
              <UButton label="Add" icon="i-lucide-plus" color="neutral" variant="outline" size="sm" @click="addModel" />
            </div>
          </template>
          <div
            v-for="(m, i) in draft.models.models"
            :key="i"
            class="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-start sm:px-6"
          >
            <UFormField class="min-w-0 flex-1" :error="errorsAt(errors, `models.model.${i}`).join('; ') || undefined">
              <ModelPicker
                :model-value="m.id || null"
                :catalog="models"
                :typical="[]"
                @update:model-value="(v) => (m.id = v ?? '')"
              />
              <p v-if="users.get(m.id)" class="mt-1 text-xs text-muted">Used by {{ users.get(m.id)?.join(", ") }}</p>
            </UFormField>
            <UInput v-model="m.note" placeholder="What it is good for" class="min-w-0 flex-1" />
            <UButton
              icon="i-lucide-trash-2"
              color="neutral"
              variant="ghost"
              aria-label="Remove"
              @click="draft.models.models.splice(i, 1)"
            />
          </div>
          <p v-if="!draft.models.models.length" class="px-6 py-4 text-sm text-muted">None listed.</p>
        </UCard>
        <p class="text-xs text-muted">
          <template v-if="catalog?.fetched_at">
            Prices and limits from OpenRouter's model list ({{ models.length }} models), fetched
            <TimeAgo :at="catalog.fetched_at" />.
          </template>
          <template v-else>OpenRouter's model list is unavailable{{ catalog?.error ? `: ${catalog.error}` : "" }}.</template>
        </p>
      </template>

      <template v-else-if="tab === 'agents'">
        <UCard v-for="a in loaded.agents" :key="a.name">
          <template #header>
            <p class="font-semibold text-highlighted">{{ a.name }}</p>
            <p class="text-sm text-muted">{{ a.description }}</p>
          </template>
          <div v-if="draft.agents[a.name]" class="space-y-4">
            <UAlert
              v-for="(msg, i) in errors.filter((e) => e.field === `agents.${a.name}`).map((e) => e.message)"
              :key="i"
              color="error"
              variant="subtle"
              :description="msg"
            />
            <UFormField label="Model" :error="errorsAt(errors, `agents.${a.name}.model`).join('; ') || undefined">
              <ModelPicker
                v-model="draft.agents[a.name]!.model"
                :catalog="models"
                :typical="typical"
                :placeholder="`Default: ${draft.models.default ?? 'none'}`"
              />
            </UFormField>
            <UFormField
              label="Tools"
              :description="loaded.tools.length ? `Globs of: ${loaded.tools.join(', ')}` : 'No Python tools in the monorepo yet.'"
            >
              <UInputTags v-model="draft.agents[a.name]!.tools" placeholder="e.g. web.*" class="w-full" />
            </UFormField>
            <UFormField
              label="MCP services"
              :description="loaded.services.length ? 'Services whose MCP server this agent uses.' : 'No services with a port yet.'"
            >
              <USelectMenu
                v-model="draft.agents[a.name]!.mcp"
                :items="loaded.services"
                multiple
                placeholder="None"
                class="w-full"
                :disabled="!loaded.services.length && !draft.agents[a.name]!.mcp.length"
              />
              <p v-if="a.mcp_stdio.length" class="mt-1 text-xs text-muted">
                Also starts its own MCP servers: {{ a.mcp_stdio.join(", ") }} (edited in its agent.toml).
              </p>
            </UFormField>
            <UFormField label="Idle timeout" :error="errorsAt(errors, `agents.${a.name}.idle_timeout`).join('; ') || undefined">
              <UInput
                :model-value="draft.agents[a.name]!.idle_timeout ?? ''"
                :placeholder="`Default: ${String(defaults.idle_timeout)}`"
                class="w-full max-w-xs"
                @update:model-value="(v: string | number) => (draft!.agents[a.name]!.idle_timeout = String(v).trim() || null)"
              />
            </UFormField>
          </div>
        </UCard>
      </template>

      <ChannelsPanel v-else-if="tab === 'channels'" />

      <template v-else>
        <UCard>
          <template #header>
            <p class="font-semibold text-highlighted">Defaults</p>
            <p class="text-sm text-muted">For agents and jobs that do not set their own.</p>
          </template>
          <SchemaFields
            v-model="draft.defaults"
            :root="loaded.defaults_schema as Schema"
            :node="loaded.defaults_schema as Schema"
            path="defaults"
            :errors="errors"
          />
        </UCard>
        <UCard>
          <template #header>
            <div class="flex items-center gap-2">
              <div class="min-w-0 flex-1">
                <p class="font-semibold text-highlighted">Push notifications</p>
                <p class="text-sm text-muted">Blocking requests pushed to your phone with ntfy; the topic is a secret.</p>
              </div>
              <USwitch :model-value="draft.push !== null" @update:model-value="setPush" />
            </div>
          </template>
          <UFormField v-if="draft.push !== null" :error="errorsAt(errors, 'push').join('; ') || undefined">
            <SchemaFields
              v-model="draft.push"
              :root="loaded.push_schema as Schema"
              :node="loaded.push_schema as Schema"
              path="push"
              :errors="[]"
            />
          </UFormField>
          <p v-else class="text-sm text-muted">Off.</p>
        </UCard>
      </template>
    </div>
  </ViewPanel>
</template>
