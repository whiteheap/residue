import { useState } from "react";
import { bytes } from "../../lib/format";
import { shareText, type Part } from "../../lib/viz";
import { Hatch, Legend, Mark, useSvgId } from "./Figure";
import { labelWidth, useWidth } from "./measure";
import { TipLines, Tooltip } from "./Tooltip";

const HEIGHT = 28;
const GAP = 2;
const MIN_BLOCK = 2;

/** Each part's block: proportional, at least 2 px, 2 px apart, the largest giving up what that adds. */
function blocks(parts: Part[], total: number, width: number): { x: number; w: number }[] {
  const room = width - GAP * Math.max(0, parts.length - 1);
  if (room <= 0 || total <= 0) return parts.map(() => ({ x: 0, w: 0 }));
  const widths = parts.map((p) => Math.max(MIN_BLOCK, Math.round((p.bytes / total) * room)));
  const over = widths.reduce((a, b) => a + b, 0) - room;
  if (over > 0) {
    const largest = widths.indexOf(Math.max(...widths));
    widths[largest] = Math.max(MIN_BLOCK, (widths[largest] ?? 0) - over);
  }
  let x = 0;
  return widths.map((w) => {
    const at = x;
    x += w + GAP;
    return { x: at, w };
  });
}

/**
 * One whole split into parts, as a tall bar: each part its own block, its
 * name and share printed inside where they fit (else the share alone), and a
 * legend of names in block order.
 * A screen reader skips the drawing and reads the legend, which gives it each
 * part's amount and share.
 */
export function SegmentedBar({ parts, total }: { parts: Part[]; total: number }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<{ i: number; x: number } | null>(null);
  const hatch = useSvgId("bar-hatch");
  const placed = blocks(parts, total, width);
  const tip = hover ? parts[hover.i] : undefined;

  return (
    <div className="viz-bar">
      <div className="viz-plot" ref={ref} onPointerLeave={() => setHover(null)}>
        {width > 0 ? (
          <svg width={width} height={HEIGHT} aria-hidden>
            <defs>
              <Hatch id={hatch} />
            </defs>
            {parts.map((p, i) => {
              const b = placed[i];
              if (!b || b.w <= 0) return null;
              const share = shareText(p.bytes, total);
              const named = `${p.label} ${share}`;
              const text = [named, share].find((t) => labelWidth(t) <= b.w - 12);
              return (
                <g
                  key={p.key}
                  className="viz-part"
                  data-tone={p.tone}
                  onPointerMove={(e) => {
                    const box = e.currentTarget.ownerSVGElement?.getBoundingClientRect();
                    if (box) setHover({ i, x: e.clientX - box.left });
                  }}
                >
                  <Mark tone={p.tone} hatch={hatch} x={b.x} y={0} width={b.w} height={HEIGHT} radius={4} />
                  {text ? (
                    <text className="viz-label" x={b.x + 6} y={18}>
                      {text}
                    </text>
                  ) : null}
                </g>
              );
            })}
          </svg>
        ) : (
          <div style={{ blockSize: HEIGHT }} />
        )}
        {hover && tip ? (
          <Tooltip x={hover.x} y={HEIGHT} width={width} height={HEIGHT} below>
            <TipLines
              value={`${bytes(tip.bytes)}, ${shareText(tip.bytes, total)}`}
              lines={[<code key="name">{tip.label}</code>, tip.note]}
            />
          </Tooltip>
        ) : null}
      </div>
      <Legend
        items={parts.map((p) => ({
          tone: p.tone,
          label: p.label,
          spoken: `${bytes(p.bytes)}, ${shareText(p.bytes, total)}, ${p.note}`,
        }))}
      />
    </div>
  );
}
