#!/usr/bin/env bash
# Copy docs/wiki/*.md into the repository's GitHub wiki, over SSH only.
#
#   scripts/publish-wiki.sh          # dry run: show what would change
#   scripts/publish-wiki.sh --yes    # commit "docs: update wiki" and push
#
# Owner and repo come from WIKI_OWNER and WIKI_REPO, else from the origin
# remote. Pages that exist only in the wiki are left alone.
set -euo pipefail

push=0
for arg in "$@"; do
  case "$arg" in
    --yes) push=1 ;;
    -h | --help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg (only --yes)" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)

owner=${WIKI_OWNER:-}
repo=${WIKI_REPO:-}
if [ -z "$owner" ] || [ -z "$repo" ]; then
  url=$(git -C "$root" remote get-url origin)
  # git@github.com:o/r.git, ssh://git@github.com/o/r.git, https://github.com/o/r(.git)
  if [[ $url =~ github\.com[:/]([^/]+)/([^/]+)$ ]]; then
    owner=${owner:-${BASH_REMATCH[1]}}
    repo=${repo:-${BASH_REMATCH[2]%.git}}
  else
    echo "origin ($url) is not on github.com; set WIKI_OWNER and WIKI_REPO" >&2
    exit 1
  fi
fi
if ! [[ $owner =~ ^[A-Za-z0-9._-]+$ && $repo =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "bad owner or repo: '$owner/$repo'" >&2
  exit 1
fi

shopt -s nullglob
pages=("$root"/docs/wiki/*.md)
if [ ${#pages[@]} -eq 0 ]; then
  echo "no pages in docs/wiki" >&2
  exit 1
fi

wiki="git@github.com:$owner/$repo.wiki.git"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if ! git clone -q "$wiki" "$tmp/wiki"; then
  echo "could not clone $wiki over SSH." >&2
  echo "Check that your SSH key is loaded (ssh-add -l). A wiki has no git" >&2
  echo "repository until its first page is created on GitHub." >&2
  exit 1
fi

cp -- "${pages[@]}" "$tmp/wiki/"
names=()
for page in "${pages[@]}"; do names+=("$(basename "$page")"); done
git -C "$tmp/wiki" add -- "${names[@]}"

if git -C "$tmp/wiki" diff --cached --quiet; then
  echo "the wiki already matches docs/wiki; nothing to push" >&2
  exit 1
fi

echo "Changes for $wiki:"
git -C "$tmp/wiki" diff --cached --stat

if [ "$push" -ne 1 ]; then
  echo
  echo "Dry run: nothing was pushed. Run again with --yes to push."
  exit 0
fi

git -C "$tmp/wiki" commit -q -m "docs: update wiki"
git -C "$tmp/wiki" push -q origin HEAD
echo "Pushed to $wiki."
