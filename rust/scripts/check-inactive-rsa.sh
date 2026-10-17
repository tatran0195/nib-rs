#!/usr/bin/env bash
# RUSTSEC-2023-0071 is temporarily ignored only because the rsa package is
# retained as inactive optional SQLx lockfile content. This guard fails if cargo
# resolves rsa into any Nibleaf workspace target. It accepts cargo's two
# version-dependent no-path outcomes: an empty tree (success) or an unmatched
# package specification (non-zero).
set -euo pipefail

set +e
output=$(cargo tree --locked --workspace --target all -i rsa 2>&1)
status=$?
set -e

if printf '%s\n' "$output" | grep -Eq '(^|[[:space:]])rsa v'; then
  printf '%s\n' "$output" >&2
  echo 'rsa resolves into a Nibleaf workspace target; remove the audit exception.' >&2
  exit 1
fi

if [[ $status -eq 0 ]] && { [[ -z "$output" ]] || [[ "$output" == *'nothing to print'* ]]; }; then
  echo 'rsa target-path guard: no workspace target path'
  exit 0
fi

if [[ $status -ne 0 ]] && [[ "$output" == *'did not match any packages'* ]]; then
  echo 'rsa target-path guard: no workspace target path'
  exit 0
fi

printf '%s\n' "$output" >&2
echo 'cargo tree failed unexpectedly while checking the inactive rsa package.' >&2
exit 1
