# Upstream merge policy

This repo is a fork of [zed-industries/zed](https://github.com/zed-industries/zed) carrying the Android port (Zdroid). We periodically pull upstream changes; this doc describes the policy.

## Remotes

```sh
git remote -v
# origin   git@github.com:Dylanmurzello/zed-android-port.git  (us)
# upstream https://github.com/zed-industries/zed              (Zed)
```

## Files we own, always

Identity + Zdroid-specific docs. Conflicts on these always resolve to OURS via `.gitattributes` (see below):

- `README.md`
- `AGENTS.md`, `GEMINI.md`, `CLAUDE.md`
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` (`*.zed-upstream.md` siblings preserve the upstream copies for reference)
- `BACKLOG.md`, `RELEASING.md`, `UPSTREAM_MERGE.md`
- `.github/ISSUE_TEMPLATE/*`
- `.github/pull_request_template.md`
- `.github/FUNDING.yml`
- `.gitattributes`

Plus all files under our owned crates:

- `crates/gpui_android/**`
- `crates/zdroid_runtime/**`
- `crates/gpui_platform/**` (we added this for the multi-platform dispatch arm)

## Files we modify in-place

The ~54 upstream Zed files where we add cfg-gates, env-aware paths, or other small patches. These use normal 3-way merge. Expect occasional conflicts when upstream refactors them; resolve case-by-case. The current set can be enumerated with:

```sh
git diff --name-only $(git merge-base origin/main upstream/main)..origin/main \
  | grep -vE "^(crates/gpui_android|crates/zdroid_runtime|crates/onboarding|crates/zed_android|crates/gpui_platform)/"
```

When you resolve a conflict in one of these, ALSO check if a workaround doc under `crates/gpui_android/docs/workarounds/` covers the affected file: the doc explains why the patch is there, which makes the resolution obvious.

## Merge driver setup (one-time, per clone)

`.gitattributes` declares `merge=ours` for owned files. Git needs the `ours` driver registered in your local config:

```sh
git config merge.ours.driver true
```

Without that, `merge=ours` is silently ignored and conflicts on identity files re-appear. Verify with:

```sh
git config --get merge.ours.driver  # should print: true
```

## Performing an upstream merge

```sh
git fetch upstream main
git checkout -b upstream-sync-$(date +%Y%m%d)
git merge upstream/main
# expected:
#   - automatic merges on most files
#   - automatic ours-merge on identity files (no prompt)
#   - 3-way conflicts on the ~54 modified upstream files, resolve manually
cargo check --workspace
cd crates/gpui_android/examples/zed_android \
  && cargo ndk --platform 26 -t arm64-v8a check
git checkout main && git merge --no-ff upstream-sync-YYYYMMDD
```

Then push and verify the APK builds end-to-end before the next public release.

## When upstream refactors a file we patch

Worst case: upstream renames or restructures a crate we modify. Two options:

1. Re-port our patch to the new structure (preferred).
2. Drop the patch if upstream's refactor obsoletes our need for it. Rare, but it happens. See e.g. Phase 8b's deletion of `termux_bootstrap.rs` once the bootstrap patches moved to the [zdroid-bootstrap](https://github.com/Dylanmurzello/zdroid-bootstrap) repo.

Either way: update the corresponding workaround doc under `crates/gpui_android/docs/workarounds/`.

## When in doubt

`git log --oneline upstream/main..HEAD` shows everything we've added on top of upstream. If you're unsure whether a file is ours or theirs, that log is the source of truth. The current count is ~50 commits.
