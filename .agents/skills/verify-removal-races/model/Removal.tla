------------------------------- MODULE Removal -------------------------------
(***************************************************************************)
(* residue's removal pipeline and trash lifecycle, one worktree W, with    *)
(* the restores and the user that can act while a removal runs.           *)
(*                                                                         *)
(* Each action is one indivisible step of the code: one Git command, one  *)
(* file system call, or one read that the code makes before it acts.     *)
(* The comment above each action names the Rust function it models.      *)
(* `git worktree remove` is not one step: Git looks the worktree up,      *)
(* runs `git status` in it, deletes its files by path, then deletes its   *)
(* Git directory by id (builtin/worktree.c, remove_worktree, Git 2.50).   *)
(*                                                                         *)
(* Actors:                                                                *)
(*   rm  `residue remove --no-trash`: remove.rs execute                   *)
(*   pg  a remove run's purge of W's trash entry: remove.rs purge,         *)
(*       trash.rs purge                                                    *)
(*   tr  `residue restore ENTRY` on W's trash entry: restore.rs            *)
(*       restore_trash, trash.rs restore                                   *)
(*   rr  `residue restore RECORD` on a removal record: removed.rs restore  *)
(*   the user, who edits W, starts a process in it, or locks it            *)
(* Start = "orig" runs rm and rr; Start = "trash" runs pg, tr and rr.     *)
(* Off and After narrow a setup; check.sh lists the setups it checks.     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Start,      \* "orig": W is a candidate at its path; "trash": an expired trash entry
  Broken,     \* names of the guards a mutant run removes; {} checks the code as it is
  Faults,     \* the faults a behavior may have, at most one: "kill", "timeout", "orphan"
  Off,        \* actors that do not run
  After       \* restores ("tr", "rr") that list only once no remove run or purge runs

Mut(name) == name \in Broken

D == {"rm", "pg"}                 \* the actors that delete
Actors == D \cup {"tr", "rr"}
\* Where each deleting actor deletes, which is also its record's `aside`.
P(a) == IF a = "rm" THEN "aside" ELSE "trash"

VARIABLES
  w,       \* the world: W's directory, its Git record, the trash entry, the removal records
  pc,      \* each actor's next step
  g,       \* each deleting actor's `git worktree remove` child
  chk,     \* each deleting actor's last passing check: where, and whether W was unchanged and idle
  ctx,     \* rm's Moved::put_back(locked) and the settle that follows it
  ts,      \* what tr's listing (restore.rs gather) saw
  rs,      \* what rr's listing saw, and how it restored
  faults,
  hist     \* history: a rule broke at some step

vars == <<w, pc, g, chk, ctx, ts, rs, faults, hist>>

(* w.at      where W's directory is: "orig" (its path), "aside" (the hidden *)
(*           .NAME.residue-removing-PID), "trash" (residue-trash/ENTRY/NAME) *)
(* w.tree    its files: "whole", "part" (some deleted, the .git file is     *)
(*           there), "nogit" (the .git file deleted), "gone"                *)
(* w.gitdir  its Git directory, .git/worktrees/ID: "ok", "part", "gone".    *)
(*           Git lists W, at w.at, while it is not "gone".                  *)
(* w.gen     0 for W's own Git directory, 1 once restore recreated it       *)
(* w.lock    its lock reason: "none", "removing" (residue: removing),       *)
(*           "trash" (residue: in trash), "user"                            *)
(* w.edits   changes to W since the plan; the plan saw 0                    *)
(* w.busy    a process the probe sees is in W                               *)
(* w.entry   W's trash entry: "info" while its record file is there         *)
(* w.records the removal records, with their refs: "rm" or "pg", named by   *)
(*           the actor that wrote them                                      *)

Listed(p) == w.at = p /\ w.gitdir # "gone"
Exists(p) == w.at = p /\ w.tree # "gone"
DotGit == w.tree \in {"whole", "part"}
\* What `git worktree move` needs of its source: listed there, a Git
\* directory it can use, and the .git file that ties the two.
Movable(p) == Listed(p) /\ w.gitdir = "ok" /\ DotGit /\ w.lock = "none"
\* removed.rs Item::interrupted: Git still lists W where the record's
\* deletion started.
Interrupted(r) == Listed(P(r))
Running(a) == pc[a] \notin {"done", "dead", "off"}
\* A restore in After waits for the remove run or purge to stop. A Git
\* child left running by an orphaning kill does not count.
Waits(a) == a \in After => \A d \in D : ~Running(d)
NoChecks == [a \in D |-> [path |-> "none", clean |-> FALSE]]
Clean == w.edits = 0 /\ ~w.busy
Go(a, s) == pc' = [pc EXCEPT ![a] = s]
Lifted == IF w.lock = "user" THEN [hist EXCEPT !.lockLifted = TRUE] ELSE hist

Init ==
  /\ w = [at |-> Start, tree |-> "whole", gitdir |-> "ok", gen |-> 0,
          lock |-> IF Start = "trash" THEN "trash" ELSE "none",
          edits |-> 0, busy |-> FALSE, ulock |-> FALSE,
          entry |-> IF Start = "trash" THEN "info" ELSE "none",
          records |-> {}]
  /\ pc = [a \in Actors |->
             IF a \in Off THEN "off"
             ELSE CASE a = "rm" -> IF Start = "orig" THEN "r_recheck" ELSE "off"
                    [] a = "pg" -> IF Start = "trash" THEN "p_recheck" ELSE "off"
                    [] a = "tr" -> IF Start = "trash" THEN "t_list" ELSE "off"
                    [] a = "rr" -> "s_list"]
  /\ g = [a \in D |-> [step |-> "none", err |-> FALSE, res |-> "none"]]
  /\ chk = NoChecks
  /\ ctx = [locked |-> FALSE, settle |-> FALSE, paths |-> {}]
  /\ ts = [reg |-> FALSE, lock |-> "none", recs |-> {}]
  /\ rs = [rec |-> "none", reg |-> FALSE, back |-> FALSE, lock |-> "none",
           how |-> "none"]
  /\ faults = 0
  /\ hist = [lostBack |-> FALSE, unchecked |-> FALSE, lied |-> FALSE,
             lockLifted |-> FALSE, undoPart |-> FALSE]

----------------------------------------------------------------------------
(* remove.rs standing and settle. Whole: Git lists W at one of the paths,  *)
(* its .git file is there (checkout_present), Git can use its Git          *)
(* directory (git::identity), the directory is the one the record was      *)
(* written for (Written::is_its_git_dir), and nothing in it is gone        *)
(* (Written::lacks). Only then does settle drop the record.               *)

Standing(ps) ==
  IF w.at \in ps /\ w.gitdir # "gone"
  THEN IF ~DotGit \/ w.gitdir # "ok" THEN "lacking"
       ELSE IF w.gen # 0 THEN "unlisted"
       ELSE "whole"
  ELSE "unlisted"

Settle(a, ps) ==
  IF Standing(ps) = "whole" \/ Mut("SettleAlwaysUndoes")
  THEN /\ w' = [w EXCEPT !.records = @ \ {a}]
       /\ hist' = IF w.tree # "whole" \/ w.gitdir # "ok"
                  THEN [hist EXCEPT !.undoPart = TRUE] ELSE hist
  ELSE UNCHANGED <<w, hist>>

----------------------------------------------------------------------------
(* rm: remove.rs execute, without --trash                                  *)

\* recheck(original): Git lists W at its path, the lock is the plan's, the
\* verdict is still candidate (no process in it), the fingerprint matches.
RRecheck ==
  /\ pc["rm"] = "r_recheck"
  /\ IF Movable("orig") /\ Clean /\ w.tree = "whole"
     THEN /\ Go("rm", "r_move")
          /\ chk' = [chk EXCEPT !["rm"] = [path |-> "orig", clean |-> Clean]]
     ELSE /\ Go("rm", "done")
          /\ UNCHANGED chk
  /\ UNCHANGED <<w, g, ctx, ts, rs, faults, hist>>

\* git::move_worktree(original, dest): to the hidden aside name.
RMove ==
  /\ pc["rm"] = "r_move"
  /\ IF Movable("orig")
     THEN /\ w' = [w EXCEPT !.at = "aside"]
          /\ chk' = NoChecks
          /\ Go("rm", "r_lock")
     ELSE /\ Go("rm", "done")
          /\ UNCHANGED <<w, chk>>
  /\ UNCHANGED <<g, ctx, ts, rs, faults, hist>>

PutBack(locked, then, ps) ==
  /\ ctx' = [locked |-> locked, settle |-> then, paths |-> ps]
  /\ Go("rm", "pb_check")

\* git::lock_worktree(dest, REMOVING); on failure put_back(false).
RLock ==
  /\ pc["rm"] = "r_lock"
  /\ IF Listed("aside") /\ w.lock = "none"
     THEN /\ w' = [w EXCEPT !.lock = "removing"]
          /\ Go("rm", "r_record")
          /\ UNCHANGED ctx
     ELSE /\ PutBack(FALSE, FALSE, {})
          /\ UNCHANGED w
  /\ UNCHANGED <<g, chk, ts, rs, faults, hist>>

\* removed::create: the record and its refs, or put_back(true) when it fails.
RRecord ==
  /\ pc["rm"] = "r_record"
  /\ \/ /\ w' = IF Mut("NoRecord") THEN w
                ELSE [w EXCEPT !.records = @ \cup {"rm"}]
        /\ Go("rm", "r_verify")
        /\ UNCHANGED ctx
     \/ /\ PutBack(TRUE, FALSE, {})
        /\ UNCHANGED w
  /\ UNCHANGED <<g, chk, ts, rs, faults, hist>>

\* verify_moved(dest): listed there, locked REMOVING, HEAD and branch as
\* planned, no process the probe sees, the fingerprint again.
VerifyOK ==
  /\ Listed("aside")
  /\ w.lock = "removing"
  /\ ~w.busy
  /\ (Mut("NoFingerprint") \/ (w.edits = 0 /\ w.tree = "whole"))

RVerify ==
  /\ pc["rm"] = "r_verify"
  /\ CASE Mut("NoVerify") ->
            /\ Go("rm", "r_unlock") /\ UNCHANGED <<chk, ctx>>
       [] VerifyOK ->
            /\ Go("rm", "r_unlock")
            /\ chk' = [chk EXCEPT !["rm"] = [path |-> "aside", clean |-> Clean]]
            /\ UNCHANGED ctx
       [] OTHER ->
            /\ PutBack(TRUE, TRUE, {"orig", "aside"}) /\ UNCHANGED chk
  /\ UNCHANGED <<w, g, ts, rs, faults, hist>>

\* git::unlock_worktree(dest); then git::remove_worktree(dest) starts.
RUnlock ==
  /\ pc["rm"] = "r_unlock"
  /\ IF Listed("aside") /\ w.lock # "none"
     THEN /\ w' = [w EXCEPT !.lock = "none"]
          /\ hist' = Lifted
          /\ Go("rm", "r_wait")
          /\ g' = [g EXCEPT !["rm"] = [step |-> "find", err |-> FALSE, res |-> "none"]]
          /\ UNCHANGED ctx
     ELSE /\ PutBack(TRUE, TRUE, {"orig", "aside"})
          /\ UNCHANGED <<w, g, hist>>
  /\ UNCHANGED <<chk, ts, rs, faults>>

\* The match on git::remove_worktree's result.
RAfter ==
  /\ pc["rm"] = "r_wait"
  /\ g["rm"].step = "none" /\ g["rm"].res # "none"
  /\ CASE g["rm"].res = "ok" ->
            /\ Go("rm", "done") /\ UNCHANGED ctx
       [] Standing({"aside"}) = "lacking" ->
            /\ ctx' = [locked |-> FALSE, settle |-> TRUE, paths |-> {"aside"}]
            /\ Go("rm", "r_settle")
       [] OTHER ->
            PutBack(FALSE, TRUE, {"orig", "aside"})
  /\ UNCHANGED <<w, g, chk, ts, rs, faults, hist>>

AfterPutBack == IF ctx.settle THEN "r_settle" ELSE "done"

\* Moved::put_back: something at the original path stops it.
PBCheck ==
  /\ pc["rm"] = "pb_check"
  /\ Go("rm", IF Exists("orig") THEN "pb_stuck"
              ELSE IF ctx.locked \/ Mut("PutBackAlwaysUnlocks") THEN "pb_unlock"
              ELSE "pb_move")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults, hist>>

PBUnlock ==
  /\ pc["rm"] = "pb_unlock"
  /\ IF Listed("aside") /\ w.lock # "none"
     THEN /\ w' = [w EXCEPT !.lock = "none"]
          /\ hist' = Lifted
          /\ Go("rm", "pb_move")
     ELSE /\ Go("rm", "pb_stuck")
          /\ UNCHANGED <<w, hist>>
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults>>

PBMove ==
  /\ pc["rm"] = "pb_move"
  /\ IF Movable("aside")
     THEN /\ w' = [w EXCEPT !.at = "orig"]
          /\ chk' = NoChecks
          /\ Go("rm", AfterPutBack)
     ELSE /\ Go("rm", "pb_stuck")
          /\ UNCHANGED <<w, chk>>
  /\ UNCHANGED <<g, ctx, ts, rs, faults, hist>>

\* Moved::stuck: lock it with residue's reason so no later run removes it.
PBStuck ==
  /\ pc["rm"] = "pb_stuck"
  /\ w' = IF Listed("aside") /\ w.lock = "none"
          THEN [w EXCEPT !.lock = "removing"] ELSE w
  /\ Go("rm", AfterPutBack)
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

RSettle ==
  /\ pc["rm"] = "r_settle"
  /\ Settle("rm", ctx.paths)
  /\ Go("rm", "done")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults>>

----------------------------------------------------------------------------
(* pg: remove.rs purge, then trash.rs purge                                *)

\* recheck(path, ours = LOCK): residue's own trash lock counts as none.
PRecheck ==
  /\ pc["pg"] = "p_recheck"
  /\ IF Mut("PurgeNoRecheck")
       \/ (Listed("trash") /\ w.gitdir = "ok" /\ DotGit
           /\ w.lock \in {"trash", "none"} /\ Clean /\ w.tree = "whole")
     THEN /\ Go("pg", "p_record")
          /\ chk' = IF Mut("PurgeNoRecheck") THEN chk
                    ELSE [chk EXCEPT !["pg"] = [path |-> "trash", clean |-> Clean]]
     ELSE /\ Go("pg", "done")
          /\ UNCHANGED chk
  /\ UNCHANGED <<w, g, ctx, ts, rs, faults, hist>>

\* removed::create, with Reason::TrashPurge and the trash path as aside.
PRecord ==
  /\ pc["pg"] = "p_record"
  /\ \/ /\ w' = [w EXCEPT !.records = @ \cup {"pg"}]
        /\ Go("pg", "p_unlock")
     \/ /\ Go("pg", "done")
        /\ UNCHANGED w
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* trash::purge: the plan's listing saw residue's lock, so it unlocks.
PUnlock ==
  /\ pc["pg"] = "p_unlock"
  /\ IF Listed("trash") /\ w.lock # "none"
     THEN /\ w' = [w EXCEPT !.lock = "none"]
          /\ hist' = Lifted
          /\ Go("pg", "p_wait")
          /\ g' = [g EXCEPT !["pg"] = [step |-> "find", err |-> FALSE, res |-> "none"]]
     ELSE /\ Go("pg", "p_settle")
          /\ UNCHANGED <<w, g, hist>>
  /\ UNCHANGED <<chk, ctx, ts, rs, faults>>

\* After git::remove_worktree: discard the entry, or lock it again.
PAfter ==
  /\ pc["pg"] = "p_wait"
  /\ g["pg"].step = "none" /\ g["pg"].res # "none"
  /\ IF g["pg"].res = "ok"
     THEN IF w.entry = "info"
          THEN /\ w' = [w EXCEPT !.entry = "none"]
               /\ Go("pg", "done")
          ELSE /\ Go("pg", "p_settle")   \* discard: the entry directory is gone
               /\ UNCHANGED w
     ELSE /\ Go("pg", "p_relock")
          /\ UNCHANGED w
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

PRelock ==
  /\ pc["pg"] = "p_relock"
  /\ w' = IF Listed("trash") /\ w.lock = "none"
          THEN [w EXCEPT !.lock = "trash"] ELSE w
  /\ Go("pg", "p_settle")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* settle(&written, [path], back = Some(original)).
PSettle ==
  /\ pc["pg"] = "p_settle"
  /\ Settle("pg", {"trash", "orig"})
  /\ Go("pg", "done")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults>>

----------------------------------------------------------------------------
(* `git worktree remove PATH` without --force, Git 2.50.1                  *)
(* builtin/worktree.c remove_worktree, dir.c remove_dir_recurse            *)

GitExit(a, r) ==
  g' = [g EXCEPT ![a] = [step |-> "none", err |-> FALSE, res |-> r]]

\* get_worktrees, find_worktree by path, die if locked, validate_worktree
\* with WT_VALIDATE_WORKTREE_MISSING_OK.
GFind(a) ==
  /\ g[a].step = "find"
  /\ IF Listed(P(a)) /\ w.gitdir = "ok" /\ w.lock = "none" /\ w.tree # "nogit"
     THEN g' = [g EXCEPT ![a].step = "exists"]
     ELSE GitExit(a, "err")
  /\ UNCHANGED <<w, pc, chk, ctx, ts, rs, faults, hist>>

\* if (file_exists(wt->path)): when nothing is at the path, Git skips the
\* files and goes on to delete the Git directory.
GExists(a) ==
  /\ g[a].step = "exists"
  /\ g' = [g EXCEPT ![a].step = IF Exists(P(a)) THEN "status" ELSE "gitdir"]
  /\ UNCHANGED <<w, pc, chk, ctx, ts, rs, faults, hist>>

\* check_clean_worktree: `git status --porcelain --ignore-submodules=none`
\* at the path. An edit may be one status does not show (an ignored file).
GStatus(a) ==
  /\ g[a].step = "status"
  /\ \/ /\ Exists(P(a)) /\ w.tree = "whole"
        /\ g' = [g EXCEPT ![a].step = "tree"]
     \/ /\ ~Exists(P(a)) \/ w.edits > 0 \/ w.tree # "whole"
        /\ GitExit(a, "err")
  /\ UNCHANGED <<w, pc, chk, ctx, ts, rs, faults, hist>>

Shrink(t) == CASE t = "whole" -> {"part", "nogit", "gone"}
               [] t = "part"  -> {"nogit", "gone"}
               [] OTHER       -> {"gone"}

\* delete_git_work_tree: remove_dir_recursively(wt->path), by path, in the
\* order readdir gives. Once W is renamed away, lstat and rmdir of the old
\* path fail with ENOENT, which Git counts as deleted.
GTree(a) ==
  /\ g[a].step = "tree"
  /\ IF Exists(P(a))
     THEN \/ \E t \in Shrink(w.tree) :
               /\ w' = [w EXCEPT !.tree = t]
               /\ hist' = IF w.tree = "whole"
                             /\ ~(chk[a].path = P(a) /\ chk[a].clean)
                          THEN [hist EXCEPT !.unchecked = TRUE] ELSE hist
               /\ g' = IF t = "gone" THEN [g EXCEPT ![a].step = "gitdir"] ELSE g
          \/ /\ g' = [g EXCEPT ![a].step = "gitdir", ![a].err = TRUE]
             /\ UNCHANGED <<w, hist>>
     ELSE /\ g' = [g EXCEPT ![a].step = "gitdir"]
          /\ UNCHANGED <<w, hist>>
  /\ UNCHANGED <<pc, chk, ctx, ts, rs, faults>>

\* delete_git_dir(wt->id): by id, wherever W is now. Git runs it even when
\* deleting the files failed, then exits.
GGitdir(a) ==
  /\ g[a].step = "gitdir"
  /\ w' = [w EXCEPT !.gitdir = "gone"]
  /\ hist' = IF w.at = "orig" THEN [hist EXCEPT !.lostBack = TRUE] ELSE hist
  /\ GitExit(a, IF g[a].err THEN "err" ELSE "ok")
  /\ UNCHANGED <<pc, chk, ctx, ts, rs, faults>>

\* git.rs REMOVE_TIMEOUT: after 30 minutes residue kills Git and goes on
\* with the error.
GitTimeout(a) ==
  /\ "timeout" \in Faults /\ faults = 0
  /\ g[a].step \in {"status", "tree"}
  /\ GitExit(a, "err")
  /\ faults' = faults + 1
  /\ UNCHANGED <<w, pc, chk, ctx, ts, rs, hist>>

\* Ctrl-C at a terminal, or another signal to residue's process group:
\* residue and its Git child stop together. A kill during delete_git_dir
\* leaves part of the Git directory.
Kill(a) ==
  /\ "kill" \in Faults /\ faults = 0
  /\ Running(a)
  /\ pc' = [pc EXCEPT ![a] = "dead"]
  /\ faults' = faults + 1
  /\ IF a \in D /\ g[a].step = "gitdir"
     THEN \E gd \in {"ok", "part"} :
            /\ w' = [w EXCEPT !.gitdir = gd]
            /\ hist' = IF gd = "part" /\ w.at = "orig"
                       THEN [hist EXCEPT !.lostBack = TRUE] ELSE hist
     ELSE UNCHANGED <<w, hist>>
  /\ g' = IF a \in D
          THEN [g EXCEPT ![a] = [step |-> "none", err |-> FALSE, res |-> "none"]]
          ELSE g
  /\ UNCHANGED <<chk, ctx, ts, rs>>

\* A signal to residue's PID alone: `kill PID`, a Force Quit, or watch's
\* 30-minute deadline on the `residue remove` it runs (run.rs stop kills
\* only that PID). residue sets no signal handler, so its `git worktree
\* remove` child is left running and goes on. It writes nothing unless it
\* fails, so the closed pipe does not stop it.
Orphan(a) ==
  /\ "orphan" \in Faults /\ faults = 0
  /\ a \in D /\ Running(a) /\ g[a].step # "none"
  /\ pc' = [pc EXCEPT ![a] = "dead"]
  /\ faults' = faults + 1
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, hist>>

----------------------------------------------------------------------------
(* tr: restore.rs restore_trash and trash.rs restore                       *)

\* restore.rs gather: one `git worktree list`, then the trash entries and
\* the removal records. A record is interrupted when Git still lists W at
\* its aside path.
TList ==
  /\ pc["tr"] = "t_list" /\ Waits("tr")
  /\ w.entry = "info"
  /\ ts' = [reg |-> Listed("trash"), lock |-> w.lock,
            recs |-> {r \in w.records : Interrupted(r)}]
  /\ Go("tr", "t_check")
  /\ UNCHANGED <<w, g, chk, ctx, rs, faults, hist>>

\* trash::restore's checks: holds_worktree() reads the disk now; the lock
\* is `registered.locked` from the listing; nothing at the original path.
TCheck ==
  /\ pc["tr"] = "t_check"
  /\ Go("tr", IF Exists("trash") /\ ts.reg /\ ts.lock \in {"none", "trash"}
                 /\ ~Exists("orig")
              THEN IF ts.lock # "none" THEN "t_unlock" ELSE "t_move"
              ELSE "done")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults, hist>>

\* `if was_locked { git::unlock_worktree }`, from the listing's lock.
TUnlock ==
  /\ pc["tr"] = "t_unlock"
  /\ IF Listed("trash") /\ w.lock # "none"
     THEN /\ w' = [w EXCEPT !.lock = "none"]
          /\ hist' = Lifted
          /\ Go("tr", "t_move")
     ELSE /\ Go("tr", "done")
          /\ UNCHANGED <<w, hist>>
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults>>

\* git::move_worktree(worktree, dest); on failure lock it again if it was.
TMove ==
  /\ pc["tr"] = "t_move"
  /\ IF Movable("trash")
     THEN /\ w' = [w EXCEPT !.at = "orig"]
          /\ chk' = NoChecks
          /\ Go("tr", "t_discard")
     ELSE /\ Go("tr", IF ts.lock # "none" THEN "t_relock" ELSE "done")
          /\ UNCHANGED <<w, chk>>
  /\ UNCHANGED <<g, ctx, ts, rs, faults, hist>>

TRelock ==
  /\ pc["tr"] = "t_relock"
  /\ w' = IF Listed("trash") /\ w.lock = "none"
          THEN [w EXCEPT !.lock = "trash"] ELSE w
  /\ Go("tr", "done")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* trash::discard(&item.dir).
TDiscard ==
  /\ pc["tr"] = "t_discard"
  /\ w' = [w EXCEPT !.entry = "none"]
  /\ Go("tr", "t_forget")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* restore_trash: removed::forget each listed record that was interrupted
\* and whose aside is this entry's worktree.
TForget ==
  /\ pc["tr"] = "t_forget"
  /\ w' = [w EXCEPT !.records = @ \ (ts.recs \cap {"pg"})]
  /\ Go("tr", "t_report")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* The restore reports W back at its path, with no note that part is gone.
TReport ==
  /\ pc["tr"] = "t_report"
  /\ hist' = IF Listed("orig") /\ w.gitdir = "ok" /\ w.tree = "whole"
             THEN hist ELSE [hist EXCEPT !.lied = TRUE]
  /\ Go("tr", "done")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults>>

----------------------------------------------------------------------------
(* rr: removed.rs restore of one removal record                            *)

\* restore.rs gather and choose: the record, whether Git lists W at its
\* aside (registered), and is_back, which is read only when it does not.
SList ==
  /\ pc["rr"] = "s_list" /\ Waits("rr")
  /\ \E r \in w.records :
       rs' = [rec |-> r, reg |-> Interrupted(r),
              back |-> ~Interrupted(r) /\ Listed("orig") /\ w.gitdir = "ok"
                       /\ DotGit,
              lock |-> w.lock, how |-> "none"]
  /\ Go("rr", "s_dest")
  /\ UNCHANGED <<w, g, chk, ctx, ts, faults, hist>>

\* Something at the destination stops it, unless the record is back.
SDest ==
  /\ pc["rr"] = "s_dest"
  /\ Go("rr", IF Exists("orig") /\ ~rs.back THEN "done" ELSE "s_copies")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults, hist>>

\* read_copies: another run forgot the record since the listing.
SCopies ==
  /\ pc["rr"] = "s_copies"
  /\ Go("rr", IF rs.rec \notin w.records THEN "done"
              ELSE IF rs.back THEN "s_after"
              ELSE IF rs.reg THEN "s_moveback"
              ELSE "s_recreate")
  /\ rs' = IF rs.back THEN [rs EXCEPT !.how = "already"] ELSE rs
  /\ UNCHANGED <<w, g, chk, ctx, ts, faults, hist>>

\* move_back's refusals: a trash aside; the PID in the aside name still
\* runs (Leftover::running); the directory is gone; another's lock.
SMoveBack ==
  /\ pc["rr"] = "s_moveback"
  /\ Go("rr", IF rs.rec = "pg" THEN "done"
              ELSE IF Running("rm") /\ ~Mut("MoveBackWhileRunning") THEN "done"
              ELSE IF ~Exists("aside") THEN "done"
              ELSE IF rs.lock \notin {"none", "removing"} THEN "done"
              ELSE IF rs.lock = "removing" THEN "s_unlock"
              ELSE "s_move")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults, hist>>

SUnlock ==
  /\ pc["rr"] = "s_unlock"
  /\ IF Listed("aside") /\ w.lock # "none"
     THEN /\ w' = [w EXCEPT !.lock = "none"]
          /\ hist' = Lifted
          /\ Go("rr", "s_move")
     ELSE /\ Go("rr", "done")
          /\ UNCHANGED <<w, hist>>
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults>>

SMove ==
  /\ pc["rr"] = "s_move"
  /\ IF Movable("aside")
     THEN /\ w' = [w EXCEPT !.at = "orig"]
          /\ chk' = NoChecks
          /\ Go("rr", "s_after")
          /\ rs' = [rs EXCEPT !.how = "moved"]
     ELSE /\ Go("rr", IF rs.lock = "removing" THEN "s_relock" ELSE "done")
          /\ UNCHANGED <<w, chk, rs>>
  /\ UNCHANGED <<g, ctx, ts, faults, hist>>

SRelock ==
  /\ pc["rr"] = "s_relock"
  /\ w' = IF Listed("aside") /\ w.lock = "none"
          THEN [w EXCEPT !.lock = "removing"] ELSE w
  /\ Go("rr", "done")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* recreate: `git worktree add` at the path from the record's HEAD, a new
\* Git directory with the saved reflog put back. Only once Git lists W
\* nowhere; then no deletion of W is still running.
SRecreate ==
  /\ pc["rr"] = "s_recreate"
  /\ IF w.gitdir = "gone" /\ ~Exists("orig")
     THEN /\ w' = [w EXCEPT !.at = "orig", !.tree = "whole", !.gitdir = "ok",
                            !.gen = 1, !.lock = "none"]
          /\ chk' = NoChecks
          /\ Go("rr", "s_after")
          /\ rs' = [rs EXCEPT !.how = "recreated"]
     ELSE /\ Go("rr", "done")
          /\ UNCHANGED <<w, chk, rs>>
  /\ UNCHANGED <<g, ctx, ts, faults, hist>>

\* holds, then forget: the record goes once the worktree holds its commits.
SAfter ==
  /\ pc["rr"] = "s_after"
  /\ w' = IF Listed("orig") /\ w.gitdir = "ok"
          THEN [w EXCEPT !.records = @ \ {rs.rec}] ELSE w
  /\ Go("rr", "s_report")
  /\ UNCHANGED <<g, chk, ctx, ts, rs, faults, hist>>

\* move_back always notes that part of it may be gone.
SReport ==
  /\ pc["rr"] = "s_report"
  /\ hist' = IF Listed("orig") /\ w.gitdir = "ok"
                /\ (w.tree = "whole"
                    \/ (rs.how = "moved" /\ ~Mut("MoveBackNoNote")))
             THEN hist ELSE [hist EXCEPT !.lied = TRUE]
  /\ Go("rr", "done")
  /\ UNCHANGED <<w, g, chk, ctx, ts, rs, faults>>

----------------------------------------------------------------------------
(* The user, and their coding agents                                       *)

UserEdits ==
  /\ w.edits = 0 /\ w.tree # "gone"
  /\ w' = [w EXCEPT !.edits = 1]
  /\ UNCHANGED <<pc, g, chk, ctx, ts, rs, faults, hist>>

UserStartsProcess ==
  /\ ~w.busy /\ w.tree # "gone"
  /\ w' = [w EXCEPT !.busy = TRUE]
  /\ UNCHANGED <<pc, g, chk, ctx, ts, rs, faults, hist>>

\* `git worktree lock`; the user never unlocks residue's own lock.
UserLocks ==
  /\ ~w.ulock /\ w.lock = "none" /\ w.gitdir # "gone"
  /\ w' = [w EXCEPT !.lock = "user", !.ulock = TRUE]
  /\ UNCHANGED <<pc, g, chk, ctx, ts, rs, faults, hist>>

----------------------------------------------------------------------------

Idle(a) ==
  \/ pc[a] \in {"done", "dead", "off"}
  \/ (pc[a] = "s_list" /\ w.records = {})
  \/ (pc[a] = "t_list" /\ w.entry # "info")

Finished ==
  /\ \A a \in Actors : Idle(a)
  /\ \A a \in D : g[a].step = "none"
  /\ UNCHANGED vars

Next ==
  \/ RRecheck \/ RMove \/ RLock \/ RRecord \/ RVerify \/ RUnlock \/ RAfter
  \/ PBCheck \/ PBUnlock \/ PBMove \/ PBStuck \/ RSettle
  \/ PRecheck \/ PRecord \/ PUnlock \/ PAfter \/ PRelock \/ PSettle
  \/ \E a \in D : GFind(a) \/ GExists(a) \/ GStatus(a) \/ GTree(a)
                  \/ GGitdir(a) \/ GitTimeout(a)
  \/ \E a \in Actors : Kill(a) \/ Orphan(a)
  \/ TList \/ TCheck \/ TUnlock \/ TMove \/ TRelock \/ TDiscard \/ TForget
  \/ TReport
  \/ SList \/ SDest \/ SCopies \/ SMoveBack \/ SUnlock \/ SMove \/ SRelock
  \/ SRecreate \/ SAfter \/ SReport
  \/ UserEdits \/ UserStartsProcess \/ UserLocks
  \/ Finished

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* The rules                                                               *)

\* Nothing of W is deleted while it is at its original path.
NotDeletedOnceBack == ~hist.lostBack

\* Git deletes W's files only after residue's last check passed on W at
\* the path Git deletes, with W unchanged and idle, and no move since.
OnlyCheckedStateDeleted == ~hist.unchecked

\* The commits only W's HEAD reflog holds stay held: by W's Git directory,
\* or by a removal record's refs.
ReflogCommitsKept == w.gitdir = "ok" \/ w.records # {}

\* A restore that reports success leaves W listed at its path with its Git
\* directory, and whole unless the report says part may be gone.
RestoreTellsTheTruth == ~hist.lied

\* residue lifts only its own locks.
OnlyOwnLocksLifted == ~hist.lockLifted

\* A removal record is dropped only while nothing of W is gone.
RecordDroppedOnlyWhenWhole == ~hist.undoPart

TypeOK ==
  /\ w.at \in {"orig", "aside", "trash"}
  /\ w.tree \in {"whole", "part", "nogit", "gone"}
  /\ w.gitdir \in {"ok", "part", "gone"}
  /\ w.lock \in {"none", "removing", "trash", "user"}
  /\ w.records \subseteq D
  /\ faults \in 0..1
=============================================================================
