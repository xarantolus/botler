<script setup lang="ts">
// Apps: web UIs that services serve (`[app]` in their job.toml). Each opens in a frame from the
// hub's apps port, another origin than this app, signed in with a one-minute ticket.
import { computed, ref, watch } from "vue";
import { call } from "../api/client";
import ViewPanel from "../components/common/ViewPanel.vue";
import { notifyError } from "../notify";
import { refreshApps, state } from "../store";

const props = defineProps<{ name?: string }>();
const src = ref<string | null>(null);
const app = computed(() => state.apps?.find((a) => a.name === props.name) ?? null);

watch(
  () => props.name,
  async (name) => {
    src.value = null;
    if (!name) return void refreshApps();
    try {
      const t = await call("app_ticket", { params: { name } });
      src.value = `${location.protocol}//${location.hostname}:${t.port}${t.path}`;
    } catch (e) {
      notifyError("Could not open the app", e);
    }
  },
  { immediate: true },
);
</script>

<template>
  <ViewPanel id="apps" :title="app?.title ?? 'Apps'" icon="i-lucide-app-window">
    <template v-if="app" #right>
      <UButton
        v-if="src"
        :to="src"
        target="_blank"
        rel="noopener noreferrer"
        icon="i-lucide-external-link"
        color="neutral"
        variant="ghost"
        aria-label="Open in a new tab"
      />
    </template>
    <iframe
      v-if="name && src"
      :key="src"
      :src="src"
      :title="app?.title ?? name"
      class="h-full min-h-[70svh] w-full rounded-lg border border-default bg-white"
    />
    <div v-else-if="!name" class="mx-auto w-full max-w-3xl space-y-3">
      <p v-if="state.apps && !state.apps.length" class="text-sm text-muted">
        No apps yet. A service shows its web UI here with an <code class="font-mono">[app]</code> table in its
        job.toml; ask Botler to build one.
      </p>
      <ULink
        v-for="a in state.apps ?? []"
        :key="a.name"
        :to="`/apps/${encodeURIComponent(a.name)}`"
        class="flex items-center gap-3 rounded-lg border border-default p-3 hover:bg-elevated"
      >
        <UIcon :name="`i-lucide-${a.icon}`" class="size-5 text-muted" />
        <div class="min-w-0 flex-1">
          <p class="font-medium text-highlighted">{{ a.title }}</p>
          <p v-if="a.description" class="truncate text-sm text-muted">{{ a.description }}</p>
        </div>
        <UBadge v-if="!a.running" color="warning" variant="subtle" size="sm" label="not running" />
      </ULink>
    </div>
  </ViewPanel>
</template>
