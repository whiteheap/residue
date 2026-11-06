# Security

residue deletes directories. A bug that lets it remove a worktree holding uncommitted, untracked or non-regenerable ignored data, a worktree in use, or anything outside the worktree is a security issue. Please report it privately. Do not open a public issue for it.

Report through GitHub private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Include the residue version, the operating system, the Git version, and the steps that reproduce it.

residue is not a security boundary between users. It runs as you, and assumes that the other processes running as you are not trying to fool it.
