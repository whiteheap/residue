import { useLayoutEffect, useState } from "react";

/**
 * The width of an element, in whole pixels, kept current as it resizes. The
 * first value is a callback ref, so an element that mounts after the
 * component does is measured too.
 */
export function useWidth<T extends HTMLElement>() {
  const [el, setEl] = useState<T | null>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    if (!el) return;
    // The observer reports the first size as soon as it starts observing.
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(Math.floor(entry.contentRect.width));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [el]);
  return [setEl, width] as const;
}

/** Chart labels are IBM Plex Mono at 11 px (`.viz-label`): every glyph is 0.6 em wide. */
export const LABEL_PX = 11;
const ADVANCE = 0.6 * LABEL_PX;

/** How wide `text` sets as a chart label. */
export function labelWidth(text: string): number {
  return text.length * ADVANCE;
}

/**
 * `text` as it fits in `width` pixels of chart label: whole, or cut with an
 * ellipsis while 4 characters still show; null when it does not fit at all.
 * A label is measured, never clipped by its mark.
 */
export function fitLabel(text: string, width: number): string | null {
  const room = Math.floor(width / ADVANCE);
  if (room >= text.length) return text;
  if (room < 5) return null;
  return `${text.slice(0, room - 1)}…`;
}
