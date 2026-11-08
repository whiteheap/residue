import { bytes, plural } from "../../lib/format";
import type { FreedSum } from "../../lib/types";

export interface RankedItem {
  key: string;
  label: string;
  sum: FreedSum;
  note?: string | null;
}

/**
 * Amounts that add to one whole, largest first, as bars in one tone, each
 * with its name and value in words: a list a screen reader reads as it is.
 */
export function Ranked({ items, label }: { items: RankedItem[]; label: string }) {
  const sorted = items.toSorted((a, b) => b.sum.bytes - a.sum.bytes);
  const max = sorted[0]?.sum.bytes ?? 0;
  return (
    <ol className="viz-ranked" aria-label={label}>
      {sorted.map((item) => (
        <li key={item.key}>
          <span className="viz-ranked-name">{item.label}</span>
          <span className="viz-ranked-value">
            <span className="num">{bytes(item.sum.bytes)}</span>, {plural(item.sum.worktrees, "worktree")}
            {item.note ? `, ${item.note}` : ""}
          </span>
          <span className="viz-ranked-track" aria-hidden>
            <span
              className="viz-ranked-bar"
              style={{ inlineSize: max > 0 ? `${(item.sum.bytes / max) * 100}%` : 0 }}
            />
          </span>
        </li>
      ))}
    </ol>
  );
}
