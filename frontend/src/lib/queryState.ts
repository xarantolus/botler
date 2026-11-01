// Page state kept in the address (`?tab=agents`), so a reload or a shared link opens the same view.

import { computed, type WritableComputedRef } from "vue";
import { useRoute, useRouter } from "vue-router";

/** `?key=` as a string, `fallback` when absent; setting it to `fallback` drops it from the URL. */
export function useQueryParam(key: string, fallback: string): WritableComputedRef<string> {
  const route = useRoute();
  const router = useRouter();
  return computed({
    get: () => (typeof route.query[key] === "string" ? (route.query[key] as string) : fallback),
    set: (v) => void router.replace({ query: { ...route.query, [key]: v === fallback ? undefined : v } }),
  });
}

/** `?key=a,b` as a list (empty when absent). */
export function useQueryList(key: string): WritableComputedRef<string[]> {
  const value = useQueryParam(key, "");
  return computed({
    get: () => (value.value ? value.value.split(",") : []),
    set: (v) => (value.value = v.join(",")),
  });
}
