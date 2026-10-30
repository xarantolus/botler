<script setup lang="ts">
// A chat's edges: what it holds (definition and grants, revocable), the events it subscribes to
// (removable; stale ones, which stopped applying while it was archived, are greyed out), who
// spawned it and whom it spawned, its pending timers and the services it uses.
import { ref, shallowRef, watch } from "vue";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import type { Connections, Grant, Subscription } from "../../generated";
import { notifyError, notifySuccess } from "../../notify";
import TimeAgo from "../common/TimeAgo.vue";

const props = defineProps<{ name: string }>();
const open = defineModel<boolean>("open", { default: false });
const data = shallowRef<Connections | null>(null);
const busy = ref<string | null>(null);

async function load() {
  try {
    data.value = await call("connections", { body: { chat: props.name } });
  } catch (e) {
    notifyError("Could not load connections", e);
  }
}
watch([open, () => props.name], ([o]) => o && void load(), { immediate: true });

const resource = (g: Grant) => `${g.kind}:${g.resource}${g.mode === "rw" ? " (rw)" : ""}`;
const filter = (s: Subscription) =>
  Object.entries(s.filter)
    .map(([k, v]) => `${k} = ${JSON.stringify(v)}`)
    .join(", ");

async function revoke(g: Grant) {
  const ok = g.stale !== null || (await confirm({
    title: `Revoke ${resource(g)}?`,
    description: "The chat is told, and its container no longer has it from its next start.",
    confirmLabel: "Revoke",
    color: "error",
  }));
  if (!ok) return;
  busy.value = g.id;
  try {
    await call("revoke_access", { body: { grant: g.id } });
    notifySuccess("Revoked", resource(g));
    await load();
  } catch (e) {
    notifyError("Could not revoke", e);
  } finally {
    busy.value = null;
  }
}

async function unsubscribe(s: Subscription) {
  busy.value = s.id;
  try {
    await call("unsubscribe", { body: { id: s.id } });
    notifySuccess("Subscription removed", s.pattern);
    await load();
  } catch (e) {
    notifyError("Could not remove the subscription", e);
  } finally {
    busy.value = null;
  }
}
</script>

<template>
  <USlideover v-model:open="open" title="Connections" :description="name" :ui="{ content: 'sm:max-w-lg' }">
    <template #body>
      <div v-if="!data" class="space-y-3">
        <USkeleton v-for="n in 4" :key="n" class="h-12 w-full" />
      </div>
      <div v-else class="space-y-6 text-sm">
        <UAlert
          v-if="data.archived"
          color="neutral"
          variant="subtle"
          icon="i-lucide-archive"
          description="Archived: its grants and subscriptions are suspended until it is restored."
        />

        <section class="space-y-2">
          <h3 class="font-semibold text-highlighted">Access</h3>
          <div v-if="data.definition_secrets.length" class="flex flex-wrap items-center gap-1.5 text-muted">
            From its definition:
            <UBadge v-for="s in data.definition_secrets" :key="s" color="neutral" variant="outline" icon="i-lucide-key-round" :label="s" />
          </div>
          <p v-if="!data.grants.length" class="text-muted">No grants beyond its definition.</p>
          <div v-for="g in data.grants" :key="g.id" class="flex items-center gap-2" :class="{ 'opacity-50': g.stale }">
            <UIcon :name="g.kind === 'secret' ? 'i-lucide-key-round' : 'i-lucide-shield-plus'" class="size-4 shrink-0 text-muted" />
            <div class="min-w-0 flex-1">
              <code class="block truncate font-mono" :class="{ 'line-through': g.stale }">{{ resource(g) }}</code>
              <p v-if="g.stale" class="text-xs text-muted">stale: {{ g.stale }}</p>
            </div>
            <UBadge v-if="g.stale" color="neutral" variant="subtle" size="sm" label="stale" />
            <UBadge v-else-if="g.suspended" color="neutral" variant="subtle" size="sm" label="suspended" />
            <span class="text-xs text-muted">{{ g.granted_by }} · <TimeAgo :at="g.created_at" /></span>
            <UButton size="xs" color="error" variant="ghost" :label="g.stale ? 'Remove' : 'Revoke'" :loading="busy === g.id" @click="revoke(g)" />
          </div>
        </section>

        <section class="space-y-2">
          <h3 class="font-semibold text-highlighted">Subscriptions</h3>
          <p v-if="!data.subscriptions.length" class="text-muted">None.</p>
          <div v-for="s in data.subscriptions" :key="s.id" class="flex items-start gap-2" :class="{ 'opacity-50': s.stale }">
            <UIcon :name="s.wake ? 'i-lucide-bell-ring' : 'i-lucide-bell-off'" class="mt-0.5 size-4 shrink-0 text-muted" />
            <div class="min-w-0 flex-1">
              <code class="font-mono">{{ s.pattern }}</code>
              <span v-if="filter(s)" class="text-muted"> where {{ filter(s) }}</span>
              <p v-if="s.note" class="truncate text-xs text-muted">{{ s.note }}</p>
              <p v-if="!s.wake" class="text-xs text-muted">quiet: read with whatever wakes it next</p>
              <p v-if="s.stale" class="text-xs text-muted">stale: {{ s.stale }}</p>
            </div>
            <UButton size="xs" color="neutral" variant="ghost" icon="i-lucide-trash-2" aria-label="Remove subscription" :loading="busy === s.id" @click="unsubscribe(s)" />
          </div>
        </section>

        <section class="space-y-2">
          <h3 class="font-semibold text-highlighted">Chats</h3>
          <div v-if="data.parent" class="flex items-center gap-2">
            <UIcon name="i-lucide-corner-left-up" class="size-4 text-muted" />
            Started by
            <ULink :to="`/chat/${encodeURIComponent(data.parent)}`" class="font-medium hover:underline" @click="open = false">{{ data.parent }}</ULink>
          </div>
          <div v-for="c in data.children" :key="c" class="flex items-center gap-2">
            <UIcon name="i-lucide-git-fork" class="size-4 text-muted" />
            Started
            <ULink :to="`/chat/${encodeURIComponent(c)}`" class="font-medium hover:underline" @click="open = false">{{ c }}</ULink>
          </div>
          <p v-if="!data.parent && !data.children.length" class="text-muted">No parent or children.</p>
        </section>

        <section v-if="data.timers.length" class="space-y-2">
          <h3 class="font-semibold text-highlighted">Timers</h3>
          <div v-for="t in data.timers" :key="t.id" class="flex items-center gap-2">
            <UIcon name="i-lucide-timer" class="size-4 shrink-0 text-muted" />
            <span class="min-w-0 flex-1 truncate">{{ t.prompt ?? "timer" }}</span>
            <TimeAgo :at="t.fire_at" class="text-xs text-muted" />
          </div>
        </section>

        <section v-if="data.services.length" class="space-y-2">
          <h3 class="font-semibold text-highlighted">Services used</h3>
          <div class="flex flex-wrap gap-1.5">
            <UBadge v-for="s in data.services" :key="s" color="neutral" variant="outline" icon="i-lucide-server" :label="s" />
          </div>
        </section>
      </div>
    </template>
  </USlideover>
</template>
