import {
  ArrowsClockwise,
  ClockCounterClockwise,
  FolderSimple,
  TreeStructure,
  type Icon,
} from "@phosphor-icons/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@cloudflare/kumo";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { Mark } from "./components/Mark";
import type { ZoomLevel } from "./components/viz/Treemap";
import { ErrorPanel, Loading } from "./components/ui";
import { api } from "./lib/api";
import { bytes } from "./lib/format";
import type { Settings } from "./lib/types";
import type { Filter } from "./lib/viz";
import { useCli } from "./lib/useCli";
import { Automatic } from "./views/Automatic";
import { Detail } from "./views/Detail";
import { Folders } from "./views/Folders";
import { RemoveDialog } from "./views/RemoveDialog";
import { Restore } from "./views/Restore";
import { Worktrees } from "./views/Worktrees";

type View = "worktrees" | "restore" | "automatic" | "folders";

const NAV: { view: View; label: string; icon: Icon; key: string }[] = [
  { view: "worktrees", label: "Worktrees", icon: TreeStructure, key: "1" },
  { view: "restore", label: "Restore", icon: ClockCounterClockwise, key: "2" },
  { view: "automatic", label: "Automatic cleanup", icon: ArrowsClockwise, key: "3" },
  { view: "folders", label: "Folders", icon: FolderSimple, key: "4" },
];

const NO_ROOTS: string[] = [];

export function App() {
  const infoQ = useCli("info", api.appInfo);
  const settingsQ = useCli("settings", api.settings);
  const [saved, setSaved] = useState<Settings | null>(null);
  const [view, setView] = useState<View>("worktrees");
  const [detail, setDetail] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [removing, setRemoving] = useState<string[] | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  // The Worktrees filter and chart zoom, kept here so leaving a worktree's checks returns to them.
  const [filter, setFilter] = useState<Filter>("all");
  const [zoom, setZoom] = useState<ZoomLevel[]>([]);

  const info = infoQ.data;
  const settings = saved ?? settingsQ.data;
  const home = info?.home;
  const roots = settings?.roots ?? NO_ROOTS;

  // Scan when the folders are known, and again whenever they change. The
  // key is a string, so the same folders in a new settings object do not
  // start another scan.
  const rootsKey = settings && settings.roots.length > 0 ? JSON.stringify(settings.roots) : null;
  const scanQ = useCli(rootsKey, () => api.scan(roots));
  // A scan of other folders is not shown as this one.
  const scanDoc = scanQ.dataKey === rootsKey ? scanQ.data : undefined;
  const rescan = scanQ.reload;

  // What opened the detail (`data-opener`), so leaving the detail returns
  // focus there, as a keyboard or screen reader user left it.
  const opener = useRef<string | null>(null);
  const open = useCallback((path: string) => {
    const from = document.activeElement?.closest<HTMLElement>("[data-opener]");
    opener.current = from?.dataset.opener ?? `row:${path}`;
    setDetail(path);
  }, []);
  useEffect(() => {
    if (detail !== null || opener.current === null) return;
    const key = opener.current;
    opener.current = null;
    document.querySelector<HTMLElement>(`[data-opener="${CSS.escape(key)}"]`)?.focus();
  }, [detail]);

  const go = useCallback((v: View) => {
    // Another screen: nothing on it opened a detail.
    opener.current = null;
    setView(v);
    setDetail(null);
  }, []);

  // Command-1..4 switch screens, Command-R refreshes, Escape leaves a detail.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (removing) return;
      if (e.metaKey && !e.shiftKey && !e.altKey && !e.ctrlKey) {
        const item = NAV.find((n) => n.key === e.key);
        if (item) {
          e.preventDefault();
          go(item.view);
          return;
        }
        if (e.key.toLowerCase() === "r") {
          e.preventDefault();
          if (view === "worktrees") rescan();
          else setRefreshKey((k) => k + 1);
        }
      } else if (e.key === "Escape" && detail) {
        e.preventDefault();
        setDetail(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [go, rescan, view, detail, removing]);

  const select = useCallback((paths: string[], on: boolean) => {
    setSelected((sel) => {
      const next = new Set(sel);
      for (const p of paths) {
        if (on) next.add(p);
        else next.delete(p);
      }
      return next;
    });
  }, []);

  const reclaimable = scanDoc?.summary.candidate_reclaimable_bytes ?? null;

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-drag" data-tauri-drag-region />
        <div className="brand" data-tauri-drag-region>
          <Mark className="brand-mark" />
          <span data-tauri-drag-region>residue</span>
        </div>
        <nav aria-label="Screens">
          <ul className="nav">
            {NAV.map((n) => (
              <li key={n.view}>
                <button
                  type="button"
                  className="nav-item"
                  aria-current={view === n.view ? "page" : undefined}
                  onClick={() => go(n.view)}
                  title={`${n.label} (Command-${n.key})`}
                >
                  <n.icon size={18} weight={view === n.view ? "fill" : "regular"} aria-hidden />
                  <span className="nav-label">{n.label}</span>
                  {n.view === "worktrees" && reclaimable ? (
                    <span className="nav-count num">{bytes(reclaimable)}</span>
                  ) : null}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <p className="sidebar-foot">
          {info ? (
            <>
              residue {info.residue_version}
              <br />
              app {info.app_version}
            </>
          ) : null}
        </p>
      </aside>

      <main className="main">
        {infoQ.error ? (
          <div className="page-body">
            <ErrorPanel
              title="residue cannot start"
              error={infoQ.error}
              action={
                <Button variant="secondary" onClick={infoQ.reload}>
                  Try again
                </Button>
              }
            />
          </div>
        ) : settingsQ.error && !settings ? (
          <div className="page-body">
            <ErrorPanel
              title="The app's settings could not be read"
              error={settingsQ.error}
              action={
                <Button variant="secondary" onClick={settingsQ.reload}>
                  Try again
                </Button>
              }
            />
          </div>
        ) : !settings ? (
          <div className="page-body">
            <Loading title="Starting" />
          </div>
        ) : (
          // A new screen starts without the last one's failure.
          <ErrorBoundary
            key={`${view}:${detail ?? ""}`}
            actions={
              detail || view !== "worktrees" ? (
                <Button variant="secondary" onClick={() => (detail ? setDetail(null) : go("worktrees"))}>
                  Back to Worktrees
                </Button>
              ) : null
            }
          >
            {view === "worktrees" ? (
              detail ? (
                <Detail path={detail} home={home} onBack={() => setDetail(null)} onRemove={setRemoving} />
              ) : (
                <Worktrees
                  doc={scanDoc}
                  error={scanQ.error}
                  scanning={scanQ.loading}
                  roots={roots}
                  home={home}
                  filter={filter}
                  onFilter={setFilter}
                  zoom={zoom}
                  onZoom={setZoom}
                  selected={selected}
                  onSelect={select}
                  onRescan={rescan}
                  onOpen={open}
                  onRemove={setRemoving}
                  onChooseFolders={() => go("folders")}
                />
              )
            ) : view === "restore" ? (
              <Restore
                roots={roots}
                home={home}
                keepTrash={info?.default_keep_trash}
                keepRemoved={info?.default_keep_removed}
                refreshKey={refreshKey}
                onRestored={rescan}
              />
            ) : view === "automatic" ? (
              <Automatic
                info={info ?? null}
                roots={roots}
                suggested={settings.suggested}
                home={home}
                refreshKey={refreshKey}
                scanDoc={scanDoc}
                scanning={scanQ.loading}
              />
            ) : (
              <Folders settings={settings} home={home} onSaved={setSaved} />
            )}
          </ErrorBoundary>
        )}
      </main>

      {removing ? (
        // The dialog's own boundary: a failure to draw a result after a
        // confirmed move stays in the dialog, and closing it scans again.
        <ErrorBoundary
          onClose={() => {
            setRemoving(null);
            rescan();
          }}
        >
          <RemoveDialog
            paths={removing}
            home={home}
            onClose={() => setRemoving(null)}
            onRemoved={(doc) => {
              const moved = new Set(doc.results.filter((r) => r.result === "trashed").map((r) => r.path));
              setSelected((sel) => new Set([...sel].filter((p) => !moved.has(p))));
              if (detail && moved.has(detail)) setDetail(null);
              rescan();
            }}
          />
        </ErrorBoundary>
      ) : null}
    </div>
  );
}
