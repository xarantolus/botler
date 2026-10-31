// The app's keyboard shortcuts: one list for the handlers (AppShell) and the help in the command
// palette. Keys use defineShortcuts' syntax (`g-r` is "g, then r"); none fire while typing.

import type { Router } from "vue-router";
import { shell } from "./shell";

export interface Shortcut {
  /** defineShortcuts key. */
  key: string;
  /** How it reads: one entry per key to press. */
  kbds: string[];
  label: string;
  run: (router: Router) => void;
}

/** The palette, filtered to the shortcuts. */
function showShortcuts() {
  shell.paletteTerm = "shortcut";
  shell.palette = true;
}

const go = (name: string) => (router: Router) => void router.push({ name });

/** Focus the chat composer, if the chat is open. */
function focusComposer() {
  document.querySelector<HTMLTextAreaElement>("#chat-composer textarea")?.focus();
}

export const SHORTCUTS: Shortcut[] = [
  { key: "/", kbds: ["/"], label: "Write a message", run: focusComposer },
  { key: "n", kbds: ["n"], label: "New chat", run: () => (shell.newChat = true) },
  { key: "shift_?", kbds: ["?"], label: "Keyboard shortcuts", run: showShortcuts },
  { key: "g-c", kbds: ["g", "c"], label: "Go to the chat", run: go("home") },
  { key: "g-r", kbds: ["g", "r"], label: "Go to requests", run: go("requests") },
  { key: "g-s", kbds: ["g", "s"], label: "Go to search", run: go("search") },
  { key: "g-e", kbds: ["g", "e"], label: "Go to events", run: go("events") },
  { key: "g-u", kbds: ["g", "u"], label: "Go to runs", run: go("runs") },
  { key: "g-i", kbds: ["g", "i"], label: "Go to incidents", run: go("incidents") },
  { key: "g-p", kbds: ["g", "p"], label: "Go to proposals", run: go("proposals") },
  { key: "g-m", kbds: ["g", "m"], label: "Go to memory", run: go("memory") },
];

/** Shown in the palette's help, handled elsewhere (the kit, the composer). */
export const BUILTIN_SHORTCUTS: { kbds: string[]; label: string }[] = [
  { kbds: ["meta", "k"], label: "Command palette" },
  { kbds: ["enter"], label: "Send (a working agent reads it after its running step)" },
  { kbds: ["ctrl", "enter"], label: "Queue until the agent is done" },
  { kbds: ["shift", "enter"], label: "New line" },
  { kbds: ["escape"], label: "Close a dialog, leave the composer" },
];
