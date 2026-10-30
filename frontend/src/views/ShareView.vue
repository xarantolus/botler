<script setup lang="ts">
// The PWA share target (`/share?title=&text=&url=`): what was shared becomes a message to the
// front door, then the chat opens. A GET navigation carries no token, so the app sends it.
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../api/client";
import { shareText } from "../lib/share";

const router = useRouter();
const error = ref<string | null>(null);

onMounted(async () => {
  const q = router.currentRoute.value.query;
  const one = (v: unknown) => (typeof v === "string" ? v : null);
  const text = shareText(one(q.title), one(q.text), one(q.url));
  if (!text) {
    await router.replace({ name: "home" });
    return;
  }
  try {
    await call("send_message", { body: { text } });
    await router.replace({ name: "home" });
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
});
</script>

<template>
  <div class="p-6">
    <UAlert v-if="error" color="error" variant="subtle" title="Could not share" :description="error" />
    <p v-else class="text-sm text-muted">Sending to botler…</p>
  </div>
</template>
