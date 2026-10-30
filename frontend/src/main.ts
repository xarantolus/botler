import ui from "@nuxt/ui/vue-plugin";
import { createApp } from "vue";
import { createRouter, createWebHistory } from "vue-router";
import App from "./App.vue";
import ChatView from "./views/ChatView.vue";
import EventsView from "./views/EventsView.vue";
import IncidentsView from "./views/IncidentsView.vue";
import MemoryView from "./views/MemoryView.vue";
import ProposalsView from "./views/ProposalsView.vue";
import RequestsView from "./views/RequestsView.vue";
import SearchView from "./views/SearchView.vue";
import SecretsView from "./views/SecretsView.vue";
import SystemView from "./views/SystemView.vue";
import "./style.css";

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/", name: "home", component: ChatView },
    { path: "/chat/:instance", name: "chat", component: ChatView, props: true },
    { path: "/events", name: "events", component: EventsView },
    { path: "/requests", name: "requests", component: RequestsView },
    { path: "/system", name: "system", component: SystemView },
    // The run list moved into System's History tab; old links keep their filters.
    { path: "/runs", redirect: (to) => ({ name: "system", query: { ...to.query, tab: "history" } }) },
    { path: "/incidents", name: "incidents", component: IncidentsView },
    { path: "/proposals", name: "proposals", component: ProposalsView },
    { path: "/secrets", name: "secrets", component: SecretsView },
    { path: "/search", name: "search", component: SearchView },
    { path: "/memory", name: "memory", component: MemoryView },
    { path: "/:rest(.*)*", redirect: "/" },
  ],
});

createApp(App).use(router).use(ui).mount("#app");
