#!/usr/bin/env bash
# Replays assertrust's backtests (docs/development.md, "Backtests").
#
# Each labels file names a repo (`repo`) and the commits to replay (`range`). For each one,
# this fetches the history the replay needs into target/backtest-clones/, fetches the own
# commits of the squash-merged PRs in the range from GitHub (refs/pull/N/head), and runs
# `assertrust backtest --labels` there.
#
#   backtest/run.sh                    every labels file here and in $ASSERTRUST_PRIVATE_LABELS
#   backtest/run.sh hono zod           only these
#   backtest/run.sh --update-labels    flags go to `assertrust backtest`
#
# Env: ASSERTRUST_PRIVATE_LABELS, a directory of labels files for private repos, kept out of
# this repo; ASSERTRUST_BACKTEST_CLONES, where the clones go; ASSERTRUST_BIN, the binary to
# run (default: a release build of this checkout).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
clones=${ASSERTRUST_BACKTEST_CLONES:-$root/target/backtest-clones}
# History kept before the range starts, so PRs opened earlier still find where they branched.
margin_days=365

bin=${ASSERTRUST_BIN:-}
if [ -z "$bin" ]; then
  cargo build --release --quiet --manifest-path "$root/Cargo.toml"
  bin=$root/target/release/assertrust
fi

flags=()
names=()
for arg in "$@"; do
  case $arg in
    -*) flags+=("$arg") ;;
    *) names+=("$arg") ;;
  esac
done

files=("$here"/*.toml)
if [ -n "${ASSERTRUST_PRIVATE_LABELS:-}" ]; then
  files+=("$ASSERTRUST_PRIVATE_LABELS"/*.toml)
fi

# A `key = "value"` line at the top of a labels file.
value() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" "$2" | head -n 1; }

status=0
for labels in "${files[@]}"; do
  [ -f "$labels" ] || continue
  name=$(basename "$labels" .toml)
  if [ ${#names[@]} -gt 0 ] && [[ " ${names[*]} " != *" $name "* ]]; then
    continue
  fi
  url=$(value repo "$labels")
  range=$(value range "$labels")
  base=${range%%..*}
  head=${range##*..}
  dir=$clones/$name
  echo "== $name"
  if [ ! -d "$dir/.git" ]; then
    git init --quiet "$dir"
    git -C "$dir" remote add origin "$url"
  fi
  git() { command git -C "$dir" "$@"; }
  if ! git cat-file -e "$base^{commit}" 2>/dev/null || ! git cat-file -e "$head^{commit}" 2>/dev/null; then
    # Both ends first, to learn when the range starts, then the history back to the margin.
    git fetch --quiet --no-tags --depth=1 origin "$base" "$head"
    since=$(( $(git log -1 --format=%ct "$base") - margin_days * 86400 ))
    git fetch --quiet --no-tags --shallow-since="@$since" origin "$head"
  fi
  # Squash-merged PRs (`Fix it (#12)`) whose own commits aren't here yet. A number GitHub
  # has no PR for is remembered, so it's asked about once.
  missing=$dir/.git/assertrust-missing-prs
  touch "$missing"
  refs=()
  while read -r pr; do
    git rev-parse --verify --quiet "refs/pull/$pr/head" >/dev/null && continue
    grep -qx "$pr" "$missing" || refs+=("refs/pull/$pr/head:refs/pull/$pr/head")
  done < <(git log --first-parent --no-merges --format=%s "$base..$head" | sed -n 's/.*(#\([0-9][0-9]*\))$/\1/p' | sort -un)
  since=$(( $(git log -1 --format=%ct "$base") - margin_days * 86400 ))
  for ((i = 0; i < ${#refs[@]}; i += 100)); do
    chunk=("${refs[@]:i:100}")
    # One fetch for many; if one of them doesn't exist, each on its own.
    git fetch --quiet --no-tags --shallow-since="@$since" origin "${chunk[@]}" 2>/dev/null && continue
    for ref in "${chunk[@]}"; do
      git fetch --quiet --no-tags --shallow-since="@$since" origin "$ref" 2>/dev/null \
        || { pr=${ref#refs/pull/}; echo "${pr%%/*}" >>"$missing"; }
    done
  done
  unset -f git
  (cd "$dir" && "$bin" backtest --labels "$labels" ${flags[@]+"${flags[@]}"}) || status=1
done
exit $status
