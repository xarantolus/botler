<script setup lang="ts">
// Channels: outside accounts (Telegram, email, ...) that count as you. Pair one with a one-time
// code sent from that account; revoke it here. Senders that are not paired are only counted.
import { onMounted, ref, shallowRef } from "vue";
import { call } from "../../api/client";
import type { ChannelsView, PairingCode } from "../../generated";
import { notifyError } from "../../notify";
import TimeAgo from "../common/TimeAgo.vue";

const view = shallowRef<ChannelsView | null>(null);
const code = shallowRef<PairingCode | null>(null);
const target = ref("");
const busy = ref(false);

async function load() {
  try {
    view.value = await call("channels", {});
  } catch (e) {
    notifyError("Could not load the channels", e);
  }
}
onMounted(load);

async function pair() {
  busy.value = true;
  try {
    code.value = await call("channel_code", { body: { target: target.value.trim() || undefined } });
  } catch (e) {
    notifyError("Could not create a code", e);
  } finally {
    busy.value = false;
  }
}

async function revoke(id: string) {
  try {
    await call("revoke_binding", { params: { id } });
    await load();
  } catch (e) {
    notifyError("Could not revoke", e);
  }
}
</script>

<template>
  <UCard>
    <template #header>
      <p class="font-semibold text-highlighted">Channels</p>
      <p class="text-sm text-muted">
        Talk to Botler from outside the web app. A channel is a service agents build (ask Botler for e.g. a Telegram
        channel); every change to one asks you. Approvals and secrets stay in the web app.
      </p>
    </template>
    <p v-if="view && !view.channels.length" class="text-sm text-muted">No channel services yet.</p>
    <div v-else class="space-y-4">
      <p class="text-sm">
        Channels: <code v-for="c in view?.channels ?? []" :key="c" class="me-2 font-mono">{{ c }}</code>
      </p>
      <div class="flex flex-wrap items-end gap-2">
        <UFormField label="Messages go to" hint="optional" description="A chat name; Botler by default.">
          <UInput v-model="target" placeholder="botler" class="w-48" />
        </UFormField>
        <UButton icon="i-lucide-link" label="Pair an account" :loading="busy" @click="pair" />
      </div>
      <UAlert v-if="code" color="info" variant="subtle" icon="i-lucide-key-round">
        <template #title>
          Send <code class="font-mono text-base">pair {{ code.code }}</code> from the account you want to pair
        </template>
        <template #description>
          Goes to {{ code.target }}. Works once, until <TimeAgo :at="code.expires_at" />.
          <UButton size="xs" variant="link" label="Done, refresh" @click="(code = null), load()" />
        </template>
      </UAlert>
    </div>
  </UCard>
  <UCard v-if="view?.bindings.length">
    <template #header><p class="font-semibold text-highlighted">Paired accounts</p></template>
    <ul class="divide-y divide-default">
      <li v-for="b in view.bindings" :key="b.id" class="flex flex-wrap items-center gap-2 py-2 text-sm">
        <UBadge color="neutral" variant="subtle" :label="b.channel" />
        <code class="min-w-0 truncate font-mono">{{ b.sender }}</code>
        <span class="text-muted">→ {{ b.target }} · <TimeAgo :at="b.created_at" /></span>
        <UButton class="ms-auto" size="xs" color="error" variant="soft" label="Revoke" @click="revoke(b.id)" />
      </li>
    </ul>
  </UCard>
  <UCard v-if="view?.strangers.length">
    <template #header>
      <p class="font-semibold text-highlighted">Unpaired senders</p>
      <p class="text-sm text-muted">Their messages were dropped; nothing was sent back.</p>
    </template>
    <ul class="divide-y divide-default">
      <li v-for="s in view.strangers" :key="`${s.channel}/${s.sender}`" class="flex flex-wrap items-center gap-2 py-2 text-sm">
        <UBadge color="neutral" variant="subtle" :label="s.channel" />
        <code class="font-mono">{{ s.sender }}</code>
        <span class="text-muted">{{ s.count }} message(s), last <TimeAgo :at="s.last_at" /></span>
      </li>
    </ul>
  </UCard>
</template>
