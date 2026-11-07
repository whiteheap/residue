/** A mark on a meter, with its words above or below the bar. */
export interface Tick {
  /** Where, from 0 to 1. */
  at: number;
  label: string;
  side: "above" | "below";
}

const BAR = { regular: 8, thin: 4 } as const;
const LABEL_ROW = 16;

/**
 * One ratio against its whole: a thin bar on a track of the same gray. A
 * screen reader reads the native `meter`, with `valueText`; the drawing and its
 * tick labels are for the eye only and say nothing the text beside it does
 * not.
 */
export function Meter({
  value,
  max,
  label,
  valueText,
  ticks = [],
  size = "regular",
}: {
  value: number;
  max: number;
  label: string;
  valueText: string;
  ticks?: Tick[];
  size?: keyof typeof BAR;
}) {
  const fraction = max > 0 ? Math.min(1, Math.max(0, value / max)) : 0;
  const bar = BAR[size];
  const above = ticks.some((t) => t.side === "above") ? LABEL_ROW : 0;
  const below = ticks.some((t) => t.side === "below") ? LABEL_ROW : 0;
  const reach = ticks.length > 0 ? 4 : 0;
  const top = above + reach;
  const height = top + bar + reach + below;
  return (
    <div className="viz-meter" data-size={size}>
      <meter
        className="visually-hidden"
        min={0}
        max={max}
        value={Math.min(value, max)}
        aria-label={label}
        aria-valuetext={valueText}
      >
        {valueText}
      </meter>
      <svg width="100%" height={height} aria-hidden>
        <rect className="viz-track" y={top} width="100%" height={bar} rx={bar / 2} />
        {fraction > 0 ? (
          <rect className="viz-meter-fill" y={top} width={`${fraction * 100}%`} height={bar} rx={bar / 2} />
        ) : null}
        {ticks.map((t) => {
          const x = `${t.at * 100}%`;
          const late = t.at > 0.5;
          return (
            <g key={`${t.side}:${t.label}`} className="viz-tick">
              <line x1={x} x2={x} y1={above} y2={top + bar + reach} />
              {/* A 2 px cut through the bar, so the mark reads on the fill and the track alike. */}
              <rect className="viz-notch" x={x} y={top} width={2} height={bar} transform="translate(-1 0)" />
              <text
                x={x}
                dx={late ? -4 : 4}
                y={t.side === "above" ? above - 5 : height - 4}
                textAnchor={late ? "end" : "start"}
              >
                {t.label}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
