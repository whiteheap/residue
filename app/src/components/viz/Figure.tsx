import { useId, type ReactNode } from "react";
import type { Tone } from "../../lib/viz";

/**
 * A chart and its words. `busy` keeps the last drawing on screen, dimmed,
 * while residue reads again. The caption holds what the chart leaves out.
 */
export function Figure({
  label,
  head,
  caption,
  busy = false,
  children,
}: {
  label: string;
  head?: ReactNode;
  caption?: ReactNode;
  busy?: boolean;
  children: ReactNode;
}) {
  return (
    <figure className="viz-figure" aria-label={label} aria-busy={busy}>
      {head ? <div className="viz-head">{head}</div> : null}
      <div className="viz-body">{children}</div>
      {caption ? <figcaption className="viz-caption">{caption}</figcaption> : null}
    </figure>
  );
}

/** An id an SVG `url(#…)` can name: `useId` output, with its punctuation removed. */
export function useSvgId(prefix: string): string {
  return `${prefix}-${useId().replace(/[^A-Za-z0-9_-]/g, "")}`;
}

/**
 * The texture for a value residue could not settle: lines at 45 degrees, 6 px
 * apart, in `--viz-hatch`, laid over the gray fill. Under forced colors it
 * marks every mark that is not bright, since the grays are gone.
 */
export function Hatch({ id }: { id: string }) {
  return (
    <pattern id={id} width={6} height={6} patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
      <rect className="viz-hatch-line" width={2} height={6} />
    </pattern>
  );
}

/** A mark's fill, with the hatch over it when its tone is hatched. */
export function Mark({
  tone,
  hatch,
  x,
  y,
  width,
  height,
  radius,
}: {
  tone: Tone;
  hatch: string;
  x: number;
  y: number;
  width: number;
  height: number;
  radius: number;
}) {
  return (
    <>
      <rect className="viz-fill" data-tone={tone} x={x} y={y} width={width} height={height} rx={radius} />
      {tone === "bright" ? null : (
        <rect
          className="viz-hatch"
          data-tone={tone}
          x={x}
          y={y}
          width={width}
          height={height}
          rx={radius}
          fill={`url(#${hatch})`}
        />
      )}
    </>
  );
}

function Swatch({ tone }: { tone: Tone }) {
  const hatch = useSvgId("swatch-hatch");
  return (
    <svg className="viz-swatch" width={10} height={10} aria-hidden>
      <defs>
        <Hatch id={hatch} />
      </defs>
      <Mark tone={tone} hatch={hatch} x={0} y={0} width={10} height={10} radius={2} />
    </svg>
  );
}

/**
 * What each tone means in this chart, in the order the chart draws them. A
 * `spoken` detail is read by a screen reader only: where the chart's own
 * drawing is hidden, the legend carries its values.
 */
export function Legend({ items }: { items: { tone: Tone; label: string; spoken?: string }[] }) {
  return (
    <ul className="viz-legend">
      {items.map((i) => (
        <li key={`${i.tone}:${i.label}`}>
          <Swatch tone={i.tone} />
          {i.label}
          {i.spoken ? <span className="visually-hidden">: {i.spoken}</span> : null}
        </li>
      ))}
    </ul>
  );
}
