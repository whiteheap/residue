//! The residue Mac app: a window over the residue CLI.
//!
//! Every check, size, removal and restore comes from the bundled `residue`
//! binary. The commands here run it with explicit arguments and return its
//! JSON documents (`residue.scan/v1`, `residue.explain/v1`,
//! `residue.remove/v1`, `residue.restore/v1`) to the window unchanged. The
//! window never decides on its own what is safe to remove.

mod cli;
#[cfg(all(debug_assertions, target_os = "macos"))]
mod devshot;
mod env;
mod service;
mod settings;

use cli::{CliError, ErrorKind, Run};
use serde::Serialize;
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

/// A scan reads every entry of every worktree; a large home folder takes a
/// while, so the deadline only catches a run that hangs.
const SCAN_DEADLINE: Duration = Duration::from_secs(30 * 60);
const REMOVE_DEADLINE: Duration = Duration::from_secs(30 * 60);
const RESTORE_DEADLINE: Duration = Duration::from_secs(10 * 60);

struct State {
    sidecar: PathBuf,
    home: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    dev: service::DevPreview,
}

type AppState<'a> = tauri::State<'a, Arc<State>>;

impl State {
    fn run(
        &self,
        args: Vec<OsString>,
        cwd: &Path,
        deadline: Duration,
    ) -> Result<cli::Output, CliError> {
        if !self.sidecar.is_file() {
            return Err(CliError {
                kind: ErrorKind::Missing,
                message: format!(
                    "the bundled residue binary is missing ({}); reinstall the app",
                    self.sidecar.display()
                ),
                command: None,
                stderr: None,
                exit_code: None,
            });
        }
        Run {
            bin: &self.sidecar,
            args,
            cwd,
            path_env: &env::path().0,
            deadline,
        }
        .output()
    }

    fn bundled_version(&self) -> Result<String, CliError> {
        let out = self.run(
            vec!["--version".into()],
            &self.home,
            Duration::from_secs(10),
        )?;
        let text = out.stdout.trim();
        match text.strip_prefix("residue ") {
            Some(v) if out.code == Some(0) => Ok(v.to_string()),
            _ => Err(out.error(
                ErrorKind::BadOutput,
                format!("residue --version printed {text:?}"),
            )),
        }
    }

    fn service_context<'a>(&'a self, version: &'a str) -> service::Context<'a> {
        service::Context {
            sidecar: &self.sidecar,
            bundled_version: version,
            data_dir: &self.data_dir,
            home: &self.home,
            path_env: &env::path().0,
        }
    }
}

/// Run blocking work off the window's thread.
async fn blocking<T: Send + 'static>(
    state: &AppState<'_>,
    work: impl FnOnce(&State) -> Result<T, CliError> + Send + 'static,
) -> Result<T, CliError> {
    let state = Arc::clone(state);
    tauri::async_runtime::spawn_blocking(move || work(&state))
        .await
        .map_err(|e| CliError::failed(format!("the background task stopped: {e}")))?
}

#[derive(Serialize)]
struct AppInfo {
    app_version: String,
    residue_version: String,
    sidecar: String,
    home: String,
    path_source: env::PathSource,
    dev_service_preview: bool,
    /// `residue remove --keep-trash` when none is given, which the app's
    /// removals and automatic cleanup use.
    default_keep_trash: &'static str,
}

#[tauri::command]
async fn app_info(app: tauri::AppHandle, state: AppState<'_>) -> Result<AppInfo, CliError> {
    let app_version = app.package_info().version.to_string();
    blocking(&state, move |s| {
        let residue_version = s.bundled_version()?;
        Ok(AppInfo {
            app_version,
            residue_version,
            sidecar: s.sidecar.to_string_lossy().into_owned(),
            home: s.home.to_string_lossy().into_owned(),
            path_source: env::path().1,
            dev_service_preview: s.dev.enabled,
            default_keep_trash: service::DEFAULT_KEEP_TRASH,
        })
    })
    .await
}

#[tauri::command]
async fn get_settings(state: AppState<'_>) -> Result<settings::Settings, CliError> {
    blocking(&state, |s| settings::load(&s.config_dir, &s.home)).await
}

#[tauri::command]
async fn set_roots(
    state: AppState<'_>,
    roots: Vec<String>,
) -> Result<settings::Settings, CliError> {
    blocking(&state, move |s| {
        settings::save(&s.config_dir, &s.home, &roots)
    })
    .await
}

/// `residue scan --json ROOT...`
#[tauri::command]
async fn scan(state: AppState<'_>, roots: Vec<String>) -> Result<Value, CliError> {
    blocking(&state, move |s| {
        let roots = settings::folders(&roots)?;
        if roots.is_empty() {
            return Err(CliError::invalid("choose a folder to scan"));
        }
        let mut args: Vec<OsString> = vec!["scan".into(), "--json".into()];
        args.extend(roots.into_iter().map(PathBuf::into_os_string));
        s.run(args, &s.home, SCAN_DEADLINE)?
            .document("residue.scan/v1")
    })
    .await
}

/// `residue explain --json PATH`
#[tauri::command]
async fn explain(state: AppState<'_>, path: String) -> Result<Value, CliError> {
    blocking(&state, move |s| {
        let path = absolute(&path)?;
        s.run(
            vec!["explain".into(), "--json".into(), path.into_os_string()],
            &s.home,
            SCAN_DEADLINE,
        )?
        .document("residue.explain/v1")
    })
    .await
}

#[derive(Serialize)]
struct Removal {
    document: Value,
    exit_code: Option<i32>,
}

/// `residue remove --trash --json PATH...`, with `--yes` only when the user
/// confirmed. Without `--yes`, and with stdin that is not a terminal, residue
/// removes and purges nothing and prints its plan: each path checked again,
/// and the expired trash entries the run would purge. The confirmation dialog
/// shows that plan. With `--yes`, residue checks every path once more before
/// it moves it, and keeps any that changed.
fn remove(s: &State, paths: &[String], confirmed: bool) -> Result<Removal, CliError> {
    if paths.is_empty() {
        return Err(CliError::invalid("no worktrees were selected"));
    }
    let mut args: Vec<OsString> = vec!["remove".into(), "--trash".into(), "--json".into()];
    if confirmed {
        args.push("--yes".into());
    }
    for p in paths {
        args.push(absolute(p)?.into_os_string());
    }
    let out = s.run(args, &s.home, REMOVE_DEADLINE)?;
    // 0: every path went to the trash. 3: at least one stayed (or, for the
    // plan, nothing was confirmed), and the document says why. Anything else
    // is an error without a document.
    match out.code {
        Some(0 | 3) => Ok(Removal {
            document: out.document("residue.remove/v1")?,
            exit_code: out.code,
        }),
        _ => Err(out.error(ErrorKind::Failed, "residue remove failed".into())),
    }
}

/// The plan for moving `paths` to the trash. Removes nothing.
#[tauri::command]
async fn plan_removal(state: AppState<'_>, paths: Vec<String>) -> Result<Removal, CliError> {
    blocking(&state, move |s| remove(s, &paths, false)).await
}

/// Move the confirmed `paths` to their repositories' trash.
#[tauri::command]
async fn remove_to_trash(state: AppState<'_>, paths: Vec<String>) -> Result<Removal, CliError> {
    blocking(&state, move |s| remove(s, &paths, true)).await
}

#[derive(Serialize)]
struct Listing {
    /// The folder the listing ran in.
    root: String,
    document: Option<Value>,
    error: Option<CliError>,
}

/// `residue restore --json` in each folder: the trash of the repositories at
/// or under it. A folder that fails reports its error beside the others.
#[tauri::command]
async fn list_restorable(
    state: AppState<'_>,
    roots: Vec<String>,
) -> Result<Vec<Listing>, CliError> {
    blocking(&state, move |s| {
        let roots = settings::folders(&roots)?;
        Ok(roots
            .into_iter()
            .map(|root| {
                let result = s
                    .run(
                        vec!["restore".into(), "--json".into()],
                        &root,
                        RESTORE_DEADLINE,
                    )
                    .and_then(|out| out.document("residue.restore/v1"));
                let root = root.to_string_lossy().into_owned();
                match result {
                    Ok(doc) => Listing {
                        root,
                        document: Some(doc),
                        error: None,
                    },
                    Err(e) => Listing {
                        root,
                        document: None,
                        error: Some(e),
                    },
                }
            })
            .collect())
    })
    .await
}

/// `residue restore --json ENTRY_PATH`, run in the folder that listed it.
#[tauri::command]
async fn restore(state: AppState<'_>, entry_path: String, root: String) -> Result<Value, CliError> {
    blocking(&state, move |s| {
        let entry = absolute(&entry_path)?;
        let root = settings::folders(&[root])?
            .pop()
            .ok_or_else(|| CliError::invalid("no folder to restore from"))?;
        let out = s.run(
            vec!["restore".into(), "--json".into(), entry.into_os_string()],
            &root,
            RESTORE_DEADLINE,
        )?;
        if out.code != Some(0) {
            return Err(out.error(ErrorKind::Failed, "residue could not restore it".into()));
        }
        out.document("residue.restore/v1")
    })
    .await
}

#[tauri::command]
async fn service_status(state: AppState<'_>) -> Result<service::Status, CliError> {
    blocking(&state, |s| {
        let version = s.bundled_version()?;
        service::status(&s.service_context(&version), &s.dev)
    })
    .await
}

/// Turn the service on, or save other folders. `basis` is the one the window
/// showed (`Status.basis`); when a fresh read differs, nothing is installed
/// and the result is `changed`, with what changed and the fresh status.
#[tauri::command]
async fn service_install(
    state: AppState<'_>,
    roots: Vec<String>,
    dry_run: bool,
    basis: service::Basis,
) -> Result<service::Save, CliError> {
    blocking(&state, move |s| {
        let roots = settings::folders(&roots)?;
        let version = s.bundled_version()?;
        let ctx = s.service_context(&version);
        service::install(&ctx, &s.dev, &roots, dry_run, &basis)
    })
    .await
}

#[tauri::command]
async fn service_uninstall(state: AppState<'_>) -> Result<service::Change, CliError> {
    blocking(&state, |s| {
        let version = s.bundled_version()?;
        service::uninstall(&s.service_context(&version), &s.dev)
    })
    .await
}

/// `residue watch --once --dry-run ROOT...` with the options a save keeps:
/// what automatic cleanup would remove now. It removes nothing.
#[tauri::command]
async fn service_preview(
    state: AppState<'_>,
    roots: Vec<String>,
) -> Result<service::Preview, CliError> {
    blocking(&state, move |s| {
        let roots = settings::folders(&roots)?;
        let version = s.bundled_version()?;
        let ctx = s.service_context(&version);
        let current = service::status(&ctx, &s.dev)?;
        service::preview(&ctx, &roots, &current.plan.keep)
    })
    .await
}

fn absolute(path: &str) -> Result<PathBuf, CliError> {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        Ok(p)
    } else {
        Err(CliError::invalid(format!("{path} is not an absolute path")))
    }
}

/// The bundled residue binary. Tauri places each `externalBin` next to the
/// app's own executable without its target-triple suffix, in `tauri dev` and
/// in the bundle (`residue.app/Contents/MacOS/residue`).
fn sidecar() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find the app's executable: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "the app's executable has no parent directory".to_string())?;
    Ok(dir.join("residue"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let paths = app.path();
            let state = State {
                sidecar: sidecar()?,
                home: env::home().ok_or("HOME is not set")?,
                config_dir: paths.app_config_dir()?,
                data_dir: paths.app_local_data_dir()?,
                dev: service::DevPreview::from_env(),
            };
            app.manage(Arc::new(state));
            // Read the login shell's PATH while the window loads.
            std::thread::spawn(|| {
                env::path();
            });
            #[cfg(all(debug_assertions, target_os = "macos"))]
            devshot::start(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_settings,
            set_roots,
            scan,
            explain,
            plan_removal,
            remove_to_trash,
            list_restorable,
            restore,
            service_status,
            service_install,
            service_uninstall,
            service_preview,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("residue app: {e}");
        std::process::exit(1);
    }
}
