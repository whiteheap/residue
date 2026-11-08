import { Button } from "@cloudflare/kumo";
import { FolderPlus, Plus, X } from "@phosphor-icons/react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { ErrorPanel, PageHeader, SolidButton } from "../components/ui";
import { api, asCliError } from "../lib/api";
import { tilde } from "../lib/format";
import type { CliError, Settings } from "../lib/types";

export function Folders({
  settings,
  home,
  onSaved,
}: {
  settings: Settings;
  home: string | undefined;
  onSaved: (s: Settings) => void;
}) {
  const [error, setError] = useState<CliError | null>(null);
  const [saving, setSaving] = useState(false);

  const save = async (roots: string[]) => {
    setSaving(true);
    setError(null);
    try {
      onSaved(await api.setRoots(roots));
    } catch (e) {
      setError(asCliError(e));
    } finally {
      setSaving(false);
    }
  };

  const add = async () => {
    const picked = await openDialog({ directory: true, multiple: true, title: "Folders to scan" });
    const list = picked == null ? [] : Array.isArray(picked) ? picked : [picked];
    if (list.length > 0) await save([...new Set([...settings.roots, ...list])]);
  };

  const unused = settings.suggested.filter((s) => !settings.roots.includes(s));

  return (
    <>
      <PageHeader
        title="Folders"
        description="residue scans these folders and every folder inside them."
        actions={
          <SolidButton icon={<FolderPlus />} onClick={add} disabled={saving}>
            Add a folder…
          </SolidButton>
        }
      />
      <div className="page-body">
        {error ? <ErrorPanel title="The folders were not saved" error={error} /> : null}
        {!settings.saved && settings.roots.length > 0 ? (
          <p className="notice">
            Found in your home folder. Add or remove one to save your own list.
          </p>
        ) : null}
        {settings.roots.length === 0 ? (
          <p className="quiet">No folders yet. Add the ones that hold your code.</p>
        ) : (
          <ul className="rows folder-rows">
            {settings.roots.map((root) => (
              <li key={root} className="row">
                <span className="row-main static">
                  <span className="row-name">{tilde(root, home)}</span>
                </span>
                <span className="row-action">
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<X />}
                    disabled={saving}
                    onClick={() => save(settings.roots.filter((r) => r !== root))}
                    aria-label={`Remove ${tilde(root, home)}`}
                  >
                    Remove
                  </Button>
                </span>
              </li>
            ))}
          </ul>
        )}
        {unused.length > 0 ? (
          <section className="block">
            <h2>Found in your home folder</h2>
            <ul className="rows folder-rows">
              {unused.map((root) => (
                <li key={root} className="row">
                  <span className="row-main static">
                    <span className="row-name">{tilde(root, home)}</span>
                  </span>
                  <span className="row-action">
                    <Button
                      variant="ghost"
                      size="sm"
                      icon={<Plus />}
                      disabled={saving}
                      onClick={() => save([...settings.roots, root])}
                      aria-label={`Add ${tilde(root, home)}`}
                    >
                      Add
                    </Button>
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
      </div>
    </>
  );
}
