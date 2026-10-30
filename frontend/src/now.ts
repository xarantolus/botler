// One shared clock for relative times, ticking while anything shows one.

import { onScopeDispose, ref, type Ref } from "vue";

const now = ref(Date.now());
let users = 0;
let timer: number | undefined;

/** The current time (ms), updated every `every` ms while any caller's scope is alive. */
export function useNow(every = 30_000): Readonly<Ref<number>> {
  if (users++ === 0) timer = window.setInterval(() => (now.value = Date.now()), every);
  onScopeDispose(() => {
    if (--users === 0) window.clearInterval(timer);
  });
  return now;
}
