# Shared Harness Baseline

## Purpose
This file records which shared Harness kit baseline this project instance was last bootstrapped or updated from.

It is a stable repository-local reference for later incremental update work.
It does not replace the law layer.

## Usage Rule
- Create or refresh this file at bootstrap time.
- Update it again whenever a shared-kit update is merged into the project instance.
- Prefer a git commit SHA or tag as the baseline identity.
- If both are available, record both.
- If an exact prior baseline is unknown, say so explicitly rather than inventing one.

## Required Fields
- Shared source repository
- Baseline commit SHA
- Baseline tag if available
- Baseline recorded on
- Recorded by
- Notes

## Baseline Record Template
- Shared source repository: `https://github.com/<owner>/<repo>.git`
- Baseline commit SHA: `<commit sha>`
- Baseline tag: `<tag or unknown>`
- Baseline recorded on: `<YYYY-MM-DD>`
- Recorded by: `<bootstrap|update|manual recovery>`
- Notes:
  - `<optional note about uncertainty, fallback comparison, or recovery context>`
