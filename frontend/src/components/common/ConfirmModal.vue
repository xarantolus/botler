<script setup lang="ts">
// A confirmation, opened through useConfirm(). Emits `close` with the result (false when
// cancelled); the overlay provider also resolves it (undefined) when it is dismissed.
import { ref } from "vue";

const props = defineProps<{
  title: string;
  description?: string;
  confirmLabel: string;
  color?: "primary" | "warning" | "error";
  /** Ask for a reason; optional unless `required`. */
  reason?: { label: string; placeholder?: string; required?: boolean };
  /** A checkbox, returned as `checked`. */
  check?: { label: string };
}>();
const emit = defineEmits<{ close: [result: { reason: string; checked: boolean } | false] }>();
const reason = ref("");
const checked = ref(false);
</script>

<template>
  <UModal :title="props.title" :description="props.description" :ui="{ footer: 'justify-end' }">
    <template v-if="props.reason || props.check" #body>
      <UCheckbox v-if="props.check" v-model="checked" :label="props.check.label" class="mb-4" />
      <UFormField
        v-if="props.reason" :label="props.reason.label" :hint="props.reason.required ? undefined : 'Optional'" :required="props.reason.required">
        <UTextarea
          v-model="reason"
          :placeholder="props.reason.placeholder"
          autoresize
          :rows="2"
          :maxrows="6"
          class="w-full"
        />
      </UFormField>
    </template>
    <template #footer>
      <UButton color="neutral" variant="ghost" label="Cancel" @click="emit('close', false)" />
      <UButton
        :color="props.color ?? 'primary'"
        :label="props.confirmLabel"
        :disabled="props.reason?.required && !reason.trim()"
        autofocus
        @click="emit('close', { reason: reason.trim(), checked })"
      />
    </template>
  </UModal>
</template>
