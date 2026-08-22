#!/bin/sh
# One-time developer setup: point git at the repo's pre-commit hook so
# unformatted Rust commits are blocked at commit time.
#
# Why this exists: git hooks live in .git/hooks (local, per-clone, never
# shared). To make the pre-commit hook travel with the repo, we keep the
# script at scripts/hooks/pre-commit and point core.hooksPath at it. That
# config is local too, so each clone must run this once after checkout.
#
# Usage:  sh scripts/setup.sh   (or: source scripts/setup.sh — both work)

set -u

# Resolve repo root even when sourced from elsewhere.
# When sourced, $0 is the shell ("/usr/bin/bash") and BASH_SOURCE is this
# script; when executed, $0 is this script and BASH_SOURCE may be unset.
_self="${BASH_SOURCE:-$0}"
case "$_self" in
  /*) _script="$_self" ;;
  *) _script="$(pwd)/$_self" ;;
esac
_root="$(cd "$(dirname "$_script")/.." && pwd)"

# Fall back to git's own notion of the root if our guess missed.
if ! cd "$_root" 2>/dev/null || ! git rev-parse --show-toplevel >/dev/null 2>&1; then
  _root="$(git rev-parse --show-toplevel 2>/dev/null)"
  [ -z "$_root" ] && { echo "setup: not in a git repo" >&2; return 1 2>/dev/null || exit 1; }
  cd "$_root" || { echo "setup: cannot cd to $_root" >&2; return 1 2>/dev/null || exit 1; }
fi

# 1. Point git at the tracked hook directory.
if [ ! -d "scripts/hooks" ]; then
  echo "setup: scripts/hooks not found — repo layout changed?" >&2
  return 1 2>/dev/null || exit 1
fi

git config core.hooksPath scripts/hooks
echo "setup: core.hooksPath = $(git config core.hooksPath)"

# 2. Verify the toolchain the hook relies on is reachable.
_miss=0
if ! command -v cargo >/dev/null 2>&1; then
  echo "setup: cargo not on PATH — install Rust (https://rustup.rs)" >&2
  _miss=1
fi
if ! command -v rustfmt >/dev/null 2>&1; then
  echo "setup: rustfmt not on PATH — run: rustup component add rustfmt" >&2
  _miss=1
fi

if [ "$_miss" -ne 0 ]; then
  echo "setup: toolchain incomplete, hook will fail until fixed" >&2
  return 1 2>/dev/null || exit 1
fi

echo "setup: cargo -> $(command -v cargo)"
echo "setup: rustfmt -> $(command -v rustfmt)"
echo "setup: done. pre-commit will block unformatted .rs; bypass with --no-verify."

unset _self _script _root _miss
