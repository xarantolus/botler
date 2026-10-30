// Uploaded secret files: text files are sent as text, anything else as base64 of the exact bytes.

import type { SecretInput } from "../generated";

export interface LoadedFile {
  name: string;
  size: number;
  binary: boolean;
  value: SecretInput;
}

/** Whether bytes are valid UTF-8 text (no NUL bytes, decodes strictly). */
export function isText(bytes: Uint8Array): boolean {
  if (bytes.includes(0)) return false;
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return true;
  } catch {
    // Invalid UTF-8: binary.
    return false;
  }
}

export function toBase64(bytes: Uint8Array): string {
  let s = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) s += String.fromCharCode(...bytes.subarray(i, i + chunk));
  return btoa(s);
}

export function secretInputFromBytes(bytes: Uint8Array): { binary: boolean; value: SecretInput } {
  if (isText(bytes)) return { binary: false, value: new TextDecoder().decode(bytes) };
  return { binary: true, value: { base64: toBase64(bytes) } };
}

export async function loadFile(file: File): Promise<LoadedFile> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  return { name: file.name, size: file.size, ...secretInputFromBytes(bytes) };
}
