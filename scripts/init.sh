#!/usr/bin/env bash
# Prepare this checkout for development: write ./.env from .env.example, filled in.
#
#   scripts/init.sh [--force] [--from DIR]
#
#   --force     ignore the values already in ./.env and start again from .env.example.
#   --from DIR  checkout to inherit settings from (default: $ORCA_ROOT_PATH, else the main git
#               worktree). Paths inside it are rewritten to point at this checkout.
#
# For each setting the first non-empty value wins:
#   1. ./.env (kept on re-runs: only empty settings are filled in, new ones are added)
#   2. the .env of the --from checkout
#   3. detected: GITHUB_TOKEN / GITLAB_TOKEN from the environment, else from your gh / glab
#      login; TIMEZONE from the system; DATA_DIR and TAGS_DIR under ./.dev (below)
#   4. the default in .env.example
# Settings in ./.env that .env.example doesn't know are kept at the end.
#
# DATA_DIR is ./.dev/data, so runs from this checkout never touch the installed service's state.
# TAGS_DIR is only set when ~/.config/producer-tag-on-merge/tags has no default tag: ./.dev/tags is
# then seeded from the --from checkout's .dev/tags, else with assets/sample-tag.wav.
# The result is for native runs (cargo run -- --env-file .env ...), not docker --env-file.
#
# Never prompts, never prints a token; tokens only travel through variables and files, never argv.
# Orca runs this for every new worktree (orca.yaml, scripts.setup).
set -euo pipefail

readonly APP=producer-tag-on-merge

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly ROOT
readonly TEMPLATE=$ROOT/.env.example
readonly ENV_FILE=$ROOT/.env
readonly DEV_DIR=$ROOT/.dev
readonly USER_TAGS_DIR=$HOME/.config/$APP/tags

log() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }
has_default_tag() { compgen -G "$1/default.*" >/dev/null; }

# --- Arguments -------------------------------------------------------------------------------
force=false
from=${ORCA_ROOT_PATH:-}
while (($#)); do
  case $1 in
    --force) force=true ;;
    --from)
      (($# >= 2)) || die "--from needs a directory"
      from=$2
      shift
      ;;
    -h | --help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
  shift
done

[[ -f $TEMPLATE ]] || die "$TEMPLATE not found"

# --- Checkout to inherit from ----------------------------------------------------------------
if [[ -z $from ]]; then
  from=$(git -C "$ROOT" worktree list --porcelain 2>/dev/null | sed -n '1s/^worktree //p') || true
fi
if [[ -n $from ]]; then
  from=$(cd "$from" 2>/dev/null && pwd -P) || { warn "'$from' not found; not inheriting"; from=; }
fi
[[ $from != "$ROOT" ]] || from=
from_env=
[[ -z $from || ! -f $from/.env ]] || from_env=$from/.env

# --- Value lookup ----------------------------------------------------------------------------
# Lookups set `found` (the value) and `origin` (where it came from, never the value itself).
found=
origin=

# Last value of KEY in an env file; empty when unset or the file is missing.
env_get() {
  [[ -n $2 && -f $2 ]] || return 0
  sed -n "s/^$1=//p" "$2" | tail -n 1
}

# KEY from ./.env (unless --force), else from the --from checkout's .env.
lookup() {
  found='' origin=''
  if ! $force; then
    found=$(env_get "$1" "$ENV_FILE")
    origin=kept
  fi
  if [[ -z $found && -n $from_env ]]; then
    found=$(env_get "$1" "$from_env")
    origin="inherited from $from_env"
    # Paths into that checkout point at this one instead.
    if [[ $found == "$from"/* ]]; then found=$ROOT/${found#"$from"/}; fi
  fi
  if [[ -z $found ]]; then origin=; fi
  return 0
}

# Host part of a URL: https://ghe.example.com/api/v3 -> ghe.example.com
host_of() {
  local rest=${1#*://}
  printf '%s' "${rest%%/*}"
}

# IANA name of the system time zone, or nothing.
system_timezone() {
  local tz=
  if [[ -n ${TZ:-} && ${TZ:-} != :* ]]; then
    tz=$TZ
  elif has timedatectl; then
    tz=$(timedatectl show --property=Timezone --value 2>/dev/null || true)
  fi
  if [[ -z $tz && -L /etc/localtime ]]; then
    tz=$(readlink /etc/localtime)
    tz=${tz##*/zoneinfo/}
  fi
  if [[ -z $tz && -f /etc/timezone ]]; then tz=$(head -n 1 /etc/timezone); fi
  if [[ $tz =~ ^[A-Za-z_]+(/[A-Za-z0-9_+-]+)*$ ]]; then printf '%s' "$tz"; fi
  return 0
}

detect() {
  found='' origin=''
  local host
  case $1 in
    GITHUB_TOKEN)
      if [[ -n ${GITHUB_TOKEN:-} ]]; then
        found=$GITHUB_TOKEN origin="GITHUB_TOKEN from the environment"
      elif has gh; then
        lookup GITHUB_API_URL
        host=$(host_of "${found:-https://api.github.com}")
        [[ $host != api.github.com ]] || host=github.com
        found=$(gh auth token --hostname "$host" 2>/dev/null || true)
        origin="gh login ($host)"
      fi
      ;;
    GITLAB_TOKEN)
      if [[ -n ${GITLAB_TOKEN:-} ]]; then
        found=$GITLAB_TOKEN origin="GITLAB_TOKEN from the environment"
      elif has glab; then
        lookup GITLAB_URL
        host=$(host_of "${found:-https://gitlab.com}")
        found=$(glab config get token --host "$host" 2>/dev/null || true)
        origin="glab login ($host)"
      fi
      ;;
    TIMEZONE)
      found=$(system_timezone)
      origin="system time zone"
      ;;
    DATA_DIR)
      found=$DEV_DIR/data origin="per-checkout state"
      ;;
    TAGS_DIR)
      if ! has_default_tag "$USER_TAGS_DIR"; then
        found=$DEV_DIR/tags origin="no tag in $USER_TAGS_DIR yet"
      fi
      ;;
  esac
  if [[ $1 == *_TOKEN && $found =~ [[:space:]] ]]; then
    warn "$1 from $origin contains whitespace; ignoring it"
    found=
  fi
  if [[ -z $found ]]; then origin=; fi
  return 0
}

resolve() {
  lookup "$1"
  [[ -n $found ]] || detect "$1"
  return 0
}

# --- Write .env ------------------------------------------------------------------------------
umask 077
tmp=$(mktemp "$ROOT/.env.XXXXXX")
trap 'rm -f "$tmp"' EXIT

written=' '
filled=()
while IFS= read -r line || [[ -n $line ]]; do
  # `KEY=default` and commented `# KEY=example` lines; a commented one is enabled when it has a value.
  if [[ $line =~ ^(#\ )?([A-Z][A-Z0-9_]*)=(.*)$ && $written != *" ${BASH_REMATCH[2]} "* ]]; then
    commented=${BASH_REMATCH[1]}
    key=${BASH_REMATCH[2]}
    resolve "$key"
    if [[ -n $found ]]; then
      printf '%s=%s\n' "$key" "$found"
      written+="$key "
      [[ $origin == kept ]] || filled+=("$key: $origin")
      continue
    fi
    [[ -n $commented ]] || written+="$key "
  fi
  printf '%s\n' "$line"
done <"$TEMPLATE" >"$tmp"

# Keep local settings the template doesn't know.
if ! $force && [[ -f $ENV_FILE ]]; then
  extras=()
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line =~ ^([A-Z][A-Z0-9_]*)=.+$ && $written != *" ${BASH_REMATCH[1]} "* ]]; then
      extras+=("$line")
      written+="${BASH_REMATCH[1]} "
    fi
  done <"$ENV_FILE"
  if ((${#extras[@]})); then
    printf '\n# --- Local -----------------------------------------------------------------------------------\n\n'
    printf '%s\n' "${extras[@]}"
  fi >>"$tmp"
fi

chmod 600 "$tmp"
mv "$tmp" "$ENV_FILE"
log "Wrote $ENV_FILE"
for entry in ${filled[@]+"${filled[@]}"}; do printf '    %s\n' "$entry"; done

# --- Dev directories -------------------------------------------------------------------------
data_dir=$(env_get DATA_DIR "$ENV_FILE")
if [[ $data_dir == "$DEV_DIR"/* ]]; then mkdir -p "$data_dir"; fi

tags_dir=$(env_get TAGS_DIR "$ENV_FILE")
if [[ $tags_dir == "$DEV_DIR"/* ]] && ! has_default_tag "$tags_dir"; then
  mkdir -p "$tags_dir"
  if [[ -n $from ]] && has_default_tag "$from/.dev/tags"; then
    cp -R "$from/.dev/tags/." "$tags_dir/"
    log "Copied tags from $from/.dev/tags"
  else
    cp "$ROOT/assets/sample-tag.wav" "$tags_dir/default.wav"
    log "Using assets/sample-tag.wav as the tag (replace $tags_dir/default.wav with yours)"
  fi
fi

# --- Next steps ------------------------------------------------------------------------------
if [[ -z $(env_get GITHUB_TOKEN "$ENV_FILE") && -z $(env_get GITLAB_TOKEN "$ENV_FILE") ]]; then
  warn "no GitHub/GitLab token: run 'gh auth login' (or 'glab auth login') and re-run $0;"
  warn "'cargo dev simulate' works without one"
else
  log "Next: cargo dev check  (alias for cargo run -- --env-file .env)"
fi
