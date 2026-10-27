---
name: land
description: >-
  Land explicitly requested Animist changes by rebasing onto local/main and
  fast-forward pushing to the local remote. Invoke only when the user requests
  landing or merging, including an explicit /land invocation; never invoke
  merely for review, preparation, passing checks, or skill installation.
disable-model-invocation: true
metadata:
  delta-action: land
---

# Land Animist changes

An invocation for landing already authorizes the scoped landing operation.
Proceed through preparation, verification, push, and destination verification.
Do not ask for routine reconfirmation of that request. Stop for actual blockers,
ambiguous scope, unsafe changes, or the separate receiver-configuration permission
described below. Installation of this skill alone does not authorize execution.

## Scope and preflight

1. Read `AGENTS.md` and applicable nested instructions. Inspect the current
   working tree, staged changes, current branch, recent commits, and Git remotes.
   Determine the exact requested files and commits; preserve unrelated work.
   Do not assume an existing commit or a clean source checkout.
2. This workflow targets Animist's `local` remote and `refs/heads/main`.
   The researched local remote is `/Users/akarifur/Projects/animist/.git`;
   its primary checkout is `/Users/akarifur/Projects/animist`. Verify the actual
   fetch/push URLs and destination at execution time. If they have changed,
   establish the intended destination before proceeding.
3. Check for an existing merge/rebase, unresolved conflicts, dirty destination
   worktrees, active hooks, signing requirements, or repository-local receive
   restrictions. Honor applicable contribution/review requirements if added.
   No separate contributor agreement, signing mandate, review mandate, or
   authoritative landing script was found when this skill was created.
   Do not treat that observation as permission to bypass future requirements.
4. Verify that Git, stable Rust compatible with edition 2024, Cargo, rustfmt,
   and Clippy are available. Sources: `Cargo.toml` declares edition 2024;
   `.github/workflows/ci.yml` installs stable Rust and rustfmt/Clippy.
   Do not install tools or expose credentials automatically.
5. Do not push to `origin`, open a pull request, publish a package, create a
   release, or modify branch protections. GitHub publication is not part of
   this local landing workflow. Check destination requirements rather than
   inventing a GitHub review/CI requirement for a local-only push.

## Prepare the intended commits

1. Establish the scoped diff and commits against the destination. If scope is
   unclear, ask rather than including every uncommitted file.
2. Use a new, uniquely named private branch for preparation/rebasing, starting
   from the source change. Do not rewrite an existing shared branch.
   If unrelated dirty work prevents safe preparation, stop; do not silently
   stash, discard, reset, or commit it.
3. Commit necessary scoped uncommitted changes explicitly by file. Preserve
   existing intended commits; do not squash or amend them without a reason
   established by the request. Use conventional commit messages, with an
   appropriate breaking-change marker/footer for breaking APIs or formats.
   Recent project history uses conventional commits.
4. Leave `CHANGELOG.md` to release-plz. The repository owner explicitly requires
   generated changelog management; `.github/workflows/release-plz.yml` defines
   release and release-PR automation. Do not hand-write release entries.
5. Satisfy applicable documentation and test obligations. In particular,
   `AGENTS.md` requires external-crate integration tests for public API changes.
6. Prefix any commit command with `GIT_EDITOR=true` and supply its message
   non-interactively. Never disable signing or hooks to bypass a requirement.

## Rebase and conflicts

1. Fetch the destination without publishing:

   ```sh
   git fetch local
   ```

2. Record the fetched `local/main` tip and the intended commit range. Check for
   already-landed changes before replaying anything. Rebase only the intended
   private commits onto that tip; use an explicit `--onto` boundary when needed
   to avoid replaying unrelated source history.

   ```sh
   GIT_EDITOR=true git rebase local/main
   ```

3. The user's conflict preference is **automatic resolution when the intended
   result is clear**. Inspect both changes and preserve unrelated destination
   work. Resolve clear conflicts, stage only the resolved paths, and continue
   with `GIT_EDITOR=true git rebase --continue`.
4. Pause on ambiguous intent, unsafe resolutions, or failed checks. Report
   conflicting paths and the decision needed; do not blindly select ours/theirs.
   Do not use interactive rebases, destructive resets, or force-pushes.
5. Review the rebased diff and verify it contains only the requested change.

## Verification

All applicable required checks must pass for the exact rebased candidate before
it is pushed. Starting checks, pending results, passing a subset, old results
before conflict resolution, or unverifiable requirements are not success.

For Rust code, manifest, dependency, or build changes, run these exact CI-defined
commands from the project root:

```sh
cargo fmt --all --check
cargo test --all-features
cargo test --no-default-features
cargo check --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Sources: `.github/workflows/ci.yml` lines 17–30 instantiate the two `cargo test`
feature arguments; lines 46–47 define compilation; lines 58–61 define formatting
and strict Clippy. Inspect the current workflow if it changes. The test matrix
covers Ubuntu for both feature sets and macOS for all features. Local execution
does not prove an untested platform passed. If applicable destination policy
requires remote/platform checks, verify all those checks for this candidate;
missing or unavailable results block landing.

For changes strictly limited to Markdown documentation or skill instructions,
review their scope, links, referenced paths, frontmatter where applicable, and
commands against their actual definitions. Do not rebuild Rust solely for prose
changes unless current repository policy requires it. Rust-related changes do
not become docs-only merely because the request also edits documentation.

For every candidate run:

```sh
git diff --check local/main...HEAD
git --no-optional-locks status --short
```

Ensure no unexplained tracked or untracked changes appeared during verification.

Known baseline compile failures were recorded in
`followup/repository-validation.md`, including obsolete `Model` construction,
missing equality derives expected by tests, and old Bedrock identity handling.
Recheck the current state rather than assuming those failures still exist.
Targeted passing tests can diagnose a failure but cannot substitute for a failed
required check. Report the blocker and that the changes have not landed.
Do not repair unrelated baseline issues silently, weaken checks, or claim
success because a failure predates the candidate.

If tools, dependencies, networking, remote checks, or platform requirements are
unavailable, report what remains unverifiable and stop where it blocks required
verification. No live-provider smoke tests or credentials are required by the
current CI definitions.

## Prepare the local receiver safely

1. Inspect the primary checkout and all relevant destination worktrees with
   read-only Git commands. Require a clean destination index and working tree.
   Recheck immediately before the push. Do not modify files directly in that
   checkout or overwrite another person's work.
2. Determine whether `main` is checked out and inspect
   `receive.denyCurrentBranch`. At setup it was unset, so pushing to the
   checked-out `main` would be rejected by Git's default policy.
3. If `main` is checked out and receiving is not already safely configured,
   ask for **explicit permission to enable `updateInstead`** in the primary
   repository. This changes configuration outside the working repository and
   is separate from ordinary landing confirmation. Do not proceed with the
   configuration change until permission is granted.
4. With that permission, set only the verified destination's configuration:

   ```sh
   git --git-dir=/Users/akarifur/Projects/animist/.git config receive.denyCurrentBranch updateInstead
   ```

   Use a different path only if preflight established and the user authorized
   that exact destination. Do not set `ignore`, disable safety hooks, or change
   other receive restrictions. `updateInstead` must reject dirty checkout state;
   never work around such a rejection.
5. If the receiver cannot be updated safely or permission is declined, report
   that the changes have not landed. Do not substitute an origin push or
   manually reset the primary checkout.

## Push and confirm landing

1. Fetch `local` again before pushing. If `local/main` advanced from the tip
   used for verification, rebase the private candidate onto the new tip,
   resolve conflicts under the established policy, and rerun applicable
   verification. Never force an outdated candidate into the destination.
2. Record the final candidate SHA. Perform only a normal fast-forward push:

   ```sh
   git push local HEAD:refs/heads/main
   ```

   A non-fast-forward rejection means the destination moved: fetch, rebase,
   and reverify. Other rejections are blockers to diagnose, not permission
   to bypass protections.
3. Query the actual receiver, not only the local tracking reference:

   ```sh
   git ls-remote local refs/heads/main
   ```

   Its SHA must equal the verified candidate. Confirm the intended commit
   range is reachable from that destination and history remains linear.
4. If `main` is checked out in the primary checkout, verify its `HEAD` matches
   the landed candidate and that both its index and working tree are clean.
   Report a destination mismatch or unexpected dirty state explicitly;
   do not attempt destructive cleanup.
5. Keep unrelated files and branches intact. Do not delete the private branch
   unless safe and requested; report the branch if it remains.
6. Report the destination, landed SHA, verification performed, and final state.
   A prepared commit, completed rebase, or successful source-side check alone
   is not landing. If any required step fails, state that the changes have
   not landed, or distinguish a successful push from a failed post-push
   verification without pretending the push was undone.
