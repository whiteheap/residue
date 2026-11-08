import { useState } from "react";
import { bytes, plural } from "../../lib/format";
import { COMMANDS, localDay } from "../../lib/freed";
import type { Freed } from "../../lib/types";
import { useWidth } from "./measure";
import { TipLines, Tooltip } from "./Tooltip";

export type Week = Freed & { start: string; end: string };

const PLOT = 120;
const AXIS = 18;
const MAX_BAR = 24;
/** The space between two columns, at least the 2 px between any two fills. */
const GAP = 4;
const RADIUS = 4;

/** A column rising from the baseline, its top corners rounded. */
function column(x: number, w: number, h: number): string {
  const r = Math.min(RADIUS, h, w / 2);
  const top = PLOT - h;
  return `M${x},${PLOT}V${top + r}Q${x},${top} ${x + r},${top}H${x + w - r}Q${x + w},${top} ${x + w},${top + r}V${PLOT}Z`;
}

function shortDay(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso.slice(5, 10) : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/**
 * What residue freed each week, oldest first, as one column per week in one
 * tone. The tallest column carries its value; a pointer over a week shows
 * the rest. The drawing is hidden from assistive technology: the table
 * under it has every week's values.
 */
export function Columns({ weeks }: { weeks: Week[] }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(0, ...weeks.map((w) => w.bytes));
  const slot = weeks.length > 0 ? width / weeks.length : 0;
  const barW = Math.max(2, Math.min(MAX_BAR, Math.floor(slot - GAP)));
  const tallest = weeks.findIndex((w) => w.bytes === max);
  const at = (i: number) => Math.round(i * slot + (slot - barW) / 2);
  const height = (b: number) => (max > 0 && b > 0 ? Math.max(2, Math.round((b / max) * (PLOT - 16))) : 0);
  const tip = hover === null ? undefined : weeks[hover];
  // Dates under every fourth week, counted back from the latest.
  const dated = (i: number) => (weeks.length - 1 - i) % 4 === 0;

  return (
    <div
      className="viz-plot viz-columns"
      ref={ref}
      onPointerMove={(e) => {
        const box = e.currentTarget.getBoundingClientRect();
        const i = Math.floor((e.clientX - box.left) / (slot || 1));
        setHover(i >= 0 && i < weeks.length ? i : null);
      }}
      onPointerLeave={() => setHover(null)}
    >
      {width > 0 ? (
        <svg width={width} height={PLOT + AXIS} aria-hidden>
          <line className="viz-baseline" x1={0} x2={width} y1={PLOT + 0.5} y2={PLOT + 0.5} />
          {weeks.map((w, i) => {
            const h = height(w.bytes);
            return (
              <g key={w.start} className="viz-column" data-hover={hover === i ? "" : undefined}>
                {/* The whole slot answers the pointer, not only the column. */}
                <rect className="viz-hit" x={i * slot} y={0} width={slot} height={PLOT} />
                {h > 0 ? <path className="viz-fill" data-tone="bright" d={column(at(i), barW, h)} /> : null}
                {i === tallest && max > 0 ? (
                  <text
                    className="viz-label viz-column-value num"
                    x={Math.min(Math.max(at(i) + barW / 2, 24), width - 24)}
                    y={PLOT - h - 5}
                    textAnchor="middle"
                  >
                    {bytes(w.bytes)}
                  </text>
                ) : null}
                {dated(i) ? (
                  <text
                    className="viz-tick-label"
                    x={Math.min(Math.max(at(i) + barW / 2, 20), width - 20)}
                    y={PLOT + 13}
                    textAnchor="middle"
                  >
                    {i === weeks.length - 1 ? "this week" : shortDay(w.start)}
                  </text>
                ) : null}
              </g>
            );
          })}
        </svg>
      ) : (
        <div style={{ blockSize: PLOT + AXIS }} />
      )}
      {hover !== null && tip ? (
        <Tooltip x={at(hover) + barW / 2} y={PLOT - height(tip.bytes)} width={width} height={PLOT}>
          <TipLines
            value={bytes(tip.bytes)}
            lines={[
              `${plural(tip.worktrees, "worktree")}, ${localDay(tip.start)} to ${localDay(tip.end)}`,
              ...COMMANDS.filter((c) => tip.by[c.by].worktrees > 0).map(
                (c) => `${c.label}: ${bytes(tip.by[c.by].bytes)}`,
              ),
            ]}
          />
        </Tooltip>
      ) : null}
    </div>
  );
}

/** The weeks as a table: every value the columns draw, and each command's share. */
export function WeekTable({ weeks }: { weeks: Week[] }) {
  const used = COMMANDS.filter((c) => weeks.some((w) => w.by[c.by].worktrees > 0));
  return (
    <table className="viz-table">
      <thead>
        <tr>
          <th scope="col">Week</th>
          <th scope="col">Freed</th>
          <th scope="col">Worktrees</th>
          {used.map((c) => (
            <th scope="col" key={c.by}>
              {c.label}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {weeks.toReversed().map((w) => (
          <tr key={w.start}>
            <th scope="row">
              {localDay(w.start)} to {localDay(w.end)}
            </th>
            <td className="num">{bytes(w.bytes)}</td>
            <td className="num">{w.worktrees}</td>
            {used.map((c) => (
              <td className="num" key={c.by}>
                {w.by[c.by].worktrees > 0 ? bytes(w.by[c.by].bytes) : "none"}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
