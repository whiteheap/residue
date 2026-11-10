//! The Cargo build folder a worktree's own config names outside the
//! worktree (#190): `build.target-dir` in its `.cargo/config` or
//! `.cargo/config.toml`. A removal deletes the worktree and that config with
//! it, and leaves the folder behind with nothing pointing at it, so residue
//! names the folder and its size. It never removes, moves or locks it, never
//! gives its path to Git, and it changes no check and no verdict.
//!
//! What is read, and only that:
//!
//! - The worktree's own `.cargo/config`, or `.cargo/config.toml` when there
//!   is no `.cargo/config`: Cargo uses the file without the extension when
//!   both exist. Not a config in a folder above the worktree or below it,
//!   `$CARGO_HOME/config.toml`, `CARGO_TARGET_DIR` or `--target-dir`, which
//!   a shell or another folder sets and residue cannot know.
//! - `build.target-dir` as a one-line string, in the forms TOML gives that
//!   key: under `[build]`, as the dotted `build.target-dir`, or in an inline
//!   `build = { ... }`, with bare or quoted keys. Anything else that could
//!   set it, a config residue cannot parse, one that names other config
//!   files with `include` and does not set it itself, is "not read", never
//!   taken as no folder.
//! - A relative path is resolved against the folder that holds `.cargo`,
//!   the worktree, as Cargo resolves a path in a config file. A folder inside
//!   the worktree is not named: the walk already counts it, and it goes with
//!   the worktree.
//!
//! The path comes from a file a cloned repository can hold, so it can name
//! any folder, `/` included. residue measures the folder only when it holds
//! the `CACHEDIR.TAG` Cargo writes in a build folder it makes, with the
//! walk that sizes a worktree (`fsinfo::measure`), and looks up its path
//! only on the filesystems the worktree's own path is on (`fsinfo::read_in`).
//! A folder it does not measure is "not measured", never empty.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::activity::{Evidence, Snapshot};
use crate::fsinfo::{self, Read, Usage};

/// The key, as its path of TOML keys.
const KEY: [&str; 2] = ["build", "target-dir"];

/// The largest config residue reads. Cargo configs are a few hundred bytes.
const CONFIG_LIMIT: usize = 256 * 1024;

/// The largest `CACHEDIR.TAG` residue reads: Cargo's is 177 bytes.
const TAG_LIMIT: usize = 4096;

/// The deepest residue reads arrays and inline tables inside each other.
/// The config comes from the repository, and each level is a call, so a
/// file of nested `[` must not run the stack out (#190).
const NEST_LIMIT: usize = 64;

/// The first two lines of the `CACHEDIR.TAG` Cargo writes in a build folder
/// it creates (`cargo_util::paths::exclude_from_backups`, Cargo 1.94.0).
/// The signature line alone is every tool's; the second says Cargo wrote
/// it.
const CARGO_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n";

/// The build folder a worktree's config names outside it, or why residue
/// could not read that config.
#[derive(Clone)]
pub struct TargetDir {
    /// The config file residue read: `.cargo/config` or
    /// `.cargo/config.toml` in the worktree.
    pub config: PathBuf,
    /// The folder, absolute, or why residue could not read the config with
    /// certainty.
    pub path: Result<PathBuf, String>,
    pub size: Size,
}

#[derive(Clone)]
pub enum Size {
    /// Not measured by this command: `scan` names the folder, `explain` and
    /// `remove` measure it.
    NotRequested,
    /// Nothing is at the path.
    Missing,
    /// Walked like a worktree. `activity` is the run's snapshot for it,
    /// which shows a process using it, or does not.
    Measured {
        usage: Box<Usage>,
        activity: Evidence,
    },
    /// Not walked, for `reason` (a code) and `detail` (in words).
    NotMeasured {
        reason: &'static str,
        detail: String,
    },
}

impl Size {
    pub fn state(&self) -> &'static str {
        match self {
            Size::NotRequested => "not_measured",
            Size::Missing => "missing",
            Size::Measured { .. } => "measured",
            Size::NotMeasured { .. } => "not_measured",
        }
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Size::NotRequested => Some("not_requested"),
            Size::NotMeasured { reason, .. } => Some(reason),
            Size::Missing | Size::Measured { .. } => None,
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            Size::NotRequested => {
                Some("this command does not measure it; residue explain does".into())
            }
            Size::NotMeasured { detail, .. } => Some(detail.clone()),
            Size::Missing | Size::Measured { .. } => None,
        }
    }
}

/// The build folder the worktree at `worktree` (canonical) names outside
/// itself, or why its config could not be read; `None` when it has no
/// config, or its config names no folder or one inside it. Reads at most
/// two small files, and only when `.cargo` exists: one lstat otherwise.
pub fn find(worktree: &Path) -> Option<TargetDir> {
    let folder = worktree.join(".cargo");
    if fs::symlink_metadata(&folder).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return None;
    }
    let not_read = |config: PathBuf, why: String| {
        Some(TargetDir {
            config,
            path: Err(why),
            size: Size::NotRequested,
        })
    };
    let devices = match devices(worktree) {
        Ok(devices) => devices,
        Err(e) => return not_read(folder, e),
    };
    let read = |name: &str| fsinfo::read_in(&folder, name, &devices, CONFIG_LIMIT);
    let (config, bytes) = match read("config") {
        Ok(Read::File(bytes, _)) => (folder.join("config"), bytes),
        Ok(Read::NoFolder | Read::NoFile) => match read("config.toml") {
            Ok(Read::File(bytes, _)) => (folder.join("config.toml"), bytes),
            Ok(Read::NoFolder | Read::NoFile) => return None,
            Err(e) => return not_read(folder.join("config.toml"), e.to_string()),
        },
        Err(e) => return not_read(folder.join("config"), e.to_string()),
    };
    let value = match target_dir(&bytes) {
        Ok(Some(value)) => value,
        Ok(None) => return None,
        Err(why) => return not_read(config, why),
    };
    if value.contains('\0') {
        return not_read(config, "its build.target-dir holds a NUL byte".into());
    }
    match resolve(worktree, &value) {
        Resolved::Inside => None,
        Resolved::Outside(path) => Some(TargetDir {
            config,
            path: Ok(path),
            size: Size::NotRequested,
        }),
        Resolved::Unsure(path) => Some(TargetDir {
            config,
            path: Ok(path),
            size: Size::NotMeasured {
                reason: "unresolved",
                detail: "its path has `..` after a folder name, which residue does not resolve"
                    .into(),
            },
        }),
    }
}

impl TargetDir {
    /// Measures the folder, once, for `explain` and `remove`. It walks it
    /// only when it holds Cargo's `CACHEDIR.TAG`, and drops the folder when
    /// its real path turns out to be inside the worktree after all (a path
    /// spelled in another case, on a filesystem that ignores case).
    pub fn measure(mut self, worktree: &Path, snapshot: &Snapshot) -> Option<TargetDir> {
        let Ok(path) = &self.path else {
            return Some(self);
        };
        if !matches!(self.size, Size::NotRequested) {
            return Some(self);
        }
        let not_measured = |reason, detail: String| Size::NotMeasured { reason, detail };
        if worktree.starts_with(path) {
            self.size = not_measured("holds_worktree", "it holds the worktree".into());
            return Some(self);
        }
        let devices = match devices(worktree) {
            Ok(devices) => devices,
            Err(e) => {
                self.size = not_measured("unreadable", e);
                return Some(self);
            }
        };
        let (tag, folder) = match fsinfo::read_in(path, "CACHEDIR.TAG", &devices, TAG_LIMIT) {
            Ok(Read::NoFolder) => {
                self.size = Size::Missing;
                return Some(self);
            }
            Ok(Read::NoFile) => {
                self.size = not_measured(
                    "no_cargo_tag",
                    "it holds no CACHEDIR.TAG, which Cargo writes in a build folder".into(),
                );
                return Some(self);
            }
            Ok(Read::File(tag, folder)) => (tag, folder),
            Err(e) => {
                self.size = not_measured("unreadable", e.to_string());
                return Some(self);
            }
        };
        if !tag.starts_with(CARGO_TAG) {
            self.size = not_measured(
                "no_cargo_tag",
                "its CACHEDIR.TAG is not the one Cargo writes".into(),
            );
            return Some(self);
        }
        // Its real path: `read_in` found no symbolic link on it, so this
        // differs only in the case of a name, where the filesystem ignores
        // case, and looks up nothing `read_in` did not.
        let real = match fs::canonicalize(path) {
            Ok(real) => real,
            Err(e) => {
                self.size = not_measured("unreadable", e.to_string());
                return Some(self);
            }
        };
        if real.starts_with(worktree) {
            return None;
        }
        let mut wanted = snapshot.wanted();
        wanted.push(folder);
        wanted.sort_unstable();
        wanted.dedup();
        self.size = match fsinfo::measure(&real, &[], &|_| false, &|_| false, &wanted) {
            Err(e) => not_measured("unreadable", e.to_string()),
            // The walk opens the folder itself: one that differs from the
            // folder the tag was read in replaced it meanwhile.
            Ok(usage) if !usage.reach.found.contains(&folder) => not_measured(
                "changed",
                "another folder took its place while residue read it".into(),
            ),
            Ok(usage) => Size::Measured {
                activity: snapshot.evidence(&real, Some(&usage.reach)),
                usage: Box::new(usage),
            },
        };
        self.path = Ok(real);
        Some(self)
    }
}

/// The devices of the folders on the worktree's path, `/` and the worktree
/// included: the filesystems residue already looks up names on to reach it.
fn devices(worktree: &Path) -> Result<Vec<u64>, String> {
    let mut devices = Vec::new();
    for folder in worktree.ancestors() {
        let device = fs::symlink_metadata(folder)
            .map_err(|e| format!("{}: {e}", folder.display()))?
            .dev();
        if !devices.contains(&device) {
            devices.push(device);
        }
    }
    Ok(devices)
}

#[derive(Debug, PartialEq)]
enum Resolved {
    Inside,
    Outside(PathBuf),
    /// A `..` follows a name the value gave: where it leads depends on
    /// whether that name is a symbolic link, so residue does not say.
    Unsure(PathBuf),
}

/// `value` resolved against `worktree` (canonical), by its names: a `..`
/// right after the worktree's own path takes back one of its names, which
/// are folders; one after a name the value gave is `Unsure`.
fn resolve(worktree: &Path, value: &str) -> Resolved {
    let given = Path::new(value);
    let mut path = worktree.to_path_buf();
    let mut added = 0usize;
    for part in given.components() {
        match part {
            Component::RootDir => {
                path = PathBuf::from("/");
                added = 0;
            }
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir if added > 0 => return Resolved::Unsure(worktree.join(given)),
            Component::ParentDir => {
                path.pop();
            }
            Component::Normal(name) => {
                path.push(name);
                added += 1;
            }
        }
    }
    if path.starts_with(worktree) {
        Resolved::Inside
    } else {
        Resolved::Outside(path)
    }
}

/// What the config sets `build.target-dir` to: `Ok(None)` when it does not
/// set it, an `Err` saying why when residue cannot tell with certainty.
fn target_dir(text: &[u8]) -> Result<Option<String>, String> {
    let text = std::str::from_utf8(text).map_err(|_| "it is not UTF-8 text".to_string())?;
    let mut parser = Parser {
        s: text.as_bytes(),
        i: 0,
        table: Vec::new(),
        found: None,
        include: false,
        depth: 0,
    };
    parser.document()?;
    match (parser.found, parser.include) {
        (Some(value), _) => Ok(Some(value)),
        (None, true) => {
            Err("it names other config files with `include`, which residue does not read".into())
        }
        (None, false) => Ok(None),
    }
}

/// A reader of the TOML a Cargo config is written in, enough to find one
/// key with certainty: it reads every line, so a key in a form it does not
/// know, or text that is not TOML, is an error and never skipped.
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    /// The keys of the current `[table]`.
    table: Vec<String>,
    found: Option<String>,
    /// The file sets `include` (more config files, which residue does not
    /// read and whose `build.target-dir` the file's own overrides).
    include: bool,
    /// How many arrays and inline tables the reader is inside.
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn at(&self, text: &str) -> bool {
        self.s[self.i..].starts_with(text.as_bytes())
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn line(&self) -> usize {
        1 + self.s[..self.i].iter().filter(|&&b| b == b'\n').count()
    }

    fn unexpected(&self) -> String {
        format!("residue does not read its line {} as TOML", self.line())
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    /// A newline, `\n` or `\r\n`.
    fn newline(&mut self) -> bool {
        if self.eat(b'\n') {
            return true;
        }
        if self.at("\r\n") {
            self.i += 2;
            return true;
        }
        false
    }

    /// From `#` to the end of the line. TOML allows no control character in
    /// a comment but a tab.
    fn comment(&mut self) -> Result<(), String> {
        while let Some(c) = self.peek() {
            if c == b'\n' || self.at("\r\n") {
                break;
            }
            if (c < 0x20 && c != b'\t') || c == 0x7f {
                return Err(self.unexpected());
            }
            self.i += 1;
        }
        Ok(())
    }

    /// Spaces, a comment, then the end of the line or of the file.
    fn line_end(&mut self) -> Result<(), String> {
        self.spaces();
        if self.eat(b'#') {
            self.comment()?;
        }
        if self.peek().is_none() || self.newline() {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// Spaces, newlines and comments, inside an array or an inline table.
    fn blank(&mut self) -> Result<(), String> {
        loop {
            self.spaces();
            if self.eat(b'#') {
                self.comment()?;
            }
            if !self.newline() {
                return Ok(());
            }
        }
    }

    fn document(&mut self) -> Result<(), String> {
        loop {
            self.spaces();
            match self.peek() {
                None => return Ok(()),
                Some(b'\n' | b'\r' | b'#') => self.line_end()?,
                Some(b'[') => self.header()?,
                Some(_) => self.pair()?,
            }
        }
    }

    /// `[table]` or `[[array.of.tables]]`.
    fn header(&mut self) -> Result<(), String> {
        let line = self.line();
        self.i += 1;
        let array = self.eat(b'[');
        self.spaces();
        let path = self.key()?;
        if !self.eat(b']') || (array && !self.eat(b']')) {
            return Err(self.unexpected());
        }
        self.line_end()?;
        if path.first().is_some_and(|k| k == "include") {
            self.include = true;
        }
        // `build.target-dir` as a table, or `build` as an array of tables:
        // not a string, which Cargo refuses, and not one residue reads.
        if starts_with(&path, &KEY) || (array && starts_with(&KEY, &path)) {
            return Err(format!("its line {line} makes {} a table", path.join(".")));
        }
        self.table = path;
        Ok(())
    }

    /// `key = value`.
    fn pair(&mut self) -> Result<(), String> {
        let key = self.key()?;
        if !self.eat(b'=') {
            return Err(self.unexpected());
        }
        self.spaces();
        let mut path = self.table.clone();
        path.extend(key);
        if path == ["include"] {
            self.include = true;
        }
        self.value(Some(&path))?;
        self.line_end()
    }

    /// A dotted key, and the spaces after it.
    fn key(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            let name = match self.peek() {
                Some(b'"') if !self.at("\"\"\"") => self.basic()?,
                Some(b'\'') if !self.at("'''") => self.literal()?,
                _ => {
                    let start = self.i;
                    while matches!(
                        self.peek(),
                        Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
                    ) {
                        self.i += 1;
                    }
                    if self.i == start {
                        return Err(self.unexpected());
                    }
                    String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
                }
            };
            path.push(name);
            self.spaces();
            if !self.eat(b'.') {
                return Ok(path);
            }
            self.spaces();
        }
    }

    /// A value, whose full key is `path` (`None` in an array): the one
    /// residue looks for is kept, any other only read past.
    fn value(&mut self, path: Option<&[String]>) -> Result<(), String> {
        if let Some(path) = path {
            if path == KEY {
                if self.found.is_some() {
                    return Err("it sets build.target-dir twice".into());
                }
                let value = match self.peek() {
                    Some(b'"') if !self.at("\"\"\"") => self.basic()?,
                    Some(b'\'') if !self.at("'''") => self.literal()?,
                    _ => {
                        return Err(format!(
                            "its line {} sets build.target-dir to something other than a one-line string",
                            self.line()
                        ));
                    }
                };
                self.found = Some(value);
                return Ok(());
            }
            if starts_with(path, &KEY) {
                return Err(format!(
                    "its line {} sets a key below build.target-dir",
                    self.line()
                ));
            }
            // `build = { target-dir = "..." }`.
            if starts_with(&KEY, path) {
                if self.peek() != Some(b'{') {
                    return Err(format!(
                        "its line {} sets {} to something other than a table",
                        self.line(),
                        path.join(".")
                    ));
                }
                return self.inline_table(Some(path));
            }
        }
        match self.peek() {
            Some(b'"') if self.at("\"\"\"") => self.multiline(b'"'),
            Some(b'"') => self.basic().map(drop),
            Some(b'\'') if self.at("'''") => self.multiline(b'\''),
            Some(b'\'') => self.literal().map(drop),
            Some(b'[') => self.array(),
            // No key below an unrelated one is build.target-dir.
            Some(b'{') => self.inline_table(None),
            _ => self.scalar(),
        }
    }

    /// One level deeper, refused past [`NEST_LIMIT`].
    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > NEST_LIMIT {
            return Err(format!(
                "its line {} nests arrays or tables more than {NEST_LIMIT} deep",
                self.line()
            ));
        }
        Ok(())
    }

    fn array(&mut self) -> Result<(), String> {
        self.i += 1;
        self.enter()?;
        loop {
            self.blank()?;
            if self.eat(b']') {
                break;
            }
            self.value(None)?;
            self.blank()?;
            if self.eat(b']') {
                break;
            }
            if !self.eat(b',') {
                return Err(self.unexpected());
            }
        }
        self.depth -= 1;
        Ok(())
    }

    /// `{ key = value, ... }`, whose own key is `path`. Newlines and a
    /// trailing comma, which TOML 1.1 allows, are read too.
    fn inline_table(&mut self, path: Option<&[String]>) -> Result<(), String> {
        self.i += 1;
        self.enter()?;
        loop {
            self.blank()?;
            if self.eat(b'}') {
                break;
            }
            let key = self.key()?;
            if !self.eat(b'=') {
                return Err(self.unexpected());
            }
            self.spaces();
            let full = path.map(|p| {
                let mut full = p.to_vec();
                full.extend(key);
                full
            });
            self.value(full.as_deref())?;
            self.blank()?;
            if self.eat(b'}') {
                break;
            }
            if !self.eat(b',') {
                return Err(self.unexpected());
            }
        }
        self.depth -= 1;
        Ok(())
    }

    /// A number, a boolean or a date: the characters TOML allows in one,
    /// and a date's space before its time.
    fn scalar(&mut self) -> Result<(), String> {
        let token = |p: &mut Self| {
            let start = p.i;
            while matches!(
                p.peek(),
                Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'+' | b'.' | b':')
            ) {
                p.i += 1;
            }
            p.i - start
        };
        let start = self.i;
        if token(self) == 0 {
            return Err(self.unexpected());
        }
        // `1979-05-27 07:32:00`: a date, a space, a time.
        let date = &self.s[start..self.i];
        if date.len() == 10
            && date[4] == b'-'
            && self.peek() == Some(b' ')
            && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit)
        {
            self.i += 1;
            token(self);
        }
        Ok(())
    }

    /// A one-line `"basic string"`, with its escapes.
    fn basic(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.unexpected());
            };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| self.unexpected()),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.unexpected());
                    };
                    self.i += 1;
                    let ch = match e {
                        b'b' => '\u{8}',
                        b't' => '\t',
                        b'n' => '\n',
                        b'f' => '\u{c}',
                        b'r' => '\r',
                        b'e' => '\u{1b}',
                        b'"' => '"',
                        b'\\' => '\\',
                        b'x' => self.hex(2)?,
                        b'u' => self.hex(4)?,
                        b'U' => self.hex(8)?,
                        _ => return Err(self.unexpected()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if (c < 0x20 && c != b'\t') || c == 0x7f => return Err(self.unexpected()),
                c => out.push(c),
            }
        }
    }

    fn hex(&mut self, digits: usize) -> Result<char, String> {
        let text = self
            .s
            .get(self.i..self.i + digits)
            .and_then(|h| std::str::from_utf8(h).ok())
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.unexpected())?;
        self.i += digits;
        u32::from_str_radix(text, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| self.unexpected())
    }

    /// A one-line `'literal string'`.
    fn literal(&mut self) -> Result<String, String> {
        self.i += 1;
        let start = self.i;
        loop {
            match self.peek() {
                None => return Err(self.unexpected()),
                Some(b'\'') => break,
                Some(c) if (c < 0x20 && c != b'\t') || c == 0x7f => {
                    return Err(self.unexpected());
                }
                Some(_) => self.i += 1,
            }
        }
        let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.i += 1;
        Ok(text)
    }

    /// A `"""multi-line"""` or `'''multi-line'''` string, read past. Up to
    /// two quotes more before the closing three belong to the string.
    fn multiline(&mut self, quote: u8) -> Result<(), String> {
        self.i += 3;
        loop {
            match self.peek() {
                None => return Err(self.unexpected()),
                Some(b'\\') if quote == b'"' => self.i += 2,
                Some(c) if c == quote => {
                    let run = self.s[self.i..].iter().take_while(|&&b| b == quote).count();
                    self.i += run;
                    if run >= 3 {
                        return if run <= 5 {
                            Ok(())
                        } else {
                            Err(self.unexpected())
                        };
                    }
                }
                Some(_) => self.i += 1,
            }
        }
    }
}

/// Whether `path` begins with the keys `prefix`.
fn starts_with<A: PartialEq<B>, B>(path: &[A], prefix: &[B]) -> bool {
    path.len() >= prefix.len() && path.iter().zip(prefix).all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    //! The config reader, on config text written here from the TOML and
    //! Cargo documentation: each form of the key it reads, each form it
    //! must refuse rather than skip, and how a relative path resolves.

    use super::*;

    fn read(text: &str) -> Result<Option<String>, String> {
        target_dir(text.as_bytes())
    }

    fn found(text: &str) -> String {
        match read(text) {
            Ok(Some(value)) => value,
            other => panic!("{text:?} gave {other:?}"),
        }
    }

    #[test]
    fn reads_each_form_toml_gives_the_key() {
        let forms = [
            "[build]\ntarget-dir = \"/b/x\"\n",
            "[build]\ntarget-dir='/b/x'",
            "build.target-dir = \"/b/x\"\n",
            "build . target-dir = \"/b/x\" # a comment\n",
            "\"build\".\"target-dir\" = \"/b/x\"\n",
            "[ \"build\" ]\n'target-dir' = \"/b/x\"\n",
            "build = { target-dir = \"/b/x\", jobs = 2 }\n",
            "build = {\n  jobs = 2,\n  target-dir = \"/b/x\",\n}\n",
            // A quoted key with an escape is the same key.
            "[build]\n\"target\\u002Ddir\" = \"/b/x\"\n",
            // A value with escapes.
            "[build]\ntarget-dir = \"/b\\u002fx\"\n",
        ];
        for text in forms {
            assert_eq!(found(text), "/b/x", "{text:?}");
        }
        assert_eq!(found("[build]\r\ntarget-dir = \"../t\"\r\n"), "../t");
    }

    #[test]
    fn reads_past_everything_else_in_a_cargo_config() {
        // The overview in Cargo's configuration reference, abridged, with the
        // key set at the end.
        let text = r#"paths = ["/path/to/override"] # path dependency overrides

[alias]     # command aliases
b = "build"
space_example = ["run", "--release", "--", "\"command list\""]

[build]
jobs = 1                      # number of parallel jobs
rustflags = ["…", "…"]        # custom flags
incremental = true

[env]
ENV_VAR_NAME_2 = { value = "value", force = true }

[http]
ssl-version.max = "tlsv1.3"
timeout = 30
multi = """
target-dir = "/not/this"
"""
lit = '''
[build]
target-dir = "/nor/this"
'''
when = 1979-05-27 07:32:00Z

[target.x86_64-unknown-linux-gnu]
runner = "foo"
[[nested.array]]
x = [1, [2, 3], { a = "b" }]

[build]
target-dir = "/b/x"
"#;
        // `[build]` twice is not TOML, but residue reads one key, so it
        // finds it either way; Cargo would refuse the file.
        assert_eq!(found(text), "/b/x");
        // A file cut short anywhere, as one being written is, gives an answer
        // or an error, never a panic: the text comes from the repository.
        for (end, _) in text.char_indices() {
            let _answer_or_error = read(&text[..end]);
        }
        assert_eq!(read("[build]\njobs = 1\n"), Ok(None));
        assert_eq!(read(""), Ok(None));
        assert_eq!(read("# only a comment"), Ok(None));
        // The key under another table is not this one.
        assert_eq!(read("[target.x]\ntarget-dir = \"/x\"\n"), Ok(None));
        assert_eq!(read("[profile.build]\ntarget-dir = \"/x\"\n"), Ok(None));
    }

    #[test]
    fn refuses_what_it_cannot_read_with_certainty() {
        for text in [
            // Not TOML.
            "[build\ntarget-dir = \"/x\"\n",
            "[build]\ntarget-dir = \"/x\n",
            "[build]\ntarget-dir = \"/x\" trailing\n",
            "[build]\ntarget-dir \"/x\"\n",
            "[build]\ntarget-dir = \"\\q\"\n",
            "[build]\ntarget-dir = \"/x\"\rjobs = 1\n",
            "a = \"\u{7}\"\n",
            // The key, set in a form that is not a one-line string.
            "[build]\ntarget-dir = \"\"\"/x\"\"\"\n",
            "[build]\ntarget-dir = 5\n",
            "[build]\ntarget-dir = [\"/x\"]\n",
            "[build.target-dir]\npath = \"/x\"\n",
            "[[build]]\ntarget-dir = \"/x\"\n",
            "build = 5\n",
            "[build]\ntarget-dir.x = \"/x\"\n",
            // Set twice.
            "[build]\ntarget-dir = \"/x\"\ntarget-dir = \"/y\"\n",
            "build.target-dir = \"/x\"\n[build]\ntarget-dir = \"/y\"\n",
            // Other files may set it.
            "include = [\"other.toml\"]\n",
            "include = [{ path = \"o.toml\", optional = true }]\n[build]\njobs = 1\n",
        ] {
            assert!(read(text).is_err(), "{text:?} gave {:?}", read(text));
        }
        assert!(target_dir(b"[build]\ntarget-dir = \"/\xff\"\n").is_err());
        // Nesting a file can hold, without running the stack out, and the
        // levels it is out of again (#190).
        let deep = |n: usize, open: &str, inner: &str, close: &str| {
            format!(
                "a = {}{inner}{}\nbuild.target-dir = \"/x\"\n",
                open.repeat(n),
                close.repeat(n)
            )
        };
        assert_eq!(found(&deep(NEST_LIMIT, "[", "", "]")), "/x");
        assert_eq!(found(&deep(NEST_LIMIT, "{ b = ", "1", " }")), "/x");
        let many: String = (0..NEST_LIMIT * 2)
            .map(|i| format!("a{i} = [[[{{ b = [1] }}]]]\n"))
            .collect();
        assert_eq!(found(&(many + "build.target-dir = \"/x\"\n")), "/x");
        for text in [
            deep(NEST_LIMIT + 1, "[", "", "]"),
            deep(NEST_LIMIT + 1, "{ b = ", "1", " }"),
            "a = ".to_string() + &"[".repeat(CONFIG_LIMIT),
        ] {
            assert!(read(&text).unwrap_err().contains("deep"), "{}", &text[..40]);
        }
        // The file's own value overrides an included one.
        assert_eq!(
            found("include = [\"o.toml\"]\n[build]\ntarget-dir = \"/x\"\n"),
            "/x"
        );
    }

    /// A folder of the test's own, by its canonical path, holding `wt`, a
    /// stand-in for a worktree.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "residue-target-dir-{test}-{}-{}",
            std::process::id(),
            crate::output::now()
        ));
        fs::create_dir_all(dir.join("wt/.cargo")).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    /// A snapshot that saw no process: the activity probe is not under test.
    fn no_probe() -> Snapshot {
        Snapshot {
            backend: "none",
            state: Err(crate::activity::Unavailable {
                reason: "unsupported",
                detail: "not probed in a unit test".into(),
            }),
        }
    }

    fn found_in(s: &Path) -> Option<TargetDir> {
        find(&s.join("wt")).and_then(|t| t.measure(&s.join("wt"), &no_probe()))
    }

    #[test]
    fn measures_only_a_folder_cargo_tagged() {
        let s = scratch("tagged");
        fs::write(
            s.join("wt/.cargo/config.toml"),
            "[build]\ntarget-dir = \"../build\"\n",
        )
        .unwrap();
        // Nothing there yet.
        let t = found_in(&s).unwrap();
        assert_eq!(t.path.as_deref().ok(), Some(s.join("build").as_path()));
        assert!(matches!(t.size, Size::Missing));
        // A folder without the tag, then with another tool's.
        fs::create_dir(s.join("build")).unwrap();
        fs::write(s.join("build/big"), vec![0u8; 100_000]).unwrap();
        let t = found_in(&s).unwrap();
        assert_eq!(t.size.reason(), Some("no_cargo_tag"));
        fs::write(
            s.join("build/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n# another tool\n",
        )
        .unwrap();
        assert_eq!(found_in(&s).unwrap().size.reason(), Some("no_cargo_tag"));
        // Cargo's tag: measured, every file counted.
        fs::write(
            s.join("build/CACHEDIR.TAG"),
            [CARGO_TAG, b"# more\n"].concat(),
        )
        .unwrap();
        let t = found_in(&s).unwrap();
        let Size::Measured { usage, .. } = &t.size else {
            panic!("not measured: {:?}", t.size.detail());
        };
        assert!(usage.apparent >= 100_000, "{}", usage.apparent);
        // `scan` names it without measuring.
        assert_eq!(
            find(&s.join("wt")).unwrap().size.reason(),
            Some("not_requested")
        );
        // `.cargo/config` wins over `.cargo/config.toml`, as in Cargo.
        fs::write(s.join("wt/.cargo/config"), "[build]\njobs = 1\n").unwrap();
        assert!(find(&s.join("wt")).is_none());
        fs::remove_dir_all(&s).unwrap();
    }

    #[test]
    fn never_measures_root_a_link_or_the_worktree() {
        let s = scratch("refused");
        let config = s.join("wt/.cargo/config.toml");
        // `/` holds the worktree, and has no Cargo tag either.
        fs::write(&config, "[build]\ntarget-dir = \"/\"\n").unwrap();
        assert_eq!(found_in(&s).unwrap().size.reason(), Some("holds_worktree"));
        // A tagged folder reached through a symbolic link is not followed.
        fs::create_dir(s.join("real")).unwrap();
        fs::write(s.join("real/CACHEDIR.TAG"), CARGO_TAG).unwrap();
        std::os::unix::fs::symlink(s.join("real"), s.join("link")).unwrap();
        fs::write(&config, "build.target-dir = \"../link\"\n").unwrap();
        let t = found_in(&s).unwrap();
        assert_eq!(t.size.reason(), Some("unreadable"), "{:?}", t.size.detail());
        // Inside the worktree: not named, the walk counts it.
        fs::write(&config, "build.target-dir = \"out\"\n").unwrap();
        assert!(found_in(&s).is_none());
        // A config that is a link, or one residue cannot parse: not read.
        fs::write(s.join("other.toml"), "build.target-dir = \"/x\"\n").unwrap();
        fs::remove_file(&config).unwrap();
        std::os::unix::fs::symlink(s.join("other.toml"), &config).unwrap();
        assert!(find(&s.join("wt")).unwrap().path.is_err());
        fs::remove_file(&config).unwrap();
        fs::write(&config, "[build\n").unwrap();
        assert!(find(&s.join("wt")).unwrap().path.is_err());
        fs::remove_dir_all(&s).unwrap();
    }

    #[test]
    fn resolves_a_relative_path_against_the_worktree() {
        let wt = Path::new("/w/repo/wt");
        let outside = |p: &str| Resolved::Outside(PathBuf::from(p));
        assert_eq!(resolve(wt, "../build"), outside("/w/repo/build"));
        assert_eq!(resolve(wt, "../../t/wt"), outside("/w/t/wt"));
        assert_eq!(resolve(wt, "/b/x"), outside("/b/x"));
        assert_eq!(resolve(wt, "/b/./x/"), outside("/b/x"));
        assert_eq!(resolve(wt, "/"), outside("/"));
        assert_eq!(resolve(wt, "../../../../.."), outside("/"));
        // Cargo does not expand `~`.
        assert_eq!(resolve(wt, "~/t"), Resolved::Inside);
        assert_eq!(resolve(wt, "target"), Resolved::Inside);
        assert_eq!(resolve(wt, "."), Resolved::Inside);
        assert_eq!(resolve(wt, ""), Resolved::Inside);
        assert_eq!(resolve(wt, "/w/repo/wt/out"), Resolved::Inside);
        assert_eq!(resolve(wt, "../wt/t"), Resolved::Inside);
        // A sibling whose name starts with the worktree's is outside.
        assert_eq!(resolve(wt, "../wt2"), outside("/w/repo/wt2"));
        // `..` after a name the value gave: that name may be a link.
        assert_eq!(
            resolve(wt, "link/../t"),
            Resolved::Unsure(PathBuf::from("/w/repo/wt/link/../t"))
        );
        assert_eq!(
            resolve(wt, "/b/../t"),
            Resolved::Unsure(PathBuf::from("/b/../t"))
        );
    }
}
