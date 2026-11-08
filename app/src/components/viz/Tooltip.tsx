import type { ReactNode, Ref } from "react";

/**
 * The `translate` that sets a tooltip 12 px from `x`, `y` in a plot of
 * `width` x `height`, on the side with more room; `below` keeps it under the
 * point. A chart that moves its tooltip with the pointer writes this to the
 * tooltip's style itself, so a move within one mark draws nothing again.
 */
export function tipTranslate(x: number, y: number, width: number, height: number, below = false): string {
  const across = x > width / 2 ? "- 100% - 12px" : "+ 12px";
  const down = !below && y > height / 2 ? "- 100% - 12px" : "+ 12px";
  return `calc(${x}px ${across}) calc(${y}px ${down})`;
}

/**
 * One tooltip for a chart, set 12 px from `x`, `y` inside a plot of `width` x
 * `height`, on the side with more room; `below` keeps it under the point. It
 * adds detail and never holds the only copy of a value: each chart's words or
 * the list beside it have them all, so it is hidden from assistive technology.
 */
export function Tooltip({
  x,
  y,
  width,
  height,
  below = false,
  ref,
  children,
}: {
  x: number;
  y: number;
  width: number;
  height: number;
  below?: boolean;
  ref?: Ref<HTMLDivElement>;
  children: ReactNode;
}) {
  return (
    <div className="viz-tip" aria-hidden ref={ref} style={{ translate: tipTranslate(x, y, width, height, below) }}>
      {children}
    </div>
  );
}

/** The tooltip's lines: the value first, then what it is. */
export function TipLines({ value, lines }: { value: string; lines: ReactNode[] }) {
  return (
    <>
      <p className="viz-tip-value num">{value}</p>
      {lines.map((line, i) => (
        // The lines are fixed in order for one tooltip; the index is the line.
        <p key={i}>{line}</p>
      ))}
    </>
  );
}
