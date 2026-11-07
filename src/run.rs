//! Run a child process with a deadline and a cap on captured output.
//!
//! Every external program residue starts (git, lsof, ps) goes through
//! [`run`], so a hung or chatty child can never hang residue itself.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum RunError {
    /// The program is not installed or not on PATH.
    NotFound,
    Spawn(io::Error),
    Timeout(Duration),
    TooLarge(usize),
    Wait(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::NotFound => write!(f, "program not found"),
            RunError::Spawn(e) => write!(f, "could not start: {e}"),
            RunError::Timeout(d) => write!(f, "timed out after {}s", d.as_secs()),
            RunError::TooLarge(n) => write!(f, "output exceeded {} MiB", n >> 20),
            RunError::Wait(e) => write!(f, "could not wait for it: {e}"),
        }
    }
}

struct Stream {
    bytes: Vec<u8>,
    overflow: bool,
    error: Option<io::Error>,
}

pub fn run(cmd: &mut Command, timeout: Duration, max_output: usize) -> Result<Captured, RunError> {
    run_with_input(cmd, None, timeout, max_output)
}

/// [`run`], with `input` written to the child's standard input, which is
/// then closed. Without input, standard input is `/dev/null`.
pub fn run_with_input(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
) -> Result<Captured, RunError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            RunError::NotFound
        } else {
            RunError::Spawn(e)
        }
    })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // On its own thread, so a child that writes a lot before it reads
        // cannot deadlock against this one. A write error means the child
        // closed its input early, which it does only when it exits; its exit
        // status, which the caller checks, carries that failure.
        thread::spawn(move || drop(stdin.write_all(&input)));
    }
    let stdout = drain(child.stdout.take(), max_output);
    let stderr = drain(child.stderr.take(), max_output);
    let deadline = Instant::now() + timeout;
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                stop(&mut child);
                return Err(RunError::Timeout(timeout));
            }
            Ok(None) => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
            Err(e) => {
                stop(&mut child);
                return Err(RunError::Wait(e));
            }
        }
    };
    let stdout = collect(stdout, deadline, timeout, max_output)?;
    let stderr = collect(stderr, deadline, timeout, max_output)?;
    Ok(Captured {
        status,
        stdout,
        stderr,
    })
}

/// Kill and reap a child that overran its deadline. A child that leads its
/// own process group (one started in a new session, as `git::fetch` does)
/// is killed with its whole group, so a program it started, such as SSH,
/// does not outlive it holding the output pipes open.
fn stop(child: &mut Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain system calls on integers. The child is not reaped
        // yet, so its pid, and the group it leads, cannot be reused.
        // kill fails only when the group is already gone; the kill below
        // still runs.
        unsafe {
            if libc::getpgid(pid) == pid {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    // kill() fails only when the child has already exited, and wait() then
    // reaps it at once. Both results are irrelevant here: the caller is
    // already reporting the timeout or wait error that made it stop the child.
    let killed = child.kill();
    let reaped = child.wait();
    drop((killed, reaped));
}

/// Read a pipe on its own thread. Bytes past `max` are read and discarded so
/// the child never blocks on a full pipe; the overflow is reported instead.
fn drain<R: Read + Send + 'static>(pipe: Option<R>, max: usize) -> Receiver<Stream> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = Stream {
            bytes: Vec::new(),
            overflow: false,
            error: None,
        };
        if let Some(mut pipe) = pipe {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = max.saturating_sub(stream.bytes.len());
                        if n > room {
                            stream.overflow = true;
                        }
                        stream.bytes.extend_from_slice(&buf[..n.min(room)]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        stream.error = Some(e);
                        break;
                    }
                }
            }
        }
        // The receiver is gone only if run() already returned an error.
        drop(tx.send(stream));
    });
    rx
}

fn collect(
    rx: Receiver<Stream>,
    deadline: Instant,
    timeout: Duration,
    max: usize,
) -> Result<Vec<u8>, RunError> {
    // A grandchild can inherit the pipe and keep it open after the child
    // exits, so wait for the reader only until the original deadline.
    let left = deadline.saturating_duration_since(Instant::now());
    let stream = rx
        .recv_timeout(left.max(Duration::from_millis(100)))
        .map_err(|_| RunError::Timeout(timeout))?;
    if let Some(e) = stream.error {
        return Err(RunError::Wait(e));
    }
    if stream.overflow {
        return Err(RunError::TooLarge(max));
    }
    Ok(stream.bytes)
}
