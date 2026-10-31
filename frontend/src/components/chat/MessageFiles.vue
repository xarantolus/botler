<script setup lang="ts">
// The files sent with a message: images as thumbnails (opening full size), anything else as a
// chip that downloads it. The hub serves uploads only with the token, so they load as blobs.
import { onBeforeUnmount, reactive, watch } from "vue";
import { fetchUpload } from "../../api/client";
import { notifyError } from "../../notify";

const props = defineProps<{ files: string[] }>();
const IMAGE = /\.(png|jpe?g|gif|webp|avif)$/i;
const name = (f: string) => f.slice(f.indexOf("/") + 1);
const urls = reactive<Record<string, string>>({});

watch(
  () => props.files,
  async (files) => {
    for (const f of files.filter((f) => IMAGE.test(f) && !urls[f])) {
      try {
        urls[f] = URL.createObjectURL(await fetchUpload(f));
      } catch (e) {
        console.warn(`no thumbnail for ${f}`, e);
      }
    }
  },
  { immediate: true },
);
onBeforeUnmount(() => Object.values(urls).forEach((u) => URL.revokeObjectURL(u)));

async function download(f: string) {
  try {
    const url = urls[f] ?? URL.createObjectURL(await fetchUpload(f));
    const a = Object.assign(document.createElement("a"), { href: url, download: name(f) });
    a.click();
    if (!urls[f]) setTimeout(() => URL.revokeObjectURL(url), 10_000);
  } catch (e) {
    notifyError("Could not download", e);
  }
}
</script>

<template>
  <ul class="flex flex-wrap justify-end gap-1.5" aria-label="Attached files">
    <li v-for="f in files" :key="f">
      <a v-if="urls[f]" :href="urls[f]" target="_blank" rel="noopener noreferrer" :title="name(f)" class="block">
        <img :src="urls[f]" :alt="name(f)" class="max-h-40 max-w-56 rounded-md border border-default object-cover" />
      </a>
      <UButton
        v-else
        icon="i-lucide-paperclip"
        :label="name(f)"
        size="xs"
        color="neutral"
        variant="outline"
        class="max-w-60"
        :ui="{ label: 'truncate' }"
        @click="download(f)"
      />
    </li>
  </ul>
</template>
