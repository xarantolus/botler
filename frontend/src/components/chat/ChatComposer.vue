<script setup lang="ts">
// Writing to a chat: Enter sends, Shift+Enter adds a line. While the agent works, Enter queues
// the message (the hub holds it until the turn ends; it can be taken back until then) and
// Ctrl/Cmd+Enter, or "Send now", interrupts the turn instead. A message to an archived chat
// restores it; one to a sleeping agent wakes it.
import { ref, useTemplateRef } from "vue";
import { call } from "../../api/client";
import type { SendMode } from "../../generated";
import { notifyError } from "../../notify";
import { refreshSession } from "../../store";

const props = defineProps<{ instance: string; archived: boolean; busy: boolean; hint?: string }>();
const emit = defineEmits<{ sent: [] }>();

const draft = ref("");
const sending = ref(false);
const prompt = useTemplateRef<{ textareaRef: HTMLTextAreaElement | null }>("prompt");

async function send(mode: SendMode) {
  const text = draft.value.trim();
  if (!text || sending.value) return;
  const to = props.instance;
  sending.value = true;
  try {
    await call("send_message", { body: { to, text, mode } });
    draft.value = "";
    emit("sent");
    await refreshSession(to);
  } catch (e) {
    notifyError("Could not send", e);
  } finally {
    sending.value = false;
  }
}

defineExpose({ focus: () => prompt.value?.textareaRef?.focus() });
</script>

<template>
  <UChatPrompt
    ref="prompt"
    v-model="draft"
    :placeholder="archived ? 'Message to restore this chat…' : busy ? 'Queue a message…' : 'Message…'"
    :maxrows="10"
    :autofocus="false"
    variant="subtle"
    :aria-label="`Message to ${instance}`"
    :ui="{ base: 'px-1' }"
    @submit="send('queue')"
    @keydown.enter.ctrl.exact.prevent="send('steer')"
    @keydown.enter.meta.exact.prevent="send('steer')"
  >
    <template #footer>
      <p class="flex min-w-0 flex-1 items-center gap-1 truncate px-1 text-xs text-dimmed">
        <span v-if="hint" class="truncate">{{ hint }}</span>
        <span v-else-if="busy" class="hidden items-center gap-1 sm:inline-flex">
          <UKbd value="enter" size="sm" /> to queue, <UKbd value="ctrl" size="sm" /><UKbd value="enter" size="sm" /> to interrupt
        </span>
        <span v-else class="hidden items-center gap-1 sm:inline-flex">
          <UKbd value="enter" size="sm" /> to send, <UKbd value="shift" size="sm" /><UKbd value="enter" size="sm" /> for a new line
        </span>
      </p>
      <UTooltip v-if="busy" text="Send now: interrupt the agent (Ctrl+Enter)">
        <UButton
          icon="i-lucide-zap"
          label="Send now"
          size="sm"
          color="neutral"
          variant="ghost"
          :disabled="!draft.trim() || sending"
          @click="send('steer')"
        />
      </UTooltip>
      <UChatPromptSubmit
        :loading="sending"
        :disabled="!draft.trim()"
        color="primary"
        :icon="busy ? 'i-lucide-list-plus' : undefined"
        :aria-label="busy ? 'Queue' : 'Send'"
      />
    </template>
  </UChatPrompt>
</template>
