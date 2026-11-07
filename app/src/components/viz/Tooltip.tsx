import type { ReactNode } from "react";

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
  children,
}: {
  x: number;
  y: number;
  width: number;
  height: number;
  below?: boolean;
  children: ReactNode;
}) {
  const across = x > width / 2 ? "- 100% - 12px" : "+ 12px";
  const down = !below && y > height / 2 ? "- 100% - 12px" : "+ 12px";
  return (
    <div className="viz-tip" aria-hidden style={{ translate: `calc(${x}px ${across}) calc(${y}px ${down})` }}>
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
