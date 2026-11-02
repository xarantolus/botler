// @vitest-environment happy-dom
// Swiping the sidebar open and closed on a phone, with the touches a phone's browser sends: a
// touch that also moved a little vertically ends in touchcancel, not touchend.

import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { defineComponent } from "vue";

// A phone's width.
vi.stubGlobal(
  "matchMedia",
  vi.fn((query: string) => ({
    matches: query.includes("max-width"),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  })),
);

import { ownsSwipe, useSidebarSwipe } from "../src/lib/swipe";
import { shell } from "../src/shell";

function touch(type: string, target: EventTarget, x: number, y: number) {
  const ev = new Event(type, { bubbles: true });
  const point = [{ clientX: x, clientY: y }];
  Object.assign(ev, { touches: type === "touchend" || type === "touchcancel" ? [] : point, changedTouches: point });
  target.dispatchEvent(ev);
}

/** A finger from x0 to x1, drifting down a little, as the browser reports it. */
function drag(target: EventTarget, x0: number, x1: number, end: "touchend" | "touchcancel") {
  touch("touchstart", target, x0, 300);
  touch("touchmove", target, (x0 + x1) / 2, 305);
  touch("touchmove", target, x1, 310);
  // A cancelled touch reports where it started, not where it went.
  touch(end, target, end === "touchcancel" ? x0 : x1, 310);
}

describe("swiping the sidebar", () => {
  it("opens left to right and closes right to left while the finger moves, also when the browser cancels the touch", async () => {
    mount(defineComponent({ setup: () => useSidebarSwipe(), render: () => null }));
    const chat = document.createElement("div");
    document.body.append(chat);
    shell.sidebar = false;
    // Open before the finger lifts.
    touch("touchstart", chat, 40, 300);
    touch("touchmove", chat, 130, 305);
    expect(shell.sidebar).toBe(true);
    touch("touchend", chat, 130, 305);
    shell.sidebar = false;
    drag(chat, 40, 220, "touchcancel");
    await flushPromises();
    expect(shell.sidebar).toBe(true);
    drag(chat, 300, 80, "touchend");
    await flushPromises();
    expect(shell.sidebar).toBe(false);
    // Mostly vertical: scrolling, not a swipe.
    touch("touchstart", chat, 40, 100);
    touch("touchmove", chat, 120, 400);
    touch("touchcancel", chat, 40, 100);
    expect(shell.sidebar).toBe(false);
  });

  it("leaves a swipe to what can still scroll that way, and to the gallery", () => {
    const wide = document.createElement("pre");
    wide.style.overflowX = "auto";
    Object.defineProperties(wide, { scrollWidth: { value: 800 }, clientWidth: { value: 300 }, scrollLeft: { value: 0, writable: true } });
    document.body.append(wide);
    // At its left edge: a right swipe is the sidebar's, a left one scrolls the code.
    expect(ownsSwipe(wide, "right")).toBe(false);
    expect(ownsSwipe(wide, "left")).toBe(true);
    wide.scrollLeft = 100;
    expect(ownsSwipe(wide, "right")).toBe(true);
    const gallery = document.createElement("div");
    gallery.className = "pswp";
    expect(ownsSwipe(gallery, "right")).toBe(true);
  });
});
