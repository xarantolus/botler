<script setup lang="ts">
// Signed-in devices: sign another one in with a one-time code, or sign one out.
import { onMounted, shallowRef } from "vue";
import { call } from "../../api/client";
import type { DeviceView, LoginCode } from "../../generated";
import { notifyError } from "../../notify";
import TimeAgo from "../common/TimeAgo.vue";

const devices = shallowRef<DeviceView[] | null>(null);
const code = shallowRef<LoginCode | null>(null);

async function load() {
  try {
    devices.value = await call("devices", {});
  } catch (e) {
    notifyError("Could not load the devices", e);
  }
}
onMounted(load);

async function newCode() {
  try {
    code.value = await call("login_code", {});
  } catch (e) {
    notifyError("Could not create a code", e);
  }
}

async function signOut(id: string) {
  try {
    await call("revoke_device", { params: { id } });
    await load();
  } catch (e) {
    notifyError("Could not sign the device out", e);
  }
}
</script>

<template>
  <UCard>
    <template #header>
      <div class="flex flex-wrap items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="font-semibold text-highlighted">Devices</p>
          <p class="text-sm text-muted">Browsers signed in to botler. Each stays signed in until you sign it out here.</p>
        </div>
        <UButton icon="i-lucide-smartphone" label="Sign in another device" @click="newCode" />
      </div>
    </template>
    <UAlert v-if="code" class="mb-4" color="info" variant="subtle" icon="i-lucide-key-round">
      <template #title>
        Enter <code class="font-mono text-base tracking-widest">{{ code.code }}</code> on the other device
      </template>
      <template #description>Works once, until <TimeAgo :at="code.expires_at" />.</template>
    </UAlert>
    <p v-if="devices && !devices.length" class="text-sm text-muted">
      No device signed in with a code yet; this browser uses a token.
    </p>
    <ul class="divide-y divide-default">
      <li v-for="d in devices ?? []" :key="d.id" class="flex flex-wrap items-center gap-2 py-2 text-sm">
        <UIcon name="i-lucide-monitor-smartphone" class="size-4 text-muted" />
        <span class="font-medium text-highlighted">{{ d.name }}</span>
        <UBadge v-if="d.current" color="primary" variant="subtle" size="sm" label="this device" />
        <span class="text-muted">signed in <TimeAgo :at="d.created_at" />, seen <TimeAgo :at="d.last_seen_at" /></span>
        <UButton class="ms-auto" size="xs" color="error" variant="soft" label="Sign out" @click="signOut(d.id)" />
      </li>
    </ul>
  </UCard>
</template>
