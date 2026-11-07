//! Development builds only: pictures of the window's web content, for
//! checking the UI from a terminal that macOS does not allow to record the
//! screen (where `screencapture` fails with "could not create image").
//!
//! Run a debug build with `RESIDUE_DESKTOP_DEV_SNAPSHOT_DIR=DIR`, then write a
//! name into `DIR/request`; the app writes `DIR/NAME.tiff` (Retina scale) and
//! deletes the request. `sips -s format png DIR/NAME.tiff --out DIR/NAME.png`
//! converts it. Only macOS debug builds contain this module.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub fn start(app: &AppHandle) {
    let Some(dir) = std::env::var_os("RESIDUE_DESKTOP_DEV_SNAPSHOT_DIR").map(PathBuf::from) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if let Err(e) = take_request(&app, &dir) {
                eprintln!("residue desktop snapshot: {e}");
            }
        }
    });
}

fn take_request(app: &AppHandle, dir: &Path) -> Result<(), String> {
    let request = dir.join("request");
    let name = match std::fs::read_to_string(&request) {
        Ok(name) => name.trim().to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read {}: {e}", request.display())),
    };
    std::fs::remove_file(&request)
        .map_err(|e| format!("cannot remove {}: {e}", request.display()))?;
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(format!("{name:?} is not a file name"));
    }
    let out = dir.join(format!("{name}.tiff"));
    let window = app
        .get_webview_window("main")
        .ok_or("the main window is gone")?;
    window
        .with_webview(move |webview| snapshot(webview.inner(), out))
        .map_err(|e| format!("cannot reach the web view: {e}"))
}

fn snapshot(webview: *mut std::ffi::c_void, out: PathBuf) {
    use block2::RcBlock;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSError;
    use objc2_web_kit::WKWebView;

    // SAFETY: on macOS, Tauri's `PlatformWebview::inner` is the window's
    // WKWebView, and `with_webview` runs this on the main thread while the
    // web view is alive.
    let wk: &WKWebView = unsafe { &*webview.cast::<WKWebView>() };
    let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        // SAFETY: WebKit passes either a valid image or a valid error.
        let Some(image) = (unsafe { image.as_ref() }) else {
            let why = unsafe { error.as_ref() }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "no image and no error".into());
            eprintln!("residue desktop snapshot: {why}");
            return;
        };
        let Some(tiff) = image.TIFFRepresentation() else {
            eprintln!("residue desktop snapshot: the image has no TIFF form");
            return;
        };
        match std::fs::write(&out, tiff.to_vec()) {
            Ok(()) => eprintln!("residue desktop snapshot: wrote {}", out.display()),
            Err(e) => eprintln!(
                "residue desktop snapshot: cannot write {}: {e}",
                out.display()
            ),
        }
    });
    // SAFETY: a nil configuration snapshots the whole visible web view.
    unsafe { wk.takeSnapshotWithConfiguration_completionHandler(None, &done) };
}
