import { useCallback, useEffect, useRef, useState } from "react";
import { asCliError } from "./api";
import type { CliError } from "./types";

interface Settled<T> {
  key: string | null;
  base: string | null;
  data: T | undefined;
  error: CliError | undefined;
}

/**
 * Run a residue command whenever `key` changes, or when `reload` is called.
 * The last result stays on screen while the next one runs; `loading` is true
 * until the run for the current key settles. `key` null runs nothing.
 */
export function useCli<T>(key: string | null, run: () => Promise<T>) {
  const [nonce, setNonce] = useState(0);
  const want = key === null ? null : `${nonce}:${key}`;
  const [settled, setSettled] = useState<Settled<T>>({ key: null, base: null, data: undefined, error: undefined });
  const runRef = useRef(run);

  useEffect(() => {
    runRef.current = run;
  });

  useEffect(() => {
    if (want === null) return;
    let live = true;
    runRef.current().then(
      (data) => {
        if (live) setSettled({ key: want, base: key, data, error: undefined });
      },
      (e: unknown) => {
        if (live) setSettled((s) => ({ key: want, base: s.base, data: s.data, error: asCliError(e) }));
      },
    );
    return () => {
      live = false;
    };
  }, [want, key]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  const current = want !== null && settled.key === want;
  return {
    data: settled.data,
    // The key the data was loaded for, which may be an earlier one.
    dataKey: settled.base,
    error: current ? settled.error : undefined,
    loading: want !== null && !current,
    reload,
  };
}

/** Seconds since the epoch at the time residue wrote a document. */
export function documentTime(generatedAt: string | undefined): number | null {
  if (!generatedAt) return null;
  const t = Date.parse(generatedAt);
  return Number.isNaN(t) ? null : Math.floor(t / 1000);
}
