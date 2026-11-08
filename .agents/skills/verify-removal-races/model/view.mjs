// The words /statechart uses for Removal.tla: each state as the user would
// see the worktree, each step as what happens.

export const title = "One worktree through removal, the trash, purge and restore";

const where = { orig: "At its path", aside: "Moved aside", trash: "In the trash" };
const files = { whole: "", part: "part of its files gone", nogit: "its .git file gone", gone: "its files gone" };

export const state = (v) => {
  const w = v.w;
  const words = [where[w.at]];
  if (files[w.tree]) words.push(files[w.tree]);
  if (w.gitdir === "gone") words.push("Git no longer lists it");
  if (w.gitdir === "part") words.push("part of its Git directory gone");
  if (Object.values(v.g).some((c) => c.step !== "none")) words.push("Git removing it");
  if (w.records.length > 0) words.push("record kept");
  return words.join(", ");
};

export const group = (v) => {
  const w = v.w;
  if (w.gitdir !== "ok" && w.records.length === 0) return "lost";
  if (w.at === "orig") return "back";
  return "removing";
};

export const colors = { lost: "orange", back: "teal", removing: "blue" };

const kept = (from, to) =>
  to.w.records.length < from.w.records.length ? "drops its record" : "keeps its record";

const labels = {
  RRecheck: () => "remove checks it again",
  RMove: () => "remove moves it aside",
  RLock: () => "remove locks it",
  RRecord: () => "remove writes its removal record",
  RVerify: () => "remove checks it at the hidden path",
  RUnlock: () => "remove unlocks it and runs git worktree remove",
  RAfter: (f) => (f.g.rm.res === "ok" ? "git worktree remove succeeds" : "git worktree remove fails"),
  PBCheck: () => "remove starts to put it back",
  PBUnlock: () => "remove unlocks it to put it back",
  PBMove: () => "remove moves it back to its path",
  PBStuck: () => "remove cannot put it back and locks it",
  RSettle: (f, t) => `remove ${kept(f, t)}`,
  PRecheck: () => "the purge checks it again",
  PRecord: () => "the purge writes its removal record",
  PUnlock: () => "the purge unlocks it and runs git worktree remove",
  PAfter: (f) => (f.g.pg.res === "ok" ? "git worktree remove succeeds" : "git worktree remove fails"),
  PRelock: () => "the purge locks it again",
  PSettle: (f, t) => `the purge ${kept(f, t)}`,
  GFind: () => "Git looks it up",
  GExists: (f, t) =>
    Object.values(t.g).some((c) => c.step === "gitdir") ? "Git finds nothing at the path" : "Git finds its folder",
  GStatus: () => "Git runs git status in it",
  GTree: () => "Git deletes its files",
  GGitdir: () => "Git deletes its Git directory",
  GitTimeout: () => "residue kills Git after 30 minutes",
  Kill: () => "residue is stopped",
  Orphan: () => "residue alone is stopped; its git worktree remove runs on",
  TList: () => "you run residue restore ENTRY",
  TCheck: () => "restore checks the trash entry",
  TUnlock: () => "restore unlocks it",
  TMove: () => "restore moves it back to its path",
  TRelock: () => "restore locks it again",
  TDiscard: () => "restore drops the trash entry",
  TForget: (f, t) => (kept(f, t) === "drops its record" ? "restore forgets the purge's record" : "restore finds no record to forget"),
  TReport: () => "restore says it is back",
  SList: () => "you run residue restore RECORD",
  SDest: () => "restore checks its path",
  SCopies: () => "restore reads the record",
  SMoveBack: () => "restore checks the hidden folder",
  SUnlock: () => "restore unlocks it",
  SMove: () => "restore moves it back to its path",
  SRelock: () => "restore locks it again",
  SRecreate: () => "restore recreates it",
  SAfter: (f, t) => (kept(f, t) === "drops its record" ? "restore forgets the record" : "restore keeps the record"),
  SReport: () => "restore says it is back",
  UserEdits: () => "you edit a file in it",
  UserStartsProcess: () => "a process starts in it",
  UserLocks: () => "you lock it with git worktree lock",
};

export const step = (action, from, to) => (labels[action] ? labels[action](from, to) : null);
