// Removal records in words, as the residue CLI prints them, so the window
// and `residue restore` read the same.

import { bytes, grouped } from "./format";
import type { NotKept } from "./types";

/**
 * Who removed a worktree and why, after "removed 3 days ago": residue's own
 * words (`removed::Reason::describe` at the repository root). A reason a
 * newer residue writes is shown as it is.
 */
export function removedBy(reason: string | null): string {
  switch (reason) {
    case "remove":
      return "by residue remove";
    case "sweep":
      return "by a residue remove --all sweep";
    case "watch_landed":
      return "by residue watch: its upstream branch was deleted";
    case "watch_low_disk":
      return "by residue watch: the disk was low";
    case "trash_purge":
      return "from the trash, by its purge after --keep-trash";
    case null:
      return "";
    default:
      return `(${reason})`;
  }
}

/** `node_modules/ (1,204 files, 180 MB)`, as `residue restore` lists it. */
export function sized(n: NotKept): string {
  return n.files != null && n.bytes != null
    ? `${n.path} (${grouped(n.files)} ${n.files === 1 ? "file" : "files"}, ${bytes(n.bytes)})`
    : n.path;
}
