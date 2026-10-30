<script setup lang="ts">
// Everything waiting for the user (questions, secrets, approvals), and what was decided lately.
import { computed, onMounted, ref } from "vue";
import DialogCard from "../components/common/DialogCard.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import { refreshDialogs, state } from "../store";

const loading = ref(false);
async function load() {
  loading.value = true;
  await refreshDialogs();
  loading.value = false;
}
onMounted(load);

const open = computed(() => state.dialogs?.open ?? []);
const closed = computed(() => (state.dialogs?.recent ?? []).filter((d) => d.status !== "open").slice(0, 30));
const tab = ref<string | number>("open");
const tabs = computed(() => [
  { label: "Waiting", value: "open", icon: "i-lucide-inbox", badge: open.value.length ? String(open.value.length) : undefined },
  { label: "Earlier", value: "closed", icon: "i-lucide-history" },
]);
</script>

<template>
  <ViewPanel id="requests" title="Requests" icon="i-lucide-inbox">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <div class="mx-auto w-full max-w-3xl space-y-4">
      <UTabs v-model="tab" :items="tabs" :content="false" variant="link" class="w-full" />
      <div v-if="!state.dialogs" class="space-y-4">
        <USkeleton v-for="n in 2" :key="n" class="h-36 w-full rounded-lg" />
      </div>
      <template v-else-if="tab === 'open'">
        <UEmpty
          v-if="!open.length"
          icon="i-lucide-circle-check-big"
          title="Nothing is waiting for you"
          description="Questions, secret requests and approvals from your agents show up here and in their chats."
          variant="naked"
          class="py-12"
        />
        <DialogCard v-for="d in open" :key="d.id" :dialog="d" show-chat />
      </template>
      <template v-else>
        <UEmpty v-if="!closed.length" icon="i-lucide-history" title="Nothing decided lately" variant="naked" class="py-12" />
        <DialogCard v-for="d in closed" :key="d.id" :dialog="d" show-chat />
      </template>
    </div>
  </ViewPanel>
</template>
