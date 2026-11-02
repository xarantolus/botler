// A horizontal swipe on a phone: left to right opens the sidebar, right to left closes it.

import { useEventListener, useMediaQuery } from "@vueuse/core";
import { shell } from "../shell";

/** How far (px) a finger must travel sideways, and how much more sideways than up or down. */
const DISTANCE = 60;
const SLOPE = 2;

/** The direction of a touch from (x0, y0) to (x1, y1), if it was a horizontal swipe. */
export function swipe(x0: number, y0: number, x1: number, y1: number): "right" | "left" | null {
  const dx = x1 - x0;
  const dy = Math.abs(y1 - y0);
  if (Math.abs(dx) < DISTANCE || Math.abs(dx) < SLOPE * dy) return null;
  return dx > 0 ? "right" : "left";
}

/** Whether the swipe is `target`'s own: it is in the image gallery, or in something that could
 * still scroll that way (wide code, a table). A panel that only scrolls up and down, or a code
 * block already at that edge, leaves it to the sidebar. */
export function ownsSwipe(target: EventTarget | null, dir: "right" | "left"): boolean {
  for (let el = target instanceof Element ? target : null; el; el = el.parentElement) {
    if (el.classList.contains("pswp")) return true;
    if (el.scrollWidth <= el.clientWidth + 1) continue;
    const overflow = getComputedStyle(el).overflowX;
    if (overflow !== "auto" && overflow !== "scroll") continue;
    // A finger moving right scrolls the content back towards its start.
    if (dir === "right" ? el.scrollLeft > 0 : el.scrollLeft + el.clientWidth < el.scrollWidth - 1) return true;
  }
  return false;
}

/** On phones the sidebar is a drawer: swiping left to right opens it, right to left closes it. */
export function useSidebarSwipe() {
  // Decided while the finger moves, once it went far enough: waiting for it to lift felt slow, and a
  // phone's browser often cancels a touch that moved at all vertically (it scrolls instead), and
  // then sends no touchend.
  const phone = useMediaQuery("(max-width: 1023.98px)");
  let touch: { x0: number; y0: number; target: EventTarget | null } | null = null;
  const passive = { passive: true };
  useEventListener(
    document,
    "touchstart",
    (e: TouchEvent) => {
      const t = e.touches[0];
      touch = phone.value && e.touches.length === 1 && t ? { x0: t.clientX, y0: t.clientY, target: e.target } : null;
    },
    passive,
  );
  useEventListener(
    document,
    "touchmove",
    (e: TouchEvent) => {
      const t = e.touches[0];
      if (!touch || !t || e.touches.length !== 1) return void (touch = null);
      const dir = swipe(touch.x0, touch.y0, t.clientX, t.clientY);
      if (!dir) return;
      const { target } = touch;
      touch = null;
      if (ownsSwipe(target, dir)) return;
      if (dir === "right" && !shell.sidebar) shell.sidebar = true;
      if (dir === "left" && shell.sidebar) shell.sidebar = false;
    },
    passive,
  );
  const endTouch = () => void (touch = null);
  useEventListener(document, "touchend", endTouch, passive);
  useEventListener(document, "touchcancel", endTouch, passive);
}
