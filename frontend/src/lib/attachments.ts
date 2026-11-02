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
const ATTACHES = /(^|_)attach_files$/;

type ToolResult = Extract<SessionItem, { kind: "tool_result" }>;

/** The files a tool result's JSON lists (`FilesAttached`, `MessageSent`). */
function listedFiles(result: ToolResult): FileRef[] {
  if (result.is_error || !SENDS_FILES.test(result.tool)) return [];
  try {
    const v = JSON.parse(result.text) as { files?: unknown };
    return Array.isArray(v.files) ? v.files.filter(isUploaded).map(uploadRef) : [];
  } catch {
    // Not the op's JSON (an older or failed result): nothing to show.
    return [];
  }
}

/** What a tool result shows besides its text: the images it returned and the files it sent to
 * another chat. Files attached to the reply are shown with the reply (`attachedFiles`). */
export function resultFiles(result: ToolResult, source: SessionSource | null): FileRef[] {
  const out = source ? result.images.map((img) => imageRef(source, result.id, img)) : [];
  if (!ATTACHES.test(result.tool)) out.push(...listedFiles(result));
  return out;
}

/** The files the agent attached to its reply while the call ran: the hub's record (also from
 * inside a script), and for older chats an `attach_files` call's own result. */
export function attachedFiles(result: ToolResult): FileRef[] {
  const out = result.files.map(uploadRef);
  if (ATTACHES.test(result.tool)) out.push(...listedFiles(result).filter((f) => !out.some((o) => o.path === f.path)));
  return out;
}
