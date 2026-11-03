<script setup lang="ts">
// Signing a browser in: a one-time code (from a signed-in device under Settings → Devices, or
// `botler login-code` on the hub) or a user token (`botler token`). The hub then keeps this
// browser signed in with a cookie scripts cannot read. A link `/#code=<code>` (the hub logs one on
// its first start) signs in right away; the fragment never reaches a server.
import type { AuthFormField, FormError, FormSubmitEvent } from "@nuxt/ui";
import { onMounted, ref } from "vue";
import { errorText } from "../../notify";
import { login } from "../../store";

type Fields = { secret: string; name: string };

const fields: AuthFormField[] = [
  {
    name: "secret",
    type: "password",
    label: "Sign-in code or token",
    placeholder: "e.g. K7QM2XPA",
    autocomplete: "one-time-code",
    required: true,
  },
  { name: "name", type: "text", label: "Name this device", placeholder: "e.g. Phone", autocomplete: "off" },
];
const error = ref("");
const busy = ref(false);

function validate(s: Partial<Fields>): FormError[] {
  return s.secret?.trim() ? [] : [{ name: "secret", message: "Enter a sign-in code or a user token." }];
}

/** A short default name: the browser and system the user agent names. */
function deviceName(): string {
  const ua = navigator.userAgent;
  const browser = /Firefox\//.test(ua) ? "Firefox" : /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "Browser";
  const os = /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "";
  return os ? `${browser} on ${os}` : browser;
}

async function signIn(secret: string, name: string) {
  busy.value = true;
  error.value = "";
  try {
    await login(secret, name);
  } catch (err) {
    error.value = errorText(err);
  } finally {
    busy.value = false;
  }
}

function submit(e: FormSubmitEvent<Fields>) {
  return signIn(e.data.secret.trim(), e.data.name?.trim() || deviceName());
}

onMounted(() => {
  const code = new URLSearchParams(location.hash.slice(1)).get("code");
  if (!code) return;
  history.replaceState(history.state, "", location.pathname + location.search);
  void signIn(code, deviceName());
});
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
        :submit="{ label: 'Sign in', block: true }"
        @submit="submit"
      >
        <template #description>
          Get a code on a signed-in device under <strong>Settings → Devices</strong>, or run
          <code class="font-mono text-highlighted">botler login-code</code> on the hub.
        </template>
        <template #validation>
          <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-circle-alert" :title="error" />
        </template>
      </UAuthForm>
    </UPageCard>
  </div>
</template>
