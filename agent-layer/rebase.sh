#!/usr/bin/env bash
# Rebase the agent-layer fork onto upstream/main and prove it still works.
# Usage: agent-layer/rebase.sh [upstream-branch]   (default: main)
#        agent-layer/rebase.sh --check             (only the checks, e.g. after resolving a conflict)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
check_only=false
if [ "${1:-}" = "--check" ]; then check_only=true; shift; fi
branch="${1:-main}"

count_markers() {
  git grep -c "fork:agent" -- ':!FORK.md' ':!agent-layer' ':!crates/agent' ':!crates/agent-ui' ':!apps/deckcraft/src/agent.rs' \
    | awk -F: '{s+=$2} END {print s+0}'
}

if ! $check_only; then
  if [ -n "$(git status --porcelain)" ]; then
    echo "Working tree not clean: commit or stash first." >&2
    exit 1
  fi
  before=$(count_markers)
  git fetch upstream
  echo "Rebasing $(git rev-parse --abbrev-ref HEAD) onto upstream/$branch…"
  # Cargo.lock conflicts on nearly every rebase (upstream bumps dependencies): take upstream's lock
  # and let cargo add our two crates to it, without updating anything else.
  resolve_lock() {
    local files
    files=$(git diff --name-only --diff-filter=U)
    [ "$files" = "Cargo.lock" ] || return 1
    git checkout --ours Cargo.lock   # during a rebase, "ours" is upstream
    cargo metadata --format-version 1 >/dev/null
    git add Cargo.lock
    GIT_EDITOR=true git rebase --continue || true   # a later commit may stop again: the loop sees it
  }
  rebased=true
  git rebase "upstream/$branch" || rebased=false
  while ! $rebased; do
    if resolve_lock; then
      rebased=true
      [ -d "$(git rev-parse --git-path rebase-merge)" ] && rebased=false
    else
      break
    fi
  done
  if ! $rebased; then
    cat >&2 <<'MSG'

Conflict. It can only be in a line marked `fork:agent` (see FORK.md, "The patch to upstream files").
Keep upstream's version of the surrounding code, put the marked line back, then:
  git add <file> && git rebase --continue && agent-layer/rebase.sh --check
MSG
    exit 1
  fi
  after=$(count_markers)
  if [ "$after" -lt "$before" ]; then
    echo "Only $after of $before fork:agent markers left after the rebase: a hook line was lost." >&2
    exit 1
  fi
fi

# Every hook is still called (a marker can survive while its line moved into dead code: the build
# catches most of that, these catch the rest).
for f in crates/engine/src/ext.rs crates/ui-egui/src/ext.rs; do
  [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }
done
for hook in "ext::before" "ext::after" "ext::intercept" "ext::commands()" "crate::ext::find"; do
  git grep -q "$hook" -- crates/engine/src/lib.rs crates/engine/src/cmd/mod.rs || { echo "engine hook $hook is no longer called" >&2; exit 1; }
done
for hook in "ext::frame" "ext::side_panels" "ext::paint_slide" "ext::context_menu" "ext::presenter_notes"; do
  git grep -q "$hook" -- crates/ui-egui/src || { echo "UI hook $hook is no longer called" >&2; exit 1; }
done

echo "== fmt";      cargo fmt --all -- --check
echo "== clippy";   cargo clippy --workspace --all-targets -- -D warnings
echo "== tests";    cargo test -p deckcraft-agent -p deckcraft-agent-ui -p deckcraft-engine -p deckcraft-ui-egui -p deckcraft-mcp -p deckcraft
echo "== layers";   cargo xtask layers
echo "== assets";   cargo xtask assets
echo "== wasm";     cargo xtask wasm
echo "== build";    cargo build --release -p deckcraft
echo "Rebased and checked: $(count_markers) fork:agent markers, all gates green."
