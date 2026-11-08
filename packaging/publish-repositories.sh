#!/usr/bin/env bash
# Publish the signed package repositories to a GitHub Pages site repository.
#
#   packaging/publish-repositories.sh DIR REMOTE BRANCH FOLDER MESSAGE
#
# DIR holds the repositories: apt/, arch/, rpm/ and residue-signing-key.asc,
# and nothing else. The script replaces FOLDER on BRANCH of the Git
# repository REMOTE with DIR and keeps every other file there. It pushes one
# commit with no parent, so BRANCH keeps no history and the packages of
# earlier releases do not pile up in it. The force push carries a lease on
# the commit it replaces: if anyone pushed to BRANCH since the clone, it
# fails and loses nothing. Git reaches REMOTE however the caller set it up,
# for example with GIT_SSH_COMMAND, and commits as the caller's Git identity.
# Needs git.
set -euo pipefail

if [ $# -ne 5 ]; then
  echo "usage: $0 DIR REMOTE BRANCH FOLDER MESSAGE" >&2
  exit 2
fi

src=$(cd "$1" && pwd)
remote=$2
branch=$3
folder=$4
message=$5
fail() { echo "$0: $*" >&2; exit 1; }

case $folder in
  '' | . | .. | */* | .git) fail "FOLDER must be one plain folder name, not '$folder'" ;;
esac

# Only the repositories and their key: nothing else in the folder is meant
# to be public.
entries=$(cd "$src" && LC_ALL=C ls -A)
[ "$entries" = "$(printf '%s\n' apt arch residue-signing-key.asc rpm)" ] ||
  fail "$src must hold exactly apt, arch, residue-signing-key.asc and rpm, and holds: $(printf '%s' "$entries" | tr '\n' ' ')"
# A Pages site published from a branch does not serve symbolic links.
links=$(find "$src" -type l)
[ -z "$links" ] || fail "symbolic links cannot be served: $links"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone --quiet --depth 1 --single-branch --branch "$branch" --no-tags "$remote" "$work"
old=$(git -C "$work" rev-parse HEAD)

git -C "$work" rm -r -q --ignore-unmatch -- "$folder"
rm -rf "${work:?}/$folder"
cp -R "$src" "$work/$folder"
# --force: a .gitignore in the site repository must not drop a file.
git -C "$work" add --all --force -- "$folder"
tree=$(git -C "$work" write-tree)

# Nothing outside FOLDER changed, and FOLDER holds exactly DIR's files, byte
# for byte: a .gitattributes there, such as an LFS or line-ending rule,
# must not change one.
git -C "$work" diff --quiet "$old" "$tree" -- ":(exclude)$folder" ||
  fail "the commit would change files outside $folder/: $(git -C "$work" diff --name-only "$old" "$tree" -- ":(exclude)$folder" | tr '\n' ' ')"
files=$(cd "$src" && find . -type f | sed 's|^\./||' | LC_ALL=C sort)
want=$(while IFS= read -r file; do
  printf '%s %s\n' "$(git -C "$work" hash-object --no-filters -- "$src/$file")" "$file"
done <<< "$files")
have=$(git -C "$work" -c core.quotePath=false ls-tree -r --format='%(objectname) %(path)' "$tree" -- "$folder" |
  sed "s| $folder/| |" | LC_ALL=C sort -k 2)
[ "$want" = "$have" ] || fail "the commit's $folder/ does not hold exactly the files in $src"

new=$(git -C "$work" commit-tree "$tree" -m "$message")
git -C "$work" push --quiet --force-with-lease="refs/heads/$branch:$old" origin "$new:refs/heads/$branch"
echo "$remote $branch: $old -> $new, $(printf '%s\n' "$want" | wc -l | tr -d ' ') files in $folder/"
