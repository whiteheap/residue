//! Run a child process with a deadline and a cap on captured output.
//!
//! Every external program residue starts (git, lsof, ps) goes through
//! [`run`], so a hung or chatty child can never hang residue itself, and
//! every child is started by [`spawn`].

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, PoisonError};
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

/// Keeps only the records of standard output that `keep` accepts, each
/// record ending with `separator`, so a caller that needs a few records of
/// a long output holds those and not the whole output (#78). `keep` sees
/// every record once, in order, so it can also remember what it has seen
/// (`git::status` notes which folders hold a tracked file, #83).
#[derive(Clone, Copy)]
pub struct Filter<K = fn(&[u8]) -> bool> {
    pub separator: u8,
    pub keep: K,
}

/// A [`Filter`] over output read in pieces: the record the last piece left
/// unfinished, and the records kept so far, each with its separator.
struct Records<K> {
    filter: Filter<K>,
    pending: Vec<u8>,
    kept: Vec<u8>,
}

impl<K: FnMut(&[u8]) -> bool> Records<K> {
    fn new(filter: Filter<K>) -> Self {
        Records {
            filter,
            pending: Vec::new(),
            kept: Vec::new(),
        }
    }

    fn feed(&mut self, mut piece: &[u8]) {
        while let Some(end) = piece.iter().position(|&b| b == self.filter.separator) {
            if self.pending.is_empty() {
                self.offer(&piece[..end]);
            } else {
                let mut record = std::mem::take(&mut self.pending);
                record.extend_from_slice(&piece[..end]);
                self.offer(&record);
                record.clear();
                self.pending = record;
            }
            piece = &piece[end + 1..];
        }
        self.pending.extend_from_slice(piece);
    }

    fn offer(&mut self, record: &[u8]) {
        if (self.filter.keep)(record) {
            self.kept.extend_from_slice(record);
            self.kept.push(self.filter.separator);
        }
    }

    /// The kept records. A last record without a separator is kept without
    /// one, so splitting the result gives the records that splitting the
    /// whole output would give, less the ones `keep` refused.
    fn finish(mut self) -> Vec<u8> {
        let last = std::mem::take(&mut self.pending);
        if !last.is_empty() && (self.filter.keep)(&last) {
            self.kept.extend_from_slice(&last);
        }
        self.kept
    }
}

pub fn run(cmd: &mut Command, timeout: Duration, max_output: usize) -> Result<Captured, RunError> {
    run_with_input(cmd, None, timeout, max_output)
}

/// [`run`], keeping only the records of standard output that `filter`
/// accepts. `max_output` still counts every byte the child writes, so a
/// filtered run fails with [`RunError::TooLarge`] exactly when [`run`] would.
pub fn run_filtered<K: FnMut(&[u8]) -> bool + Send + 'static>(
    cmd: &mut Command,
    timeout: Duration,
    max_output: usize,
    filter: Filter<K>,
) -> Result<Captured, RunError> {
    run_inner(cmd, None, timeout, max_output, Some(filter))
}

/// [`run`], with `input` written to the child's standard input, which is
/// then closed. Without input, standard input is `/dev/null`.
pub fn run_with_input(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
) -> Result<Captured, RunError> {
    run_inner::<fn(&[u8]) -> bool>(cmd, input, timeout, max_output, None)
}

/// Start `cmd`, one child at a time. On macOS, which has no `pipe2`, the
/// standard library makes a child's pipes with `pipe()` and marks them
/// close-on-exec in a second step. A child another thread starts between
/// the two steps inherits them, and then the first child's output ends only
/// when that other child, and anything it left running, exits (#114). Under
/// this lock no child starts while another start is making its pipes.
///
/// The lock covers only the children started here, so this is the one call
/// of `Command::spawn` in residue, and `clippy.toml` refuses `spawn`,
/// `output` and `status` anywhere else. A start holds the lock for the
/// `posix_spawn` or `fork` and `exec` alone, never while the child runs.
#[expect(
    clippy::disallowed_methods,
    reason = "the one place a child starts, under the lock"
)]
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    static STARTING: Mutex<()> = Mutex::new(());
    // The lock guards no data, so a start that panicked holding it left
    // nothing half done.
    let _starting = STARTING.lock().unwrap_or_else(PoisonError::into_inner);
    cmd.spawn()
}

fn run_inner<K: FnMut(&[u8]) -> bool + Send + 'static>(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    filter: Option<Filter<K>>,
) -> Result<Captured, RunError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = spawn(cmd).map_err(|e| {
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
    let stdout = drain(child.stdout.take(), max_output, filter);
    let stderr = drain::<_, fn(&[u8]) -> bool>(child.stderr.take(), max_output, None);
    let deadline = Instant::now() + timeout;
    // Each drain thread ends when its pipe closes, which happens when the
    // child exits, so this wakes at the exit and not on a timer (#82).
    let stdout = finish(&mut child, stdout, deadline, timeout)?;
    let stderr = finish(&mut child, stderr, deadline, timeout)?;
    let status = reap(&mut child, deadline, timeout)?;
    Ok(Captured {
        status,
        stdout: checked(stdout, max_output)?,
        stderr: checked(stderr, max_output)?,
    })
}

/// Wait for a drain thread until the deadline. A child that is still
/// running then is stopped. One that has exited while its pipe stays open
/// left a grandchild holding it, which is not waited for either.
fn finish(
    child: &mut Child,
    rx: Receiver<Stream>,
    deadline: Instant,
    timeout: Duration,
) -> Result<Stream, RunError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if let Ok(stream) = rx.recv_timeout(left) {
        return Ok(stream);
    }
    match child.try_wait() {
        Ok(Some(_)) => Err(RunError::Timeout(timeout)),
        Ok(None) => {
            stop(child);
            Err(RunError::Timeout(timeout))
        }
        Err(e) => {
            stop(child);
            Err(RunError::Wait(e))
        }
    }
}

/// Reap a child whose pipes have closed. It has almost always exited, but
/// the kernel closes its files a moment before it can be reaped, and a
/// child can close both pipes and keep running, so check at once, then
/// back off up to 20 ms until the deadline.
fn reap(child: &mut Child, deadline: Instant, timeout: Duration) -> Result<ExitStatus, RunError> {
    let mut pause = Duration::from_micros(50);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                stop(child);
                return Err(RunError::Timeout(timeout));
            }
            Ok(None) => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
            Err(e) => {
                stop(child);
                return Err(RunError::Wait(e));
            }
        }
    }
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
/// With a filter, only the records it keeps are held, and `max` still counts
/// every byte read.
fn drain<R: Read + Send + 'static, K: FnMut(&[u8]) -> bool + Send + 'static>(
    pipe: Option<R>,
    max: usize,
    filter: Option<Filter<K>>,
) -> Receiver<Stream> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = Stream {
            bytes: Vec::new(),
            overflow: false,
            error: None,
        };
        let mut records = filter.map(Records::new);
        let mut read = 0usize;
        if let Some(mut pipe) = pipe {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = max.saturating_sub(read);
                        if n > room {
                            stream.overflow = true;
                        }
                        let piece = &buf[..n.min(room)];
                        read += piece.len();
                        match records.as_mut() {
                            Some(records) => records.feed(piece),
                            None => stream.bytes.extend_from_slice(piece),
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        stream.error = Some(e);
                        break;
                    }
                }
            }
        }
        if let Some(records) = records {
            stream.bytes = records.finish();
        }
        // The receiver is gone only if run() already returned an error.
        drop(tx.send(stream));
    });
    rx
}

/// The bytes of a finished stream, or the read error or overflow it had.
fn checked(stream: Stream, max: usize) -> Result<Vec<u8>, RunError> {
    if let Some(e) = stream.error {
        return Err(RunError::Wait(e));
    }
    if stream.overflow {
        return Err(RunError::TooLarge(max));
    }
    Ok(stream.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The records `ls-files -v -z` marks as hiding edits.
    fn hidden(record: &[u8]) -> bool {
        record.len() >= 3
            && record[1] == b' '
            && (record[0].is_ascii_lowercase() || record[0] == b'S')
    }

    const FILTER: Filter = Filter {
        separator: 0,
        keep: hidden,
    };

    #[test]
    fn a_record_split_across_reads_is_kept_whole() {
        let input: &[u8] =
            b"H a/kept-out\0S skip worktree\0h assumed\0H x\0\0S\0S last-without-separator";
        let mut whole = Records::new(FILTER);
        whole.feed(input);
        let expected = whole.finish();
        assert_eq!(
            expected,
            b"S skip worktree\0h assumed\0S last-without-separator".to_vec()
        );
        for i in 0..=input.len() {
            for j in i..=input.len() {
                let mut records = Records::new(FILTER);
                records.feed(&input[..i]);
                records.feed(&input[i..j]);
                records.feed(&input[j..]);
                assert_eq!(records.finish(), expected, "reads split at {i} and {j}");
            }
        }
    }

    #[test]
    fn a_filtered_run_keeps_the_records_and_counts_every_byte() {
        let mut cmd = Command::new("printf");
        cmd.arg(r"H a\000S b\000H c\000");
        let out = run_filtered(&mut cmd, Duration::from_secs(10), 1 << 20, FILTER).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"S b\0".to_vec());

        let mut cmd = Command::new("printf");
        cmd.arg(r"S b\000H c\000");
        match run_filtered(&mut cmd, Duration::from_secs(10), 5, FILTER) {
            Err(RunError::TooLarge(5)) => {}
            other => panic!("expected TooLarge(5), got {:?}", other.map(|o| o.stdout)),
        }
    }

    fn shell(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[test]
    fn a_child_past_its_deadline_is_stopped() {
        let started = Instant::now();
        match run(&mut shell("sleep 30"), Duration::from_millis(200), 1 << 20) {
            Err(RunError::Timeout(_)) => {}
            other => panic!("expected Timeout, got {:?}", other.map(|o| o.status)),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_grandchild_holding_a_pipe_ends_at_the_deadline() {
        let started = Instant::now();
        match run(
            &mut shell("sleep 30 & exit 0"),
            Duration::from_millis(300),
            1 << 20,
        ) {
            Err(RunError::Timeout(_)) => {}
            other => panic!("expected Timeout, got {:?}", other.map(|o| o.status)),
        }
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn a_child_that_closes_its_pipes_is_still_waited_for() {
        let started = Instant::now();
        let out = run(
            &mut shell("exec >&- 2>&-; sleep 0.3; exit 3"),
            Duration::from_secs(20),
            1 << 20,
        )
        .unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert!(started.elapsed() >= Duration::from_millis(300));
    }
}
