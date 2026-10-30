import { createApp } from "vue";
import { createRouter, createWebHistory } from "vue-router";
import App from "./App.vue";
import ChatView from "./views/ChatView.vue";
import EventsView from "./views/EventsView.vue";
import IncidentsView from "./views/IncidentsView.vue";
import MemoryView from "./views/MemoryView.vue";
import ProposalsView from "./views/ProposalsView.vue";
import RequestsView from "./views/RequestsView.vue";
import RunsView from "./views/RunsView.vue";
import SecretsView from "./views/SecretsView.vue";
import "./style.css";

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/", name: "home", component: ChatView },
    { path: "/chat/:instance", name: "chat", component: ChatView, props: true },
    { path: "/events", name: "events", component: EventsView },
    { path: "/requests", name: "requests", component: RequestsView },
    { path: "/runs", name: "runs", component: RunsView },
    { path: "/incidents", name: "incidents", component: IncidentsView },
    { path: "/proposals", name: "proposals", component: ProposalsView },
    { path: "/secrets", name: "secrets", component: SecretsView },
    { path: "/memory", name: "memory", component: MemoryView },
    { path: "/:rest(.*)*", redirect: "/" },
  ],
});

createApp(App).use(router).mount("#app");
