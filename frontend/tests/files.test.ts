// @vitest-environment happy-dom
// Files with a message or a tool call, rendered by type with the real kit.

import { flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import MessageFiles from "../src/components/chat/MessageFiles.vue";
import { formatSize, presentation, safeType, type FileRef } from "../src/lib/attachments";
import { mountInApp } from "./mount";

const file = (name: string, mime: string, size: number | null = 1000): FileRef => ({ name, mime, size, path: `/api/v1/uploads/id/${name}` });
/** The blob types the component made URLs of, in order. */
let blobs: string[] = [];

beforeEach(() => {
  blobs = [];
  vi.stubGlobal("fetch", vi.fn(async () => new Response(new Blob(["x"]))));
  vi.spyOn(URL, "createObjectURL").mockImplementation((b) => {
    blobs.push((b as Blob).type);
    return `blob:test/${blobs.length}`;
  });
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("presentation", () => {
  it("shows media inline, opens PDFs and text, downloads the rest", () => {
    expect(["image/png", "video/mp4", "audio/mpeg", "application/pdf", "text/plain; charset=utf-8", "image/svg+xml", "text/html"].map(presentation)).toEqual([
      "image",
      "video",
      "audio",
      "open",
      "open",
      "download",
      "download",
    ]);
    expect(safeType("text/html")).toBe("application/octet-stream");
    expect(safeType("application/pdf")).toBe("application/pdf");
    expect([500, 2048, 5 * 1024 * 1024].map(formatSize)).toEqual(["500 B", "2.0 KB", "5.0 MB"]);
  });
});

describe("MessageFiles", () => {
  it("renders images, players, tabs and downloads by type", async () => {
    const w = await mountInApp(MessageFiles, {
      files: [
        file("shot.png", "image/png"),
        file("demo.mp4", "video/mp4"),
        file("voice.ogg", "audio/ogg"),
        file("report.pdf", "application/pdf"),
        file("page.html", "application/octet-stream", 2048),
      ],
    });
    await flushPromises();
    expect(w.find("img").attributes("src")).toMatch(/^blob:/);
    // An image opens the gallery.
    await w.find("a.cursor-zoom-in").trigger("click");
    await vi.waitFor(() => expect(document.querySelector(".pswp")).not.toBeNull(), { timeout: 5000 });
    const video = w.find("video");
    expect([video.attributes("controls"), video.attributes("preload")]).toEqual(["", "metadata"]);
    expect(w.find("audio").attributes("controls")).toBe("");
    const buttons = w.findAll("button").map((b) => b.text());
    expect(buttons).toEqual(["report.pdf · 1000 B", "page.html · 2.0 KB"]);
    // Nothing that can run script becomes a blob URL of its own type.
    expect(blobs).toEqual(["image/png", "video/mp4", "audio/ogg"]);
  });

  it("opens a PDF in a new tab and downloads anything else", async () => {
    const tab = { location: { href: "" }, close: vi.fn() };
    const open = vi.spyOn(window, "open").mockReturnValue(tab as unknown as Window);
    const clicked: { href: string; download: string }[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      clicked.push({ href: this.href, download: this.download });
    });
    const w = await mountInApp(MessageFiles, {
      files: [file("report.pdf", "application/pdf"), file("page.html", "application/octet-stream")],
    });
    const [pdf, html] = w.findAll("button");
    await pdf!.trigger("click");
    await flushPromises();
    expect(open).toHaveBeenCalledWith("about:blank", "_blank");
    expect(tab.location.href).toMatch(/^blob:/);
    await html!.trigger("click");
    await flushPromises();
    expect(clicked).toEqual([{ href: expect.stringMatching(/^blob:/), download: "page.html" }]);
    expect(blobs).toEqual(["application/pdf", "application/octet-stream"]);
  });

  it("loads a large video only when asked", async () => {
    const w = await mountInApp(MessageFiles, { files: [file("long.webm", "video/webm", 300 * 1024 * 1024)], align: "start" });
    await flushPromises();
    expect(w.find("video").exists()).toBe(false);
    expect(fetch).not.toHaveBeenCalled();
    await w.find("button").trigger("click");
    await flushPromises();
    expect(w.find("video").attributes("src")).toMatch(/^blob:/);
  });
});
