<script setup lang="ts">
// A section of the app: the dashboard panel with its title bar (and an optional toolbar).
defineProps<{ id: string; title: string; icon: string; bodyClass?: string }>();
</script>

<template>
  <UDashboardPanel :id="id" :ui="{ body: bodyClass }">
    <template #header>
      <UDashboardNavbar :title="title" :icon="icon" :ui="{ right: 'gap-2' }">
        <template #leading><UDashboardSidebarCollapse /></template>
        <template #right><slot name="right" /></template>
      </UDashboardNavbar>
      <UDashboardToolbar v-if="$slots.toolbar">
        <slot name="toolbar" />
      </UDashboardToolbar>
    </template>
    <template #body>
      <slot />
    </template>
  </UDashboardPanel>
</template>
