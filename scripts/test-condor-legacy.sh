#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

suite="${1:-cw}"

: "${CW_PARITY_LEGACY_ROOT:?set CW_PARITY_LEGACY_ROOT to the Condor repo root}"

if [[ -z "${CW_PARITY_RUST_BIN:-}" ]]; then
    cargo build --quiet --bin cw
    CW_PARITY_RUST_BIN="$REPO_ROOT/target/debug/cw"
    export CW_PARITY_RUST_BIN
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scripts"

case "$suite" in
    cw)
        cp "$CW_PARITY_LEGACY_ROOT/scripts/test-cw.sh" "$tmp/scripts/test-cw.sh"
        cp "$CW_PARITY_LEGACY_ROOT/scripts/test-lib.sh" "$tmp/scripts/test-lib.sh"
        cp "$REPO_ROOT/scripts/compat/condor/cw.sh" "$tmp/scripts/cw.sh"
        cp "$REPO_ROOT/scripts/compat/condor/worktree-lib.sh" "$tmp/scripts/worktree-lib.sh"
        exec bash "$tmp/scripts/test-cw.sh"
        ;;
    *)
        echo "unknown suite: $suite" >&2
        exit 2
        ;;
esac
