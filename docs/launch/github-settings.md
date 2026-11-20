# GitHub settings for the public repository

Apply these after the repository is public. `scripts/apply-github-settings.sh`
does sections 1 and 2: it prints every call by default and applies them with
`--yes`. The repository comes from `GH_REPO` (`owner/name`), else from
`gh repo view`. Make the `ci.yml` change in (c) first: the script refuses
while a required job name is missing from `ci.yml`.

API endpoints and field names were checked against GitHub's REST docs on
2026-11-20; see "Not verified" at the end for what was not.

## 1. Ruleset on the default branch

One branch ruleset, `default branch`, on `~DEFAULT_BRANCH`, enforcement
`active` (`POST /repos/{owner}/{repo}/rulesets`, or
`PUT /repos/{owner}/{repo}/rulesets/{ruleset_id}` when it exists).

| Rule (`type`) | Setting |
| --- | --- |
| `deletion` | The branch cannot be deleted. |
| `non_fast_forward` | No force pushes. |
| `pull_request` | `required_approving_review_count: 1`, `dismiss_stale_reviews_on_push: true`, `require_code_owner_review: true` (`.github/CODEOWNERS` is `* @byQuexo`), `require_last_push_approval: false`, `required_review_thread_resolution: false`, `allowed_merge_methods: ["squash", "rebase"]` |
| `required_status_checks` | `Changes`, `Rust (fmt, clippy, tests, API spec drift)`, `Runner and desktop (TypeScript)`, `Dependency audit`; `strict_required_status_checks_policy: false` |
| `required_linear_history` | No merge commits on the branch. |

Bypass: `{"actor_type": "RepositoryRole", "actor_id": 5, "bypass_mode": "pull_request"}`
(the repository admin role).

The required checks are every job in `.github/workflows/ci.yml` that runs on
`pull_request`, by their `name:`. The strict policy ("branch must be up to
date") stays off: it would force an update and a fresh CI run before every
merge. No `integration_id` is set, so a check of that name from any source
counts; outside contributors cannot post checks to this repository.

### Consequences

**(a) Linear history forbids merge commits.** The repository merges with merge
commits today ("Merge pull request #20 ..."). Switch to squash merges before
the ruleset goes on: Settings → General → Pull Requests → untick **Allow merge
commits**, keep **Allow squash merging** (and **Allow rebase merging** if you
want it), and set the squash default commit message to **Pull request title**
so the Conventional Commit title becomes the commit on `main`. Over the API
that is `PATCH /repos/{owner}/{repo}` with `"allow_merge_commit": false`. The
ruleset's `allowed_merge_methods` leaves out `merge` as well.

**(b) A solo maintainer cannot approve their own pull request.** GitHub never
counts the author's own review, so with one required approval nothing the
maintainer opens could merge. The bypass for the repository admin role
(`actor_id: 5`, `bypass_mode: pull_request`) fixes that:

- An admin can merge a pull request without the approval: the merge box offers
  a checkbox to merge without waiting for the requirements (bypass rules).
- The bypass covers the whole ruleset, not just the approval. Ticking that box
  also merges with failing or missing checks, so only use it on green PRs.
- `pull_request` mode means admins can bypass on pull requests only. Direct
  pushes, force pushes and deleting `main` stay blocked for them too.
- Anyone later given the admin role gets the same bypass.
- Stricter alternative: two rulesets, the approval rule with the bypass and
  the checks, linear history, deletion and force-push rules without one.

**(c) `paths-ignore` blocks docs-only pull requests.** `ci.yml` skips the whole
workflow when only `docs/**` or `*.md` change. A required check from a skipped
workflow never reports and stays pending ("Expected — Waiting for status to be
reported"), so a docs-only PR can never merge. A job skipped by its `if:`
condition reports "Success" instead (GitHub docs, "Troubleshooting required
status checks"). This is already done in `.github/workflows/ci.yml` (no action needed):

1. Remove the `paths-ignore` block, so the trigger is just `pull_request:` and
   `workflow_dispatch:`. Update the comment at the top ("no docs-only runs").
2. Add this job:

   ```yaml
     changes:
       name: Changes
       runs-on: ubuntu-latest
       timeout-minutes: 5
       outputs:
         code: ${{ steps.diff.outputs.code }}
       steps:
         - uses: actions/checkout@v4
           with:
             fetch-depth: 2
         # On a pull request the checkout is the merge commit; its first parent is the base branch.
         - id: diff
           run: |
             if [ "$GITHUB_EVENT_NAME" != pull_request ]; then
               echo code=true >> "$GITHUB_OUTPUT"
               exit 0
             fi
             files=$(git diff --name-only HEAD^1 HEAD)
             if printf '%s\n' "$files" | grep -qvE '^docs/|\.md$'; then
               echo code=true >> "$GITHUB_OUTPUT"
             else
               echo code=false >> "$GITHUB_OUTPUT"
             fi
   ```

3. Add these two lines to the `rust`, `ts` and `audit` jobs:

   ```yaml
       needs: changes
       if: ${{ !cancelled() && needs.changes.outputs.code != 'false' }}
   ```

4. Leave the `docker` job as it is, and keep every job's `name:` exactly as it
   is: the ruleset requires checks by name, and a renamed job leaves the old
   check pending forever.

Docs-only PRs then run only `Changes` (seconds); `Rust` and `TypeScript` are
skipped and count as passed. The condition fails open: if `Changes` breaks,
its output is empty, the real jobs still run, and `Changes` itself is a
required check that shows the failure. The change detection uses only `git`,
no third-party action.

**(d) The E2E job cannot gate merges.** `E2E (Docker backend)` only runs on
`workflow_dispatch`; on a pull request its `if:` skips it, and a skipped job
counts as passed, so requiring it would add nothing. Run it by hand (Actions →
CI → Run workflow, on the PR's branch) before merging sandbox changes. That
works for branches in this repository, not for forks.

## 2. Actions

| Setting | API |
| --- | --- |
| Require approval for workflow runs from all outside contributors | `PUT /repos/{owner}/{repo}/actions/permissions/fork-pr-contributor-approval` with `{"approval_policy": "all_external_contributors"}` |
| Default `GITHUB_TOKEN` permissions read-only; Actions may not approve PRs | `PUT /repos/{owner}/{repo}/actions/permissions/workflow` with `{"default_workflow_permissions": "read", "can_approve_pull_request_reviews": false}` |

Workflows that need more ask for it in their own `permissions:` block, as
`ci.yml` does with `contents: read`.

Public repositories run Actions on standard GitHub-hosted runners for free
(larger runners are still billed), so the "minutes are scarce" reasoning in
`ci.yml` no longer applies after the switch.

## 3. Policy: `pull_request_target`

A workflow triggered by `pull_request_target` runs with the base repository's
secrets and a write token. It must never check out or run code from the pull
request. The script greps `.github/workflows/` and refuses to apply anything if
a workflow that mentions `pull_request_target` also refers to PR code
(`pull_request.head.`, `head_ref`, `merge_commit_sha`, `refs/pull/`,
`pull/<n>/head` or `/merge`, or `gh pr checkout`). It is a
text match: on a false alarm, read the workflow and fix the match, do not
remove the check. Today no workflow uses `pull_request_target`.

## 4. By hand

The script does not touch these. Turn them on in Settings, or with the API
call shown.

- **Private vulnerability reporting** (needed by `SECURITY.md`): Settings →
  Security. `PUT /repos/{owner}/{repo}/private-vulnerability-reporting`.
  Turn it on right after the switch to public: until then the **Report a
  vulnerability** button that `SECURITY.md` points to does not exist (it is
  most likely available for public repositories only; check).
- **Secret scanning and push protection:** Settings → Security.
  `PATCH /repos/{owner}/{repo}` with
  `{"security_and_analysis": {"secret_scanning": {"status": "enabled"}, "secret_scanning_push_protection": {"status": "enabled"}}}`.
- **Dependabot alerts:** Settings → Security.
  `PUT /repos/{owner}/{repo}/vulnerability-alerts`.
- **Wiki:** Settings → General → Features → Wikis (`"has_wiki": true` on
  `PATCH /repos/{owner}/{repo}`). Create a first page in the browser (a wiki
  has no git repository before that), then publish `docs/wiki/` with
  `scripts/publish-wiki.sh` (dry run first, `--yes` to push).
- **Labels:** `good first issue`, `help wanted`, `bug` and `enhancement` are
  GitHub defaults. The issue drafts in `docs/launch/issues/` also use
  `area: firecracker`, `area: sandbox`, `area: daemon`, `area: runner`,
  `area: providers` and `area: ci`; create those once, e.g.
  `gh label create "area: ci"`.

## Not verified

- `actor_id: 5` for the admin role: GitHub's REST docs do not list role ids.
  The value comes from the terraform-provider-github docs (`maintain` 2,
  `write` 4, `admin` 5). After applying, check that the ruleset's bypass list
  shows "Repository admin".
- That a required check's context is the job's `name:` (not prefixed with the
  workflow name). This is how GitHub names check runs from Actions jobs, but
  it is not on the pages that were checked. After applying, the PR's checks
  list must show the three names as "Required".
- That `allowed_merge_methods` in the ruleset hides the merge-commit button.
- The Settings menu paths above; GitHub moves them from time to time.
- Whether `security_and_analysis` behaves differently for public repositories.
- Nothing in this file or the script has been run against GitHub.
