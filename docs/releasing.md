# Release once, publish verified files

Prepare the stable version in `Cargo.toml`, all seven workspace entries in
`Cargo.lock`, and `docs/release-notes.md` **before** opening the release PR.
Use a branch in this repository; fork PRs can run checks but cannot supply an
automatic release. Review workflow/helper changes as publication-sensitive code.

1. `Release candidate` runs on PRs targeting `main`, with read-only repository
   permission. It checks the Python release helper, formatting and ordinary
   locked Rust tests on Linux x64, Windows x64, macOS arm64 and macOS x64. Each
   job builds release executables, checks their version, packages them, and
   uploads an immutable artifact named with the target, run ID and run attempt.
2. After the PR passes, merge it. `Publish verified release` runs on `main` and
   admits only a version increase belonging to one merged same-repository PR.
   It verifies the successful candidate run and all four required jobs/steps,
   the exact merged Git tree, the candidate's synthetic-merge parents, each
   artifact's GitHub digest, and each archive's inner digest and size.
3. The publisher creates the version tag at that merged commit, uploads a draft
   release, verifies the complete asset set, and publishes it. It does not run
   Cargo, rebuild, retest, unpack the product archives, or execute downloaded
   binaries. Installer and release-note bytes come from the same verified tree.

A merge with no version change does not create a release. A changed merged tree,
missing/expired artifact, incomplete attempt, fork source, wrong tag or differing
existing asset stops publication rather than silently rebuilding or overwriting.
An unchanged successful release is verified read-only. Normal feature merges
therefore do not require a manual tag or a second full CI run.

## Evidence and permissions

`release-provenance.json` records the merged commit/tree, candidate run/attempt,
artifact IDs and hashes. `SHA256SUMS` covers the four archives, both installers
and that provenance file. Artifacts are retained for 30 days; retention is not
permanent storage. The publisher's token has contents-write, actions-read and
pull-requests-read permissions. PR jobs have no release permission. Downloaded
artifact members must be exactly one regular archive and one bounded JSON
manifest; paths, symlinks, extra/duplicate members and mismatches are rejected.

This is a reviewed-repository workflow, not protection against a maintainer who
can merge a malicious workflow. Do not run untrusted PR code with the publisher's
write token. The publisher executes only its checked-out `main` helper.

## Recovery without another test/build

For a publication or upload interruption, rerun the failed publisher. Or run
`Publish verified release` manually **from main**, supplying the full merged
commit SHA in `merge_commit` if main has since advanced. The commit must remain
an ancestor of main. This manual workflow publishes; it is not a test command.

Existing tags must point to the same commit. A matching draft resumes only its
missing uploads; matching published releases are not overwritten. Partial or
different assets fail closed. A lower version cannot replace a newer latest
release. A missing/expired candidate needs an explicit new candidate run/PR
with matching source; the publisher never hides that by rebuilding itself.
When rerunning candidate CI, rerun **all jobs**: artifacts from separate attempts
are deliberately not combined. A head/base change needs a new successful PR
candidate whose tree equals the actual merge. No test bypass is offered.

Publishing a GitHub release does not update any installed Agentlaw instance.

## Official platform behavior

- [PR workflows test the synthetic merge by default](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request).
- [Artifact metadata, SHA-256 digests, expiry and downloads](https://docs.github.com/en/rest/actions/artifacts).
- [Workflow runs and their exact head SHA](https://docs.github.com/en/rest/actions/workflow-runs).
- [Release asset metadata and digests](https://docs.github.com/en/rest/releases/assets).
