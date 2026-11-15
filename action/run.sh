#!/usr/bin/env bash
# The GitHub Action (action.yml, docs/architecture.md step 8): runs `assertrust check` on the
# pull request or push that started the workflow, as annotations on the diff and a table in
# the job summary, and keeps that table in one PR comment once there are findings.
#
#   pull_request   what the PR adds since it split off from its base: BASE...HEAD
#   push           each commit pushed: BEFORE..AFTER, or for a new branch (or rewritten
#                  history) what it adds since it split off from the default branch
#
# With `judge: true` it adds --judge (and --judge-model, --judge-effort): the AI judge, with the key the
# workflow sets on the step. Its verdict cache is restored and saved around this script.
#
# Reads the action's inputs as INPUT_* variables and the event from $GITHUB_EVENT_PATH, and
# passes assertrust's `findings` output on. Exits with assertrust's code: 0 clean or
# warn-only, 1 a finding reached `fail-on`, 2 it couldn't run. Written for bash 3.2 too
# (macOS's /bin/bash).
set -euo pipefail

readonly releases=https://github.com/BeyondFolder/assertrust/releases/download
readonly marker='<!-- assertrust -->'

error() {
  echo "::error title=assertrust::$*"
  exit 2
}

notice() {
  echo "::notice title=assertrust::$*"
}

# A value from the event, or nothing.
event() {
  jq -r "$1 // empty" "$GITHUB_EVENT_PATH"
}

# Sets `binary`: the one given, else the release for this runner, downloaded and checked.
find_binary() {
  if [[ -n "${INPUT_PATH:-}" ]]; then
    binary=$INPUT_PATH
    return
  fi
  local version=${INPUT_VERSION:-}
  if [[ -z "$version" ]]; then
    # The version this copy of the action was released with.
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$GITHUB_ACTION_PATH/Cargo.toml" | head -n 1)
  fi
  version=${version#v}
  local target
  case "${RUNNER_OS:-}/${RUNNER_ARCH:-}" in
    Linux/X64) target=x86_64-unknown-linux-gnu ;;
    Linux/ARM64) target=aarch64-unknown-linux-gnu ;;
    macOS/X64) target=x86_64-apple-darwin ;;
    macOS/ARM64) target=aarch64-apple-darwin ;;
    *) error "there's no assertrust build for ${RUNNER_OS:-this runner} ${RUNNER_ARCH:-} yet, only Linux and macOS" ;;
  esac
  local dir="${RUNNER_TEMP:-/tmp}/assertrust-$version"
  local archive="assertrust-$target.tar.xz"
  binary="$dir/assertrust-$target/assertrust"
  if [[ -x "$binary" ]]; then
    return
  fi
  mkdir -p "$dir"
  local url="$releases/v$version/$archive"
  curl -fsSL --retry 3 -o "$dir/$archive" "$url" \
    || error "couldn't download assertrust $version for $target from $url: use a released tag of the action (@v0), or set version"
  curl -fsSL --retry 3 -o "$dir/$archive.sha256" "$url.sha256" \
    || error "couldn't download the checksum of assertrust $version from $url.sha256"
  local expected actual
  expected=$(awk 'NF { print $1; exit }' "$dir/$archive.sha256")
  actual=$(shasum -a 256 "$dir/$archive" | awk '{ print $1 }')
  [[ "$expected" == "$actual" ]] || error "assertrust $version for $target doesn't match its checksum"
  tar -xJf "$dir/$archive" -C "$dir"
}

# Sets `range`: the commits to check, from the event.
find_range() {
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request)
      range="$(event .pull_request.base.sha)...$(event .pull_request.head.sha)"
      ;;
    push)
      local before after base default
      before=$(event .before)
      after=$(event .after)
      if [[ -z "$after" || "$after" =~ ^0+$ ]]; then
        notice "the branch was deleted: nothing to check"
        exit 0
      fi
      if [[ -n "$before" && ! "$before" =~ ^0+$ ]] \
        && git merge-base --is-ancestor "$before" "$after" 2>/dev/null; then
        range="$before..$after"
        return
      fi
      # A new branch, or history rewritten: what it adds to the default branch.
      default=$(event .repository.default_branch)
      default=${default:-main}
      if git rev-parse --verify --quiet "refs/remotes/origin/$default" >/dev/null; then
        default="origin/$default"
      fi
      base=$(git merge-base "$default" "$after" 2>/dev/null) \
        || error "couldn't find where $after split off from $default"
      if [[ "$base" == "$after" ]]; then
        notice "no commits to check: $after is already on $default"
        exit 0
      fi
      range="$base..$after"
      ;;
    *)
      error "run it on pull_request or push events, or give the commits to check: args: --range BASE..HEAD"
      ;;
  esac
}

# Posts the summary as the PR's comment, or edits the one already there: the oldest a bot
# (the workflow's token) wrote, starting with the marker. A pull request without findings
# gets none, until a comment exists to say so.
comment() {
  local summary=$1 findings=$2 number repo comments existing body
  number=$(event .pull_request.number)
  repo=${GITHUB_REPOSITORY:-}
  [[ -n "$number" && -n "$repo" ]] || return 0
  if ! comments=$(gh api --paginate --slurp "repos/$repo/issues/$number/comments" 2>/dev/null); then
    # Not knowing whether there's one, posting could add a second.
    notice "couldn't read the PR's comments, so the comment wasn't updated; the findings are in the job summary"
    return 0
  fi
  existing=$(jq -r --arg marker "$marker" \
    '[.[][] | select(.user.type == "Bot" and (.body | startswith($marker)))][0].id // empty' \
    <<<"$comments")
  if [[ -z "$existing" && "${findings:-0}" == 0 ]]; then
    return 0
  fi
  body=$(cat "$summary")
  if [[ -n "${GITHUB_RUN_ID:-}" ]]; then
    body+=$'\n\n'"<sub>[assertrust](https://github.com/BeyondFolder/assertrust) · [run](${GITHUB_SERVER_URL:-https://github.com}/$repo/actions/runs/$GITHUB_RUN_ID)</sub>"
  fi
  if [[ -n "$existing" ]]; then
    gh api --method PATCH "repos/$repo/issues/comments/$existing" -f body="$body" >/dev/null 2>&1 \
      || notice "couldn't update the PR comment (a fork's pull request gets a read-only token); the findings are in the job summary"
  else
    gh api --method POST "repos/$repo/issues/$number/comments" -f body="$body" >/dev/null 2>&1 \
      || notice "couldn't post the PR comment (a fork's pull request gets a read-only token); the findings are in the job summary"
  fi
}

# Whether the arguments in `args` hold one of the flags given after it, as `--flag value`
# or `--flag=value`.
has_flag() {
  local args=" $1 " flag
  shift
  for flag in "$@"; do
    case "$args" in
      *" $flag "* | *" $flag="*) return 0 ;;
    esac
  done
  return 1
}

main() {
  local args extra=() mode=() fail_on=() judge=()
  # `args: |` may put them on several lines.
  args=$(printf '%s' "${INPUT_ARGS:-}" | tr '\n' ' ')
  read -r -a extra <<<"$args"
  if [[ "$(git rev-parse --is-shallow-repository 2>/dev/null)" == true ]]; then
    error "the checkout has only the last commits: use actions/checkout with fetch-depth: 0"
  fi
  if has_flag "$args" --format; then
    error "the action needs assertrust's github format: leave --format out of args"
  fi
  if ! has_flag "$args" --range --staged --worktree; then
    find_range
    mode=(--range "$range")
  fi
  # A --fail-on in args wins over the input.
  if [[ -n "${INPUT_FAIL_ON:-}" ]] && ! has_flag "$args" --fail-on; then
    fail_on=(--fail-on "$INPUT_FAIL_ON")
  fi
  case "${INPUT_JUDGE:-false}" in
    true) judge=(--judge) ;;
    false | "") ;;
    *) error "judge is \`$INPUT_JUDGE\`; it can be true or false" ;;
  esac
  if [[ -n "${INPUT_JUDGE_MODEL:-}" ]] && ! has_flag "$args" --judge-model; then
    judge+=(--judge-model "$INPUT_JUDGE_MODEL")
  fi
  if [[ -n "${INPUT_JUDGE_EFFORT:-}" ]] && ! has_flag "$args" --judge-effort; then
    judge+=(--judge-effort "$INPUT_JUDGE_EFFORT")
  fi
  find_binary

  # Its own summary and outputs files first, to post the summary as the comment too.
  local summary outputs findings code=0
  summary=$(mktemp)
  outputs=$(mktemp)
  GITHUB_STEP_SUMMARY=$summary GITHUB_OUTPUT=$outputs "$binary" check --format github \
    ${mode[@]+"${mode[@]}"} ${fail_on[@]+"${fail_on[@]}"} ${judge[@]+"${judge[@]}"} \
    ${extra[@]+"${extra[@]}"} || code=$?
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    cat "$summary" >>"$GITHUB_STEP_SUMMARY"
  fi
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    cat "$outputs" >>"$GITHUB_OUTPUT"
  fi
  findings=$(sed -n 's/^findings=//p' "$outputs")
  if [[ "$code" -le 1 && "${INPUT_COMMENT:-true}" == true && "${GITHUB_EVENT_NAME:-}" == pull_request ]]; then
    comment "$summary" "$findings"
  fi
  rm -f "$summary" "$outputs"
  exit "$code"
}

main
