// Files shown with a message or a tool call: uploads (the user's, and files agents send with
// `attach_files` / `send_message`) and images a tool returned (a screenshot), which stay in the
// session file and are served from there. Both load with the token, as blobs.

import type { ImageRef, SessionItem, Uploaded } from "../generated";

export interface FileRef {
  name: string;
  /** What the hub serves it as (`uploads::media_type`); `application/octet-stream` is a download. */
  mime: string;
  /** Bytes, when known (images in a session are not measured). */
  size: number | null;
  /** The hub path to fetch it from. */
  path: string;
}

/** How a file is shown: inline (image, player), opened in a new tab, or downloaded. */
export type Presentation = "image" | "video" | "audio" | "open" | "download";

const IMAGE = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif"];

export function presentation(mime: string): Presentation {
  const base = mime.split(";")[0]!.trim().toLowerCase();
  if (IMAGE.includes(base)) return "image";
  if (base.startsWith("video/")) return "video";
  if (base.startsWith("audio/")) return "audio";
  // A blob URL runs with the app's origin, so only types that cannot run script open in a tab.
  if (base === "application/pdf" || base === "text/plain") return "open";
  return "download";
}

/** The type a blob is given before the app makes a URL of it: never anything that runs script. */
export function safeType(mime: string): string {
  return presentation(mime) === "download" ? "application/octet-stream" : mime;
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let n = bytes / 1024;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

export function uploadRef(u: Uploaded): FileRef {
  return {
    name: u.file.slice(u.file.indexOf("/") + 1),
    mime: u.mime,
    size: u.size,
    path: `/api/v1/uploads/${u.file.split("/").map(encodeURIComponent).join("/")}`,
  };
}

/** Where a session's images are: the chat and the session file shown. */
export interface SessionSource {
  instance: string;
  file: string;
}

export function imageRef(source: SessionSource, entry: string, img: ImageRef): FileRef {
  const ext = img.mime.split("/")[1] ?? "png";
  const at = `${encodeURIComponent(source.instance)}/images/${encodeURIComponent(entry)}/${img.index}`;
  return {
    name: `image-${entry}-${img.index}.${ext === "jpeg" ? "jpg" : ext}`,
    mime: img.mime,
    size: null,
    path: `/api/v1/sessions/${at}?file=${encodeURIComponent(source.file)}`,
  };
}

const isUploaded = (v: unknown): v is Uploaded =>
  typeof v === "object" &&
  v !== null &&
  typeof (v as Uploaded).file === "string" &&
  typeof (v as Uploaded).mime === "string" &&
  typeof (v as Uploaded).size === "number";

/** Hub tools whose results list the files an agent sent (`FilesAttached`, `MessageSent`). */
const SENDS_FILES = /(^|_)(attach_files|send_message)$/;

type ToolResult = Extract<SessionItem, { kind: "tool_result" }>;

/** What a tool result shows besides its text: the images it returned and the files it sent. */
export function resultFiles(result: ToolResult, source: SessionSource | null): FileRef[] {
  const out = source ? result.images.map((img) => imageRef(source, result.id, img)) : [];
  if (!result.is_error && SENDS_FILES.test(result.tool)) {
    try {
      const v = JSON.parse(result.text) as { files?: unknown };
      if (Array.isArray(v.files)) out.push(...v.files.filter(isUploaded).map(uploadRef));
    } catch {
      // Not the op's JSON (an older or failed result): nothing to show.
    }
  }
  return out;
}
