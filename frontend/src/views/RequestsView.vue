<script setup lang="ts">
import { NEmpty, NScrollbar, NSpace, NText } from "naive-ui";
import { computed, onMounted } from "vue";
import DialogCard from "../components/DialogCard.vue";
import { refreshDialogs, state } from "../store";

onMounted(refreshDialogs);

const open = computed(() => state.dialogs?.open ?? []);
const closed = computed(() => (state.dialogs?.recent ?? []).filter((d) => d.status !== "open").slice(0, 30));
</script>

<template>
  <NScrollbar style="height: 100%">
    <div style="max-width: 860px; margin: 0 auto; padding: 12px 16px">
      <NSpace vertical size="large">
        <NEmpty v-if="!open.length" description="Nothing is waiting for you." style="margin: 30px 0" />
        <DialogCard v-for="d in open" :key="d.id" :dialog="d" show-chat />
        <template v-if="closed.length">
          <NText depth="3" strong>Earlier</NText>
          <DialogCard v-for="d in closed" :key="d.id" :dialog="d" show-chat />
        </template>
      </NSpace>
    </div>
  </NScrollbar>
</template>
