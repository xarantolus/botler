<script setup lang="ts">
// The PWA share target (`/share?title=&text=&url=`): what was shared goes into the composer of
// Botler, or of a chat you pick, so you can add context before sending. Nothing is sent here.
import { computed, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { chatTitle } from "../lib/events";
import { shareText } from "../lib/share";
import ViewPanel from "../components/common/ViewPanel.vue";
import { shell } from "../shell";
import { frontDoorName, state } from "../store";

const router = useRouter();
const one = (v: unknown) => (typeof v === "string" ? v : null);
const q = router.currentRoute.value.query;
const text = shareText(one(q.title), one(q.text), one(q.url));

const chats = computed(() =>
  state.instances
    .filter((i) => i.state !== "archived")
    .map((i) => ({ label: chatTitle(i, frontDoorName.value), value: i.name })),
);
const to = ref<string | undefined>(frontDoorName.value ?? undefined);
watch(frontDoorName, (d) => (to.value ??= d ?? undefined));

function open() {
  if (!to.value || !text) return;
  shell.draft = { to: to.value, text };
  void router.replace(`/chat/${encodeURIComponent(to.value)}`);
}
</script>

<template>
  <ViewPanel id="share" title="Share with botler" icon="i-lucide-share-2">
    <UCard class="mx-auto w-full max-w-lg" :ui="{ body: 'space-y-4' }">
      <UAlert v-if="!text" color="warning" variant="subtle" icon="i-lucide-circle-alert" title="Nothing was shared" />
      <template v-else>
        <pre class="max-h-48 overflow-auto rounded-md border border-muted bg-muted px-3 py-2 text-sm whitespace-pre-wrap break-words">{{ text }}</pre>
        <UFormField label="Send to" description="It goes into that chat's message box, so you can add to it before sending.">
          <USelect v-model="to" :items="chats" value-key="value" class="w-full" aria-label="Chat to share with" />
        </UFormField>
      </template>
      <template #footer>
        <div class="flex justify-end gap-2">
          <UButton color="neutral" variant="ghost" label="Cancel" @click="router.replace({ name: 'home' })" />
          <UButton icon="i-lucide-message-square" label="Continue in chat" :disabled="!text || !to" @click="open" />
        </div>
      </template>
    </UCard>
  </ViewPanel>
</template>
