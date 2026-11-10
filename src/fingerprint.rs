//! The fingerprints a program reads from one run and passes back to the
//! next with `--expect`: a remove plan's (`plan` in `residue.remove/v1`)
//! and the service's (`service status`). Each is 32 lowercase hex digits.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The exit status of a run whose `--expect` names what is no longer
/// there: `remove` removed, trashed, purged and forgot nothing, and
/// `service install` wrote nothing.
pub const CHANGED: u8 = 4;

/// FNV-1a, 128 bits, over parts, each with its length first, so a byte
/// cannot move from one part to the next and leave the same hash. A check
/// against change by accident, not a secret: whoever can change what it
/// covers can run residue too.
pub struct Hasher(u128);

impl Hasher {
    const BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    /// A hash of `domain`, the first part, which names what it covers.
    pub fn start(domain: &[u8]) -> Hasher {
        let mut hash = Hasher(Self::BASIS);
        hash.part(domain);
        hash
    }

    pub fn part(&mut self, bytes: &[u8]) {
        for b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            self.0 ^= u128::from(*b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    /// 32 lowercase hex digits.
    pub fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

/// A fingerprint as [`Hasher::hex`] writes it: 32 lowercase hex digits.
pub fn is_fingerprint(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a remove plan does with one thing it lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Does {
    /// A worktree it deletes with `git worktree remove`.
    Remove,
    /// A worktree it moves into the trash.
    Trash,
    /// An expired trash entry whose worktree it deletes.
    Purge,
    /// A trash entry or a removal record that holds only its record files,
    /// which it clears.
    Clear,
    /// An expired removal record it forgets, with its refs.
    Forget,
    /// Left as it is: a check did not pass, or no run may act on it.
    Keep,
    /// A PATH that names no worktree residue may remove.
    Invalid,
}

impl Does {
    fn code(self) -> &'static [u8] {
        match self {
            Does::Remove => b"remove",
            Does::Trash => b"trash",
            Does::Purge => b"purge",
            Does::Clear => b"clear",
            Does::Forget => b"forget",
            Does::Keep => b"keep",
            Does::Invalid => b"invalid",
        }
    }

    /// It changes something, so the plan covers what it changes.
    fn acts(self) -> bool {
        !matches!(self, Does::Keep | Does::Invalid)
    }
}

/// A branch and HEAD, as Git lists them or a record keeps them.
#[derive(Clone, Copy)]
pub struct At<'a> {
    /// The full ref name; `None` for a detached HEAD.
    pub branch: Option<&'a str>,
    pub head: Option<&'a str>,
}

/// What the `info` of a trash entry or a removal record says of the
/// worktree it holds or held.
#[derive(Clone, Copy)]
pub struct Was<'a> {
    pub original: &'a Path,
    pub at: At<'a>,
    /// When it was trashed or removed, in Unix seconds.
    pub when: i64,
}

/// One thing a remove plan lists, as its fingerprint covers it. Each is
/// named by where it is and what the plan does with it. What the plan
/// changes it also covers by what it is: a branch, a HEAD, a record. It
/// never covers what changes between two runs while the plan does not: a
/// time a run read, a size, a process, why a check kept something.
#[derive(Clone)]
pub enum Step<'a> {
    /// A worktree a PATH names, or one a sweep found.
    Worktree {
        /// Its canonical path, or the PATH as given when it names no
        /// worktree residue may remove (`Does::Invalid`).
        path: &'a Path,
        /// The repository's common Git directory; `None` when the PATH
        /// names no worktree.
        repository: Option<&'a Path>,
        does: Does,
        at: At<'a>,
    },
    /// An expired trash entry, or one that holds only its record.
    Trash {
        entry: &'a Path,
        does: Does,
        /// `None` when residue cannot read its `info`.
        record: Option<Was<'a>>,
        /// The worktree in it as Git lists it; `None` when Git does not.
        listed: Option<At<'a>>,
    },
    /// An expired removal record, or one with no `info` to clear.
    Record {
        entry: &'a Path,
        does: Does,
        /// `None` when residue cannot read its `info`.
        record: Option<Was<'a>>,
        /// The commits its refs keep besides HEAD, which forgetting stops
        /// keeping.
        pinned: Vec<&'a str>,
    },
}

/// One step's parts, each with its length first, as one byte string.
struct Parts(Vec<u8>);

impl Parts {
    fn new(kind: &[u8]) -> Parts {
        Parts(Vec::new()).part(kind)
    }

    fn part(mut self, bytes: &[u8]) -> Parts {
        self.0
            .extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.0.extend_from_slice(bytes);
        self
    }

    fn path(self, path: &Path) -> Parts {
        self.part(path.as_os_str().as_bytes())
    }

    /// `name` and the value, or `no NAME`: never confused with an empty
    /// value.
    fn opt(self, name: &str, value: Option<&[u8]>) -> Parts {
        match value {
            Some(v) => self.part(name.as_bytes()).part(v),
            None => self.part(format!("no {name}").as_bytes()),
        }
    }

    fn at(self, at: &At) -> Parts {
        self.opt("branch", at.branch.map(str::as_bytes))
            .opt("head", at.head.map(str::as_bytes))
    }

    fn was(self, was: Option<&Was>) -> Parts {
        match was {
            Some(w) => self
                .part(b"record")
                .path(w.original)
                .at(&w.at)
                .part(&w.when.to_le_bytes()),
            None => self.part(b"no record"),
        }
    }
}

impl Step<'_> {
    fn encode(&self) -> Vec<u8> {
        let parts = match self {
            Step::Worktree {
                path,
                repository,
                does,
                at,
            } => {
                let parts = Parts::new(b"worktree")
                    .path(path)
                    .opt("repository", repository.map(|r| r.as_os_str().as_bytes()))
                    .part(does.code());
                if does.acts() { parts.at(at) } else { parts }
            }
            Step::Trash {
                entry,
                does,
                record,
                listed,
            } => {
                let parts = Parts::new(b"trash").path(entry).part(does.code());
                if !does.acts() {
                    parts
                } else {
                    let parts = parts.was(record.as_ref());
                    match listed {
                        Some(at) => parts.part(b"listed").at(at),
                        None => parts.part(b"not listed"),
                    }
                }
            }
            Step::Record {
                entry,
                does,
                record,
                pinned,
            } => {
                let parts = Parts::new(b"removed").path(entry).part(does.code());
                if !does.acts() {
                    parts
                } else {
                    let mut parts = parts
                        .was(record.as_ref())
                        .part(&(pinned.len() as u64).to_le_bytes());
                    for commit in pinned.iter() {
                        parts = parts.part(commit.as_bytes());
                    }
                    parts
                }
            }
        };
        parts.0
    }
}

/// The fingerprint of a remove plan: its steps, each once, in an order of
/// their own, so the order of the PATHs does not count, and the
/// `--keep-trash` and `--keep-removed` the plan says new entries and
/// records keep, in seconds.
pub fn plan(steps: &[Step], keep_trash: i64, keep_removed: i64) -> String {
    let mut encoded: Vec<Vec<u8>> = steps.iter().map(Step::encode).collect();
    encoded.sort();
    encoded.dedup();
    let mut hash = Hasher::start(b"residue remove plan 1");
    hash.part(&keep_trash.to_le_bytes());
    hash.part(&keep_removed.to_le_bytes());
    for step in &encoded {
        hash.part(step);
    }
    hash.hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn at(branch: Option<&'static str>, head: &'static str) -> At<'static> {
        At {
            branch,
            head: Some(head),
        }
    }

    fn worktree(path: &'static str, does: Does) -> Step<'static> {
        Step::Worktree {
            path: Path::new(path),
            repository: Some(Path::new("/r/.git")),
            does,
            at: at(Some("refs/heads/w"), HEAD),
        }
    }

    fn was(original: &'static str, branch: &'static str) -> Was<'static> {
        Was {
            original: Path::new(original),
            at: at(Some(branch), HEAD),
            when: 1_767_225_600,
        }
    }

    fn trashed(does: Does) -> Step<'static> {
        Step::Trash {
            entry: Path::new("/r/.git/residue-trash/t-20260101T000000Z"),
            does,
            record: Some(was("/r/.worktrees/t", "refs/heads/t")),
            listed: Some(at(Some("refs/heads/t"), HEAD)),
        }
    }

    fn removed(does: Does) -> Step<'static> {
        Step::Record {
            entry: Path::new("/r/.git/residue-removed/r-20260101T000000Z"),
            does,
            record: Some(was("/r/.worktrees/r", "refs/heads/r")),
            pinned: Vec::new(),
        }
    }

    /// A plan that trashes w, keeps u, purges one trash entry and forgets
    /// one removal record.
    fn base() -> Vec<Step<'static>> {
        vec![
            worktree("/r/.worktrees/w", Does::Trash),
            worktree("/r/.worktrees/u", Does::Keep),
            trashed(Does::Purge),
            removed(Does::Forget),
        ]
    }

    fn of(steps: &[Step]) -> String {
        plan(steps, 604_800, 2_592_000)
    }

    /// `base` with one change.
    fn changed(change: impl FnOnce(&mut Vec<Step<'static>>)) -> String {
        let mut steps = base();
        change(&mut steps);
        of(&steps)
    }

    /// The service's fingerprint, which the Mac app passes on every save,
    /// is the same as before the hash moved here.
    #[test]
    fn the_hash_is_fnv_1a_128_with_lengths() {
        let mut hash = Hasher::start(b"residue service fingerprint");
        hash.part(b"0.0.12");
        hash.part(b"");
        hash.part(b"no file");
        hash.part(b"no binary");
        assert_eq!(hash.hex(), "cd127761c2dcd6d6564aeb6f8e5d5858");
    }

    #[test]
    fn a_fingerprint_is_32_lowercase_hex_digits() {
        let fp = "0123456789abcdef0123456789abcdef";
        assert!(is_fingerprint(fp));
        let upper = fp.to_uppercase();
        let long = format!("{fp}0");
        for bad in [
            "",
            "none",
            &upper,
            &fp[1..],
            &long,
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcde ",
        ] {
            assert!(!is_fingerprint(bad), "{bad:?}");
        }
    }

    /// Two runs that make the same plan give the same fingerprint, in any
    /// order of their PATHs, and with a PATH given twice.
    #[test]
    fn the_same_plan_in_any_order_gives_the_same_fingerprint() {
        let fp = of(&base());
        assert!(is_fingerprint(&fp));
        assert_eq!(of(&base()), fp);
        assert_eq!(changed(|s| s.reverse()), fp);
        assert_eq!(changed(|s| s.swap(0, 1)), fp);
        assert_eq!(
            changed(|s| s.push(worktree("/r/.worktrees/w", Does::Trash))),
            fp
        );
    }

    /// Each change to what the plan acts on, or to what it does with
    /// something it lists, gives another fingerprint.
    #[test]
    fn each_change_to_the_plan_gives_another_fingerprint() {
        let set_at = |s: &mut Vec<Step<'static>>, new: At<'static>| {
            if let Step::Worktree { at, .. } = &mut s[0] {
                *at = new;
            }
        };
        let all = [
            ("same", of(&base())),
            (
                "another branch",
                changed(|s| set_at(s, at(Some("refs/heads/x"), HEAD))),
            ),
            ("detached", changed(|s| set_at(s, at(None, HEAD)))),
            (
                "HEAD moved",
                changed(|s| set_at(s, at(Some("refs/heads/w"), OTHER))),
            ),
            (
                "another path",
                changed(|s| s[0] = worktree("/r/.worktrees/v", Does::Trash)),
            ),
            (
                "another repository",
                changed(|s| {
                    if let Step::Worktree { repository, .. } = &mut s[0] {
                        *repository = Some(Path::new("/other/.git"));
                    }
                }),
            ),
            (
                "deleted, not trashed",
                changed(|s| s[0] = worktree("/r/.worktrees/w", Does::Remove)),
            ),
            (
                "kept, not trashed",
                changed(|s| s[0] = worktree("/r/.worktrees/w", Does::Keep)),
            ),
            (
                "a kept one is a candidate",
                changed(|s| s[1] = worktree("/r/.worktrees/u", Does::Trash)),
            ),
            (
                "a kept one is invalid",
                changed(|s| s[1] = worktree("/r/.worktrees/u", Does::Invalid)),
            ),
            ("one PATH fewer", changed(|s| drop(s.remove(1)))),
            (
                "the trash entry kept",
                changed(|s| s[2] = trashed(Does::Keep)),
            ),
            (
                "the trash entry cleared",
                changed(|s| s[2] = trashed(Does::Clear)),
            ),
            (
                "another trash entry",
                changed(|s| {
                    if let Step::Trash { entry, .. } = &mut s[2] {
                        *entry = Path::new("/r/.git/residue-trash/t-20260101T000000Z-2");
                    }
                }),
            ),
            (
                "the trashed worktree's HEAD moved",
                changed(|s| {
                    if let Step::Trash { listed, .. } = &mut s[2] {
                        *listed = Some(at(Some("refs/heads/t"), OTHER));
                    }
                }),
            ),
            (
                "Git no longer lists the trashed worktree",
                changed(|s| {
                    if let Step::Trash { listed, .. } = &mut s[2] {
                        *listed = None;
                    }
                }),
            ),
            (
                "the trash entry's record says another time",
                changed(|s| {
                    if let Step::Trash {
                        record: Some(was), ..
                    } = &mut s[2]
                    {
                        was.when += 1;
                    }
                }),
            ),
            (
                "one more trash entry",
                changed(|s| {
                    s.push(Step::Trash {
                        entry: Path::new("/r/.git/residue-trash/x-20260101T000000Z"),
                        does: Does::Purge,
                        record: None,
                        listed: None,
                    })
                }),
            ),
            (
                "the removal record kept",
                changed(|s| s[3] = removed(Does::Keep)),
            ),
            (
                "another removal record",
                changed(|s| {
                    if let Step::Record { entry, .. } = &mut s[3] {
                        *entry = Path::new("/r/.git/residue-removed/r-20260101T000001Z");
                    }
                }),
            ),
            (
                "the removal record's HEAD",
                changed(|s| {
                    if let Step::Record {
                        record: Some(was), ..
                    } = &mut s[3]
                    {
                        was.at = at(Some("refs/heads/r"), OTHER);
                    }
                }),
            ),
            (
                "the removal record unreadable",
                changed(|s| {
                    if let Step::Record { record, .. } = &mut s[3] {
                        *record = None;
                    }
                }),
            ),
            (
                "the removal record pins a commit",
                changed(|s| {
                    if let Step::Record { pinned, .. } = &mut s[3] {
                        pinned.push(OTHER);
                    }
                }),
            ),
            ("no removal record", changed(|s| drop(s.remove(3)))),
            ("another --keep-trash", plan(&base(), 86_400, 2_592_000)),
            ("another --keep-removed", plan(&base(), 604_800, 86_400)),
        ];
        for (i, (what, a)) in all.iter().enumerate() {
            assert!(is_fingerprint(a), "{what}: {a}");
            for (other, b) in &all[i + 1..] {
                assert_ne!(a, b, "{what} and {other}");
            }
        }
    }

    /// What the plan only keeps counts by where it is and that it is kept,
    /// never by its branch, its HEAD or its record: those change while
    /// someone works in it, and the plan acts on none of them.
    #[test]
    fn what_the_plan_keeps_counts_only_as_kept() {
        let kept = |at: At<'static>| Step::Worktree {
            path: Path::new("/r/.worktrees/u"),
            repository: Some(Path::new("/r/.git")),
            does: Does::Keep,
            at,
        };
        let a = of(&[kept(at(Some("refs/heads/u"), HEAD))]);
        assert_eq!(a, of(&[kept(at(Some("refs/heads/x"), OTHER))]));
        assert_eq!(a, of(&[kept(at(None, HEAD))]));
        let entry = trashed(Does::Keep);
        let mut bare = entry.clone();
        if let Step::Trash { listed, record, .. } = &mut bare {
            *listed = None;
            *record = None;
        }
        assert_eq!(of(&[entry]), of(&[bare]));
        // Once the plan acts on it, its HEAD counts.
        let acted = |at: At<'static>| Step::Worktree {
            path: Path::new("/r/.worktrees/u"),
            repository: Some(Path::new("/r/.git")),
            does: Does::Remove,
            at,
        };
        assert_ne!(
            of(&[acted(at(Some("refs/heads/u"), HEAD))]),
            of(&[acted(at(Some("refs/heads/u"), OTHER))])
        );
    }
}
