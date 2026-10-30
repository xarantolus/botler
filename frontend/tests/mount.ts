// Mount a component the way the app runs it: inside <UApp> (tooltips, toasts, overlays), with
// Nuxt UI's plugin and a router.

import UApp from "@nuxt/ui/components/App.vue";
import ui from "@nuxt/ui/vue-plugin";
import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";
import { defineComponent, h, type Component } from "vue";
import { createMemoryHistory, createRouter } from "vue-router";

export async function mountInApp(component: Component, props: Record<string, unknown>): Promise<VueWrapper> {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/", component: { render: () => null } },
      { path: "/chat/:instance", component: { render: () => null } },
      { path: "/events", name: "events", component: { render: () => null } },
      { path: "/system", name: "system", component: { render: () => null } },
    ],
  });
  await router.push("/");
  const Root = defineComponent({ render: () => h(UApp, null, () => h(component, props)) });
  const wrapper = mount(Root, { global: { plugins: [router, ui] }, attachTo: document.body });
  await flushPromises();
  return wrapper;
}
