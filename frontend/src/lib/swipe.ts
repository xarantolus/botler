// A horizontal swipe on a phone: left to right opens the sidebar, right to left closes it.

/** How far (px) a finger must travel sideways, and how much more sideways than up or down. */
const DISTANCE = 60;
const SLOPE = 2;

/** The direction of a finished touch from (x0, y0) to (x1, y1), if it was a horizontal swipe. */
export function swipe(
  x0: number,
  y0: number,
  x1: number,
  y1: number,
): "right" | "left" | null {
  const dx = x1 - x0;
  const dy = Math.abs(y1 - y0);
  if (Math.abs(dx) < DISTANCE || Math.abs(dx) < SLOPE * dy) return null;
  return dx > 0 ? "right" : "left";
}

/** The touch began on something that scrolls sideways itself (code, tables): the swipe is its own. */
export function scrollsSideways(target: EventTarget | null): boolean {
  for (
    let el = target instanceof Element ? target : null;
    el;
    el = el.parentElement
  ) {
    const overflow = getComputedStyle(el).overflowX;
    if (
      (overflow === "auto" || overflow === "scroll") &&
      el.scrollWidth > el.clientWidth
    )
      return true;
  }
  return false;
}
