#!/usr/bin/env bash
# Applies sections 1 and 2 of docs/launch/github-settings.md: the default branch
# ruleset and the Actions settings. Meant for after the repository is public.
#
#   scripts/apply-github-settings.sh          # dry run: print every call
#   scripts/apply-github-settings.sh --yes    # apply them
#
# The repository is GH_REPO (owner/name), else what `gh repo view` finds here.
set -euo pipefail

apply=0
for arg in "$@"; do
  case "$arg" in
    --yes) apply=1 ;;
    -h | --help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg (only --yes)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

RULESET="default branch"
# The `name:` of every job in .github/workflows/ci.yml that runs on pull requests.
CHECKS=(
  "Changes"
  "Rust (fmt, clippy, tests, API spec drift)"
  "Runner and desktop (TypeScript)"
  "Dependency audit"
)

# pull_request_target runs with secrets and a write token, so it must never run PR code.
# shortcut: a text match, not a YAML parse; enough while the workflows are few and hand-written.
for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
  [ -f "$wf" ] || continue
  grep -q pull_request_target "$wf" || continue
  if grep -nE 'pull_request\.head\.|head_ref|merge_commit_sha|refs/pull/|pull/.*/(head|merge)|gh pr checkout' "$wf" >&2; then
    echo "refusing: $wf runs on pull_request_target and refers to the PR head (lines above)" >&2
    exit 1
  fi
done

for check in "${CHECKS[@]}"; do
  if ! grep -qxF "    name: $check" .github/workflows/ci.yml; then
    echo "ci.yml has no job named '$check'; make the ci.yml change in docs/launch/github-settings.md (c) first" >&2
    exit 1
  fi
done

repo=${GH_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
if ! [[ $repo =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
  echo "bad repository: '$repo' (set GH_REPO=owner/name)" >&2
  exit 1
fi
visibility=$(gh api "repos/$repo" --jq .visibility)
if [ "$visibility" != public ]; then
  if [ "$apply" -eq 1 ]; then
    echo "refusing: $repo is $visibility; these settings are for after it is public" >&2
    exit 1
  fi
  echo "note: $repo is $visibility; --yes refuses until it is public" >&2
fi

call() { # METHOD PATH [JSON BODY]
  if [ "$apply" -ne 1 ]; then
    echo "would call: $1 $2"
    if [ $# -gt 2 ]; then printf '%s\n' "$3"; fi
    return 0
  fi
  if [ $# -gt 2 ]; then
    printf '%s' "$3" | gh api -X "$1" "$2" --input - >/dev/null
  else
    gh api -X "$1" "$2" >/dev/null
  fi
  echo "done: $1 $2"
}

contexts=$(printf '{"context": "%s"}, ' "${CHECKS[@]}")
contexts=${contexts%, }
# actor_id 5 is the base "admin" repository role; it may bypass on pull requests only.
ruleset=$(cat <<JSON
{
  "name": "$RULESET",
  "target": "branch",
  "enforcement": "active",
  "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
  "bypass_actors": [
    {"actor_type": "RepositoryRole", "actor_id": 5, "bypass_mode": "pull_request"}
  ],
  "rules": [
    {"type": "deletion"},
    {"type": "non_fast_forward"},
    {"type": "required_linear_history"},
    {"type": "pull_request", "parameters": {
      "required_approving_review_count": 1,
      "dismiss_stale_reviews_on_push": true,
      "require_code_owner_review": true,
      "require_last_push_approval": false,
      "required_review_thread_resolution": false,
      "allowed_merge_methods": ["squash", "rebase"]
    }},
    {"type": "required_status_checks", "parameters": {
      "strict_required_status_checks_policy": false,
      "required_status_checks": [$contexts]
    }}
  ]
}
JSON
)

if [ "$visibility" != public ]; then
  # Rulesets may be unavailable while the repository is private; only a dry run gets here.
  echo "would call: POST repos/$repo/rulesets (PUT repos/$repo/rulesets/<id> if '$RULESET' exists)"
  printf '%s\n' "$ruleset"
else
  id=$(gh api --paginate "repos/$repo/rulesets" --jq ".[] | select(.name == \"$RULESET\") | .id")
  case $id in
    *$'\n'*) echo "several rulesets are named '$RULESET'; delete the extra ones first" >&2; exit 1 ;;
    "") call POST "repos/$repo/rulesets" "$ruleset" ;;
    *) call PUT "repos/$repo/rulesets/$id" "$ruleset" ;;
  esac
fi

call PUT "repos/$repo/actions/permissions/workflow" \
  '{"default_workflow_permissions": "read", "can_approve_pull_request_reviews": false}'
call PUT "repos/$repo/actions/permissions/fork-pr-contributor-approval" \
  '{"approval_policy": "all_external_contributors"}'

if [ "$apply" -ne 1 ]; then
  echo
  echo "Dry run: nothing was changed. Run again with --yes to apply."
fi
