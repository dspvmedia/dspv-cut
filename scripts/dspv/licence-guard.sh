#!/bin/bash
# Fails when upstream's licence files changed since the baseline in .dspv/licence-baseline.txt.
# Usage: scripts/dspv/licence-guard.sh [--update]   (run with --update only after a person has read the changes)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
ref="${UPSTREAM_REF:-upstream-main}"
files=$(git ls-tree -r --name-only "$ref" | grep -E '^(LICENSE|NOTICE|ATTRIBUTION)' | sort)
now=$( { for f in $files; do printf '%s  %s\n' "$(git show "$ref:$f" | shasum -a 256 | cut -d' ' -f1)" "$f"; done
         printf 'cargo-licence  %s\n' "$(git show "$ref:Cargo.toml" | grep -E '^license' | sort -u | tr -d '\n')"; } )
if [ "${1:-}" = "--update" ]; then printf '%s\n' "$now" > .dspv/licence-baseline.txt; echo "baseline updated"; exit 0; fi
if [ ! -f .dspv/licence-baseline.txt ]; then echo "no baseline; run with --update after reading the licence files"; exit 2; fi
if diff <(printf '%s\n' "$now") .dspv/licence-baseline.txt >/dev/null; then echo "licence files unchanged"; else
  echo "UPSTREAM LICENCE FILES CHANGED. Read the diff, then re-baseline with --update:"; diff <(printf '%s\n' "$now") .dspv/licence-baseline.txt || true; exit 1; fi
