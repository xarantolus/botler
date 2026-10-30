<script setup lang="ts">
// Start a chat with a first message (and optionally another agent definition).
import type { FormError, FormSubmitEvent } from "@nuxt/ui";
import { reactive, ref } from "vue";
import { useRouter } from "vue-router";
import { call } from "../../api/client";
import { notifyError } from "../../notify";
import { shell } from "../../shell";
import { refreshInstances } from "../../store";

const router = useRouter();
const form = reactive({ text: "", definition: "" });
const busy = ref(false);

function validate(s: typeof form): FormError[] {
  return s.text.trim() ? [] : [{ name: "text", message: "Say what the chat is about." }];
}

async function create(e: FormSubmitEvent<typeof form>) {
  busy.value = true;
  try {
    const r = await call("new_chat", {
      body: { text: e.data.text.trim(), definition: e.data.definition.trim() || undefined },
    });
    shell.newChat = false;
    form.text = "";
    form.definition = "";
    await refreshInstances();
    await router.push(`/chat/${encodeURIComponent(r.instance)}`);
  } catch (err) {
    notifyError("Could not start the chat", err);
  } finally {
    busy.value = false;
  }
}

function onKey(e: KeyboardEvent) {
  // Enter starts it, as in the chat composer; Shift+Enter adds a line.
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    (e.target as HTMLElement).closest("form")?.requestSubmit();
  }
}
</script>

<template>
  <UModal
    v-model:open="shell.newChat"
    title="New chat"
    description="A new agent starts with your first message. Botler can also start chats for you."
  >
    <template #body>
      <UForm id="new-chat" :state="form" :validate="validate" class="space-y-4" @submit="create">
        <UFormField label="First message" name="text" required>
          <UTextarea
            v-model="form.text"
            placeholder="What is this chat about?"
            autoresize
            :rows="3"
            :maxrows="10"
            autofocus
            class="w-full"
            @keydown="onKey"
          />
        </UFormField>
        <UFormField label="Agent definition" name="definition" hint="Optional" help="Leave empty for the default, chat.">
          <UInput v-model="form.definition" placeholder="chat" class="w-full" icon="i-lucide-file-code-2" />
        </UFormField>
      </UForm>
    </template>
    <template #footer>
      <div class="flex w-full justify-end gap-2">
        <UButton color="neutral" variant="ghost" label="Cancel" @click="shell.newChat = false" />
        <UButton type="submit" form="new-chat" icon="i-lucide-send-horizontal" label="Start chat" :loading="busy" />
      </div>
    </template>
  </UModal>
</template>
