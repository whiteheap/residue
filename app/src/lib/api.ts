import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  CliError,
  ExplainDocument,
  Listing,
  Removal,
  RestoreDocument,
  ScanDocument,
  ServiceBasis,
  ServiceChange,
  ServicePreview,
  ServiceSave,
  ServiceStatus,
  Settings,
  StatsDocument,
} from "./types";

/** Every rejection from a command is a CliError; anything else is wrapped. */
export function asCliError(e: unknown): CliError {
  if (e && typeof e === "object" && "kind" in e && "message" in e) {
    return e as CliError;
  }
  return {
    kind: "failed",
    message: typeof e === "string" ? e : String(e),
    command: null,
    stderr: null,
    exit_code: null,
  };
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw asCliError(e);
  }
}

export const api = {
  appInfo: () => call<AppInfo>("app_info"),
  settings: () => call<Settings>("get_settings"),
  setRoots: (roots: string[]) => call<Settings>("set_roots", { roots }),
  scan: (roots: string[]) => call<ScanDocument>("scan", { roots }),
  explain: (path: string) => call<ExplainDocument>("explain", { path }),
  planRemoval: (paths: string[]) => call<Removal>("plan_removal", { paths }),
  removeToTrash: (paths: string[]) => call<Removal>("remove_to_trash", { paths }),
  listRestorable: (roots: string[]) => call<Listing[]>("list_restorable", { roots }),
  restore: (entryPath: string, root: string) =>
    call<RestoreDocument>("restore", { entryPath, root }),
  serviceStatus: () => call<ServiceStatus>("service_status"),
  /** `basis` is the one the dialog showed; a fresh read that differs installs nothing. */
  serviceInstall: (roots: string[], dryRun: boolean, basis: ServiceBasis) =>
    call<ServiceSave>("service_install", { roots, dryRun, basis }),
  serviceUninstall: () => call<ServiceChange>("service_uninstall"),
  servicePreview: (roots: string[]) => call<ServicePreview>("service_preview", { roots }),
  stats: () => call<StatsDocument>("stats"),
};
