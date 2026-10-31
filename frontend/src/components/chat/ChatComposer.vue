<script setup lang="ts">
// Writing to a chat: Enter sends, Shift+Enter adds a line. While the agent works, a sent message
// reaches it once its running tool calls are done, before its next model call (pi's steer: a
// running tool is never stopped); Ctrl/Cmd+Enter, or "When done", queues it instead (the hub
// holds it until the agent is done; it can be taken back until then). A message to an archived chat
// restores it; one to a sleeping agent wakes it. Files attach with the paperclip, by dropping
// them on the composer or by pasting (images): each uploads right away and goes with the message.
import { computed, reactive, ref, useTemplateRef, watch } from "vue";
import { call, upload } from "../../api/client";
import type { SendMode } from "../../generated";
import { bytes } from "../../lib/time";
import { notifyError } from "../../notify";
import { shell } from "../../shell";
import { refreshSession } from "../../store";

const props = defineProps<{ instance: string; archived: boolean; busy: boolean; hint?: string }>();
const emit = defineEmits<{ sent: [] }>();

const draft = ref("");
const sending = ref(false);
const prompt = useTemplateRef<{ textareaRef: HTMLTextAreaElement | null }>("prompt");
const picker = useTemplateRef<HTMLInputElement>("picker");

interface Attachment {
  key: number;
  name: string;
  size: number;
  /** 0..1 while uploading. */
  progress: number;
  /** The upload (`<id>/<name>`), once stored. */
  file: string | null;
  error: string | null;
}
const attachments = reactive<Attachment[]>([]);
let nextKey = 0;
const dragging = ref(false);
const uploading = computed(() => attachments.some((a) => !a.file && !a.error));
const failed = computed(() => attachments.some((a) => a.error));
const canSend = computed(() => (!!draft.value.trim() || attachments.length > 0) && !uploading.value && !failed.value);

/** A shared text (the share target) waiting for this chat's composer. */
watch(
  () => [shell.draft, props.instance] as const,
  ([d]) => {
    if (d && d.to === props.instance) {
      draft.value = d.text;
      shell.draft = null;
      prompt.value?.textareaRef?.focus();
    }
  },
  { immediate: true },
);

function attach(files: Iterable<File>) {
  for (const f of files) {
    const a = reactive<Attachment>({ key: nextKey++, name: f.name, size: f.size, progress: 0, file: null, error: null });
    attachments.push(a);
    upload(f, (p) => (a.progress = p)).then(
      (u) => (a.file = u.file),
      (e: unknown) => (a.error = e instanceof Error ? e.message : String(e)),
    );
  }
}
function remove(a: Attachment) {
  attachments.splice(attachments.indexOf(a), 1);
}
function onPicked(e: Event) {
  const input = e.target as HTMLInputElement;
  if (input.files) attach(input.files);
  input.value = "";
}
function onDrop(e: DragEvent) {
  dragging.value = false;
  if (e.dataTransfer?.files.length) attach(e.dataTransfer.files);
}
/** Pasted images (screenshots) become attachments; pasted text stays text. */
function onPaste(e: ClipboardEvent) {
  const files = [...(e.clipboardData?.files ?? [])];
  if (!files.length) return;
  e.preventDefault();
  const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  attach(files.map((f) => (f.name && f.name !== "image.png" ? f : new File([f], `pasted-${stamp}.${f.type.split("/")[1] ?? "png"}`, { type: f.type }))));
}

async function send(mode: SendMode) {
  if (!canSend.value || sending.value) return;
  const text = draft.value.trim();
  const files = attachments.flatMap((a) => (a.file ? [a.file] : []));
  const to = props.instance;
  sending.value = true;
  try {
    await call("send_message", { body: { to, text, mode, files: files.length ? files : undefined } });
    draft.value = "";
    attachments.splice(0);
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
  <div
    class="relative"
    @dragenter.prevent="dragging = true"
    @dragover.prevent
    @dragleave.self="dragging = false"
    @drop.prevent="onDrop"
  >
    <div
      v-if="dragging"
      class="pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-lg border-2 border-dashed border-primary bg-primary/10 text-sm font-medium text-primary"
    >
      Drop to attach
    </div>
    <UChatPrompt
      ref="prompt"
      v-model="draft"
      :placeholder="archived ? 'Message to restore this chat…' : busy ? 'Message… (read after the running step)' : 'Message…'"
      :maxrows="10"
      :autofocus="false"
      variant="subtle"
      :aria-label="`Message to ${instance}`"
      :ui="{ base: 'px-1' }"
      @submit="send('steer')"
      @paste="onPaste"
      @keydown.enter.ctrl.exact.prevent="send('queue')"
      @keydown.enter.meta.exact.prevent="send('queue')"
    >
      <template v-if="attachments.length" #header>
        <ul class="flex flex-wrap gap-1.5 px-1 pt-1" aria-label="Attached files">
          <li
            v-for="a in attachments"
            :key="a.key"
            class="flex max-w-60 items-center gap-1.5 rounded-md border px-2 py-1 text-xs"
            :class="a.error ? 'border-error/40 bg-error/5 text-error' : 'border-default bg-default'"
            :title="a.error ?? a.name"
          >
            <UIcon :name="a.error ? 'i-lucide-circle-alert' : 'i-lucide-paperclip'" class="size-3.5 shrink-0" />
            <span class="truncate">{{ a.name }}</span>
            <span v-if="!a.file && !a.error" class="w-10 shrink-0"><UProgress :model-value="a.progress * 100" size="xs" /></span>
            <span v-else-if="a.file" class="shrink-0 text-dimmed">{{ bytes(a.size) }}</span>
            <UButton icon="i-lucide-x" size="xs" color="neutral" variant="link" class="p-0" :aria-label="`Remove ${a.name}`" @click="remove(a)" />
          </li>
        </ul>
      </template>
      <template #footer>
        <input ref="picker" type="file" multiple class="hidden" @change="onPicked" />
        <UTooltip text="Attach files (or drop them here, or paste an image)">
          <UButton icon="i-lucide-paperclip" size="sm" color="neutral" variant="ghost" aria-label="Attach files" @click="picker?.click()" />
        </UTooltip>
        <p class="flex min-w-0 flex-1 items-center gap-1 truncate px-1 text-xs text-dimmed">
          <span v-if="failed" class="truncate text-error">Remove the files that failed to upload to send.</span>
          <span v-else-if="hint" class="truncate">{{ hint }}</span>
          <span v-else-if="busy" class="hidden items-center gap-1 sm:inline-flex">
            <UKbd value="enter" size="sm" /> to send after the running step, <UKbd value="ctrl" size="sm" /><UKbd value="enter" size="sm" /> when done
          </span>
          <span v-else class="hidden items-center gap-1 sm:inline-flex">
            <UKbd value="enter" size="sm" /> to send, <UKbd value="shift" size="sm" /><UKbd value="enter" size="sm" /> for a new line
          </span>
        </p>
        <UTooltip v-if="busy" text="Queue it until the agent is done; it can be taken back until then (Ctrl+Enter)">
          <UButton
            icon="i-lucide-list-plus"
            label="When done"
            size="sm"
            color="neutral"
            variant="ghost"
            :disabled="!canSend || sending"
            @click="send('queue')"
          />
        </UTooltip>
        <UChatPromptSubmit :loading="sending || uploading" :disabled="!canSend" color="primary" aria-label="Send" />
      </template>
    </UChatPrompt>
  </div>
</template>
