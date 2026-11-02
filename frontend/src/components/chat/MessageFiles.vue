<script setup lang="ts">
// Files with a message or a run of tool calls (lib/attachments.ts), each shown as what it is:
// images as thumbnails (opening a gallery of the list's images, PhotoSwipe), videos and audio with a player, PDFs and text opening in a new
// tab, anything else a chip that downloads it. The hub serves them only with the token, so they
// load as blobs; large media waits for a click.
import { onBeforeUnmount, reactive, watch } from "vue";
import { fetchFile } from "../../api/client";
import { formatSize, presentation, safeType, type FileRef } from "../../lib/attachments";
import { notifyError } from "../../notify";

const props = withDefaults(defineProps<{ files: FileRef[]; align?: "start" | "end" }>(), { align: "end" });
/** Video and audio up to this size load with the message; larger ones on a click. */
const AUTO_MEDIA = 25 * 1024 * 1024;
const ICONS = { video: "i-lucide-play", audio: "i-lucide-play", open: "i-lucide-external-link", download: "i-lucide-download" };
/** Blob URLs of the files loaded so far, by path. */
const urls = reactive<Record<string, string>>({});

const shows = (f: FileRef) => presentation(f.mime);
const label = (f: FileRef) => (f.size === null ? f.name : `${f.name} · ${formatSize(f.size)}`);

async function load(f: FileRef): Promise<string> {
  urls[f.path] ??= URL.createObjectURL(new Blob([await fetchFile(f.path)], { type: safeType(f.mime) }));
  return urls[f.path]!;
}

watch(
  () => props.files,
  async (files) => {
    for (const f of files) {
      const p = shows(f);
      const auto = p === "image" || ((p === "video" || p === "audio") && f.size !== null && f.size <= AUTO_MEDIA);
      if (!auto || urls[f.path]) continue;
      try {
        await load(f);
      } catch (e) {
        console.warn(`could not load ${f.name}`, e);
      }
    }
  },
  { immediate: true },
);
onBeforeUnmount(() => Object.values(urls).forEach((u) => URL.revokeObjectURL(u)));

/** Open a gallery of the list's loaded images at `f`, sized by their thumbnails. */
async function zoom(f: FileRef, e: MouseEvent) {
  const shown = props.files.filter((x) => shows(x) === "image" && urls[x.path]);
  const list = (e.currentTarget as HTMLElement).closest("ul");
  const thumbs = new Map([...(list?.querySelectorAll<HTMLImageElement>("img[data-path]") ?? [])].map((i) => [i.dataset.path, i]));
  const [{ default: PhotoSwipe }] = await Promise.all([import("photoswipe"), import("photoswipe/style.css")]);
  const dataSource = shown.map((x) => {
    const img = thumbs.get(x.path);
    return { src: urls[x.path], msrc: urls[x.path], alt: x.name, width: img?.naturalWidth || 1600, height: img?.naturalHeight || 1200 };
  });
  new PhotoSwipe({ dataSource, index: Math.max(0, shown.indexOf(f)), showHideAnimationType: "fade" }).init();
}

async function activate(f: FileRef) {
  const p = shows(f);
  // Opened before the fetch, while the click still allows a new tab.
  const tab = p === "open" ? window.open("about:blank", "_blank") : null;
  try {
    const url = await load(f);
    if (tab) tab.location.href = url;
    else if (p === "download") Object.assign(document.createElement("a"), { href: url, download: f.name }).click();
  } catch (e) {
    tab?.close();
    notifyError(`Could not load ${f.name}`, e);
  }
}
</script>

<template>
  <ul class="flex flex-wrap gap-1.5" :class="align === 'end' ? 'justify-end' : 'justify-start'" aria-label="Attached files">
    <li v-for="f in files" :key="f.path" :data-file="shows(f)">
      <a v-if="shows(f) === 'image' && urls[f.path]" :href="urls[f.path]" :title="f.name" class="block cursor-zoom-in" @click.prevent="zoom(f, $event).catch((e) => notifyError(`Could not open ${f.name}`, e))">
        <img :src="urls[f.path]" :data-path="f.path" :alt="f.name" class="max-h-40 max-w-56 rounded-md border border-default object-cover" />
      </a>
      <video
        v-else-if="shows(f) === 'video' && urls[f.path]"
        :src="urls[f.path]"
        :title="f.name"
        controls
        preload="metadata"
        class="max-h-80 max-w-full rounded-md border border-default"
      />
      <audio v-else-if="shows(f) === 'audio' && urls[f.path]" :src="urls[f.path]" :title="f.name" controls preload="metadata" />
      <UButton
        v-else
        :icon="shows(f) === 'image' ? 'i-lucide-image' : ICONS[shows(f) as keyof typeof ICONS]"
        :label="label(f)"
        size="xs"
        color="neutral"
        variant="outline"
        class="max-w-72"
        :ui="{ label: 'truncate' }"
        @click="activate(f)"
      />
    </li>
  </ul>
</template>
