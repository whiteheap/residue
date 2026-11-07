//! Running residue: an explicit argv, a known PATH, a deadline, and the output
//! read back as one of residue's JSON documents. The documents are the
//! contract between the CLI and this app; nothing here decides what is safe
//! to remove.

use serde::Serialize;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How much of stdout or stderr is kept. A scan of a few hundred worktrees is
/// a few megabytes; past this the output is cut and fails to parse, which is
/// reported, never guessed around.
const OUTPUT_CAP: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The binary is not there, or the system refused to start it.
    Missing,
    /// It ran and exited without the document it owes.
    Failed,
    /// It did not finish before the deadline, so the app stopped it.
    Timeout,
    /// It printed something that is not the expected document.
    BadOutput,
    /// The app refused the request before running anything.
    Invalid,
}

/// An error the window shows as it is: what failed, the command, and what
/// residue printed on stderr.
#[derive(Debug, Clone, Serialize)]
pub struct CliError {
    pub kind: ErrorKind,
    pub message: String,
    pub command: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
}

impl CliError {
    pub fn invalid(message: impl Into<String>) -> Self {
        CliError {
            kind: ErrorKind::Invalid,
            message: message.into(),
            command: None,
            stderr: None,
            exit_code: None,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        CliError {
            kind: ErrorKind::Failed,
            message: message.into(),
            command: None,
            stderr: None,
            exit_code: None,
        }
    }
}

/// What a finished run printed.
pub struct Output {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

/// One residue run.
pub struct Run<'a> {
    pub bin: &'a Path,
    pub args: Vec<OsString>,
    pub cwd: &'a Path,
    pub path_env: &'a OsStr,
    pub deadline: Duration,
}

impl Run<'_> {
    pub fn output(&self) -> Result<Output, CliError> {
        let command = shown(self.bin, &self.args);
        let mut child = Command::new(self.bin)
            .args(&self.args)
            .current_dir(self.cwd)
            .env("PATH", self.path_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CliError {
                kind: ErrorKind::Missing,
                message: format!("cannot start {}: {e}", self.bin.display()),
                command: Some(command.clone()),
                stderr: None,
                exit_code: None,
            })?;

        let stdout = child.stdout.take().map(reader);
        let stderr = child.stderr.take().map(reader);
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= self.deadline => {
                    // Stop it: a hung child would hold the window's request
                    // forever. Both calls fail only if it already exited.
                    let killed = child.kill();
                    let waited = child.wait();
                    return Err(CliError {
                        kind: ErrorKind::Timeout,
                        message: format!(
                            "residue did not finish within {} s and was stopped{}",
                            self.deadline.as_secs(),
                            match (killed, waited) {
                                (Ok(()), Ok(_)) => String::new(),
                                (k, w) => format!(" ({k:?}, {w:?})"),
                            }
                        ),
                        command: Some(command),
                        stderr: stderr.map(join_reader).filter(|s| !s.is_empty()),
                        exit_code: None,
                    });
                }
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(e) => {
                    return Err(CliError {
                        kind: ErrorKind::Failed,
                        message: format!("cannot wait for residue: {e}"),
                        command: Some(command),
                        stderr: None,
                        exit_code: None,
                    });
                }
            }
        };
        Ok(Output {
            command,
            stdout: stdout.map(join_reader).unwrap_or_default(),
            stderr: stderr.map(join_reader).unwrap_or_default(),
            code: status.code(),
        })
    }
}

type Reader = thread::JoinHandle<std::io::Result<String>>;

fn reader(stream: impl Read + Send + 'static) -> Reader {
    thread::spawn(move || {
        let mut text = Vec::new();
        stream.take(OUTPUT_CAP).read_to_end(&mut text)?;
        Ok(String::from_utf8_lossy(&text).into_owned())
    })
}

/// The text a reader thread collected. A thread that failed to read or
/// panicked says so in the text, so the failure reaches the window.
fn join_reader(handle: Reader) -> String {
    match handle.join() {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => format!("(could not read the output: {e})"),
        Err(_) => "(the thread reading the output stopped)".to_string(),
    }
}

impl Output {
    /// The JSON document residue printed, when its `schema` is `schema`.
    /// residue prints one on success and on the exit statuses that still
    /// report (remove's 3, a restore listing that missed a path). Anything
    /// else is an error carrying stderr.
    pub fn document(&self, schema: &str) -> Result<Value, CliError> {
        let trimmed = self.stdout.trim();
        if trimmed.is_empty() {
            return Err(self.error(
                ErrorKind::Failed,
                match self.code {
                    Some(code) => {
                        format!("residue exited with status {code} and printed no report")
                    }
                    None => "residue was stopped by a signal and printed no report".to_string(),
                },
            ));
        }
        let doc: Value = serde_json::from_str(trimmed).map_err(|e| {
            self.error(
                ErrorKind::BadOutput,
                format!("residue printed something that is not JSON ({e})"),
            )
        })?;
        match doc.get("schema").and_then(Value::as_str) {
            Some(s) if s == schema => Ok(doc),
            other => Err(self.error(
                ErrorKind::BadOutput,
                format!(
                    "expected a {schema} document, residue printed {}",
                    other.unwrap_or("a document without a schema")
                ),
            )),
        }
    }

    pub fn error(&self, kind: ErrorKind, message: String) -> CliError {
        let stderr = self.stderr.trim();
        CliError {
            kind,
            message,
            command: Some(self.command.clone()),
            stderr: (!stderr.is_empty()).then(|| stderr.to_string()),
            exit_code: self.code,
        }
    }
}

/// The command line, quoted for a shell, as residue shows its own commands.
pub fn shown(bin: &Path, args: &[OsString]) -> String {
    std::iter::once(bin.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|a| {
            let a = a.to_string_lossy();
            if !a.is_empty()
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-%=:".contains(&b))
            {
                a.into_owned()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
