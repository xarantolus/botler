<script setup lang="ts">
// Signing in with a user token (from `botler token`).
import type { AuthFormField, FormError, FormSubmitEvent } from "@nuxt/ui";
import { ref } from "vue";
import { errorText } from "../../notify";
import { login } from "../../store";

const fields: AuthFormField[] = [
  {
    name: "token",
    type: "password",
    label: "User token",
    placeholder: "Paste your token",
    autocomplete: "off",
    required: true,
  },
];
const error = ref("");
const busy = ref(false);

function validate(s: Partial<{ token: string }>): FormError[] {
  return s.token?.trim() ? [] : [{ name: "token", message: "Paste the token from `botler token`." }];
}

async function submit(e: FormSubmitEvent<{ token: string }>) {
  busy.value = true;
  error.value = "";
  try {
    await login(e.data.token.trim());
  } catch (err) {
    error.value = errorText(err);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="flex min-h-svh items-center justify-center bg-default px-4 py-12">
    <UPageCard class="w-full max-w-sm" variant="subtle">
      <UAuthForm
        :fields="fields"
        :validate="validate"
        :loading="busy"
        title="Sign in to botler"
        icon="i-lucide-bot"
        :submit="{ label: 'Open', block: true }"
        @submit="submit"
      >
        <template #description>
          Paste your user token. <code class="font-mono text-highlighted">botler token</code> prints it on the hub.
        </template>
        <template #validation>
          <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-circle-alert" :title="error" />
        </template>
      </UAuthForm>
    </UPageCard>
  </div>
</template>
