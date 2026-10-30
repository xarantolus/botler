<script setup lang="ts">
// Writing to a chat: Enter sends, Shift+Enter adds a line. A message to an archived chat
// restores it; one to a sleeping agent wakes it.
import { ref, useTemplateRef } from "vue";
import { call } from "../../api/client";
import { notifyError } from "../../notify";
import { queueSent, refreshSession } from "../../store";

const props = defineProps<{ instance: string; archived: boolean; hint?: string }>();
const emit = defineEmits<{ sent: [] }>();

const draft = ref("");
const sending = ref(false);
const prompt = useTemplateRef<{ textareaRef: HTMLTextAreaElement | null }>("prompt");

async function send() {
  const text = draft.value.trim();
  if (!text || sending.value) return;
  const to = props.instance;
  sending.value = true;
  try {
    await call("send_message", { body: { to, text } });
    queueSent(to, text);
    draft.value = "";
    emit("sent");
    window.setTimeout(() => void refreshSession(to), 400);
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
    :placeholder="archived ? 'Message to restore this chat…' : 'Message…'"
    :maxrows="10"
    :autofocus="false"
    variant="subtle"
    :aria-label="`Message to ${instance}`"
    :ui="{ base: 'px-1' }"
    @submit="send"
  >
    <template #footer>
      <p class="flex min-w-0 flex-1 items-center gap-1 truncate px-1 text-xs text-dimmed">
        <span v-if="hint" class="truncate">{{ hint }}</span>
        <span v-else class="hidden items-center gap-1 sm:inline-flex">
          <UKbd value="enter" size="sm" /> to send, <UKbd value="shift" size="sm" /><UKbd value="enter" size="sm" /> for a new line
        </span>
      </p>
      <UChatPromptSubmit :loading="sending" :disabled="!draft.trim()" color="primary" aria-label="Send" />
    </template>
  </UChatPrompt>
</template>
