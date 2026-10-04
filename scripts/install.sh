#!/usr/bin/env bash
# Install producer-tag-on-merge as a background service in your user session.
#
#   scripts/install.sh [docker|native] [--reconfigure] [--tag FILE]
#
#   docker         (Linux default) build the Docker image and run it from a systemd user unit.
#                  Needs docker (your user in the docker group, or rootless Docker), systemd and
#                  PulseAudio or PipeWire-pulse.
#   native         (macOS default) build the binary with cargo, install it to ~/.local/bin and
#                  run it from a systemd user unit (Linux) or a LaunchAgent (macOS).
#   --reconfigure  rewrite ~/.config/producer-tag-on-merge/env (tokens and settings).
#   --tag FILE     install FILE as your tag (default: keep the current one, or ask).
#
# Never uses root. Tokens come from GITHUB_TOKEN / GITLAB_TOKEN, else from your gh / glab login
# (after asking), else a hidden prompt; they only travel through variables and files, never argv.
# Settings exported when running this script (WATCH, GITHUB_REPOS, GITLAB_URL, QUIET_HOURS, ...)
# are written to the env file too. Re-running it updates the image/binary and restarts the service;
# the env file, tags and state are kept.
set -euo pipefail

readonly APP=producer-tag-on-merge
readonly IMAGE=$APP:latest
readonly LABEL=com.villegasmich.$APP
readonly CONFIG_DIR=$HOME/.config/$APP
readonly ENV_FILE=$CONFIG_DIR/env
readonly TAGS_DIR=$CONFIG_DIR/tags
readonly BIN_DIR=$HOME/.local/bin
readonly BIN=$BIN_DIR/$APP
readonly TAG_EXTENSIONS=(wav ogg flac mp3 aiff m4a)
readonly MAX_TAG_BYTES=$((5 * 1024 * 1024))
# Written to the env file when exported. Not TAGS_DIR/DATA_DIR: the Docker image sets its own.
readonly SETTINGS=(GITHUB_API_URL GITHUB_REPOS GITLAB_URL GITLAB_PROJECTS WATCH
  POLL_INTERVAL_SECONDS CATCH_UP_MINUTES MAX_PLAYS_PER_POLL PLAYER PLAYER_COMMAND VOLUME
  PLAY_TIMEOUT_SECONDS QUIET_HOURS TIMEZONE RUST_LOG)

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly ROOT

log() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed${2:+ ($2)}"; }
interactive() { [[ -t 0 ]]; }
# Yes/no question, default yes. Non-interactive: yes.
confirm() {
  interactive || return 0
  local answer
  read -r -p "$1 [Y/n] " answer
  [[ -z $answer || $answer =~ ^[Yy] ]]
}

# --- Arguments -------------------------------------------------------------------------------
os=$(uname -s)
case $os in
  Linux) mode=docker ;;
  Darwin) mode=native ;;
  *) die "unsupported OS '$os' (Linux and macOS only)" ;;
esac
reconfigure=false
tag_file=
while (($#)); do
  case $1 in
    docker | native) mode=$1 ;;
    --reconfigure) reconfigure=true ;;
    --tag)
      (($# >= 2)) || die "--tag needs a file"
      tag_file=$2
      shift
      ;;
    -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
  shift
done
[[ $EUID -ne 0 ]] || die "run this as your normal user, not root: the service needs your audio session"
if [[ $os == Darwin && $mode == docker ]]; then
  die "Docker on macOS can't reach CoreAudio and isn't automated; use native, or see docs/deployment.md#docker-on-macos-experimental"
fi

# --- Requirements ----------------------------------------------------------------------------
if [[ $os == Linux ]]; then
  need systemctl "systemd is required to run the service"
  systemctl --user show-environment >/dev/null 2>&1 \
    || die "no systemd user session (run this from your desktop session, not via sudo/su)"
  pulse_socket=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pulse/native
  [[ -S $pulse_socket ]] \
    || warn "no PulseAudio socket at $pulse_socket; on PipeWire install pipewire-pulse, or tags won't be heard"
fi
if [[ $mode == docker ]]; then
  need docker "https://docs.docker.com/engine/install/"
  docker info >/dev/null 2>&1 \
    || die "cannot talk to Docker; is it running, and is your user in the 'docker' group (or using rootless Docker)?"
else
  need cargo "install Rust from https://rustup.rs"
fi

# --- Env file (tokens + settings) ------------------------------------------------------------
check_token() {
  [[ ! $1 =~ [[:space:]] ]] || die "the $2 token contains whitespace; paste it again"
}

if [[ -f $ENV_FILE && $reconfigure == false ]]; then
  log "Keeping $ENV_FILE (use --reconfigure to rewrite it)"
else
  github_token=${GITHUB_TOKEN:-}
  if [[ -z $github_token ]] && command -v gh >/dev/null 2>&1 \
    && gh auth status --hostname github.com >/dev/null 2>&1 \
    && confirm "Link GitHub using the token of your gh CLI login?"; then
    github_token=$(gh auth token --hostname github.com)
  fi
  if [[ -z $github_token ]] && interactive; then
    read -r -s -p "GitHub token (read-only; Enter to skip GitHub): " github_token
    echo
  fi

  gitlab_url=${GITLAB_URL:-https://gitlab.com}
  gitlab_host=${gitlab_url#*://}
  gitlab_host=${gitlab_host%%/*}
  gitlab_token=${GITLAB_TOKEN:-}
  if [[ -z $gitlab_token ]] && command -v glab >/dev/null 2>&1; then
    glab_token=$(glab config get token --host "$gitlab_host" 2>/dev/null || true)
    if [[ -n $glab_token ]] && confirm "Link GitLab ($gitlab_host) using the token of your glab login?"; then
      gitlab_token=$glab_token
    fi
  fi
  if [[ -z $gitlab_token ]] && interactive; then
    read -r -s -p "GitLab token for $gitlab_host (read_api; Enter to skip GitLab): " gitlab_token
    echo
  fi

  [[ -n $github_token || -n $gitlab_token ]] \
    || die "no token: export GITHUB_TOKEN and/or GITLAB_TOKEN, or run this interactively (docs/configuration.md)"
  [[ -z $github_token ]] || check_token "$github_token" GitHub
  [[ -z $gitlab_token ]] || check_token "$gitlab_token" GitLab

  log "Writing $ENV_FILE (mode 600)"
  mkdir -p "$CONFIG_DIR"
  tmp=$(umask 077 && mktemp "$CONFIG_DIR/env.XXXXXX")
  {
    printf '# %s settings, written by scripts/install.sh. See docs/configuration.md.\n' "$APP"
    if [[ -n $github_token ]]; then printf 'GITHUB_TOKEN=%s\n' "$github_token"; fi
    if [[ -n $gitlab_token ]]; then printf 'GITLAB_TOKEN=%s\n' "$gitlab_token"; fi
    for name in "${SETTINGS[@]}"; do
      if [[ -n ${!name:-} ]]; then printf '%s=%s\n' "$name" "${!name}"; fi
    done
  } >"$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$ENV_FILE"
fi
chmod 600 "$ENV_FILE"

# --- Tags ------------------------------------------------------------------------------------
install_tag() {
  local file=$1 ext size
  [[ -f $file ]] || die "tag file '$file' not found"
  ext=$(printf '%s' "${file##*.}" | tr '[:upper:]' '[:lower:]')
  [[ " ${TAG_EXTENSIONS[*]} " == *" $ext "* ]] \
    || die "unsupported tag format '.$ext' (use ${TAG_EXTENSIONS[*]})"
  size=$(wc -c <"$file" | tr -d ' ')
  ((size > 0 && size <= MAX_TAG_BYTES)) || die "tag file must be between 1 byte and 5 MB"
  for other in "${TAG_EXTENSIONS[@]}"; do rm -f "$TAGS_DIR/default.$other"; done
  cp "$file" "$TAGS_DIR/default.$ext"
  log "Installed $file as your tag ($TAGS_DIR/default.$ext)"
}

mkdir -p "$TAGS_DIR"
has_default=false
for ext in "${TAG_EXTENSIONS[@]}"; do
  if [[ -f $TAGS_DIR/default.$ext ]]; then has_default=true; fi
done
if [[ -n $tag_file ]]; then
  install_tag "$tag_file"
elif [[ $has_default == false ]]; then
  answer=
  if interactive; then
    read -r -p "Path to your producer tag (${TAG_EXTENSIONS[*]}; Enter for the bundled sample): " answer
  fi
  install_tag "${answer:-$ROOT/assets/sample-tag.wav}"
fi

# --- Build -----------------------------------------------------------------------------------
if [[ $mode == docker ]]; then
  log "Building Docker image $IMAGE"
  docker build --tag "$IMAGE" "$ROOT"
  data_dir=$HOME/.local/share/$APP
  mkdir -p "$data_dir"
  run_app() {
    docker run --rm --user "$(id -u):$(id -g)" --env-file "$ENV_FILE" \
      --mount "type=bind,source=$pulse_socket,target=/run/pulse/native" \
      --mount "type=bind,source=$TAGS_DIR,target=/tags,readonly" \
      --mount "type=bind,source=$data_dir,target=/data" \
      "$IMAGE" "$@"
  }
else
  log "Building the release binary"
  (cd "$ROOT" && cargo build --release --locked)
  mkdir -p "$BIN_DIR"
  install -m 0755 "$ROOT/target/release/$APP" "$BIN"
  log "Installed $BIN"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) warn "$BIN_DIR is not on your PATH; add it to run '$APP' by hand" ;;
  esac
  run_app() { "$BIN" --env-file "$ENV_FILE" "$@"; }
fi

# --- Verify before starting ------------------------------------------------------------------
log "Checking configuration, tokens, tags and audio"
run_app check || die "check failed; fix the issues above and re-run (--reconfigure to change tokens)"
log "Playing your tag once"
run_app play || warn "could not play the tag; see 'docs/deployment.md' (audio) and run '$APP play' later"

# --- Service ---------------------------------------------------------------------------------
if [[ $os == Linux ]]; then
  unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
  unit=$unit_dir/$APP.service
  mkdir -p "$unit_dir"
  log "Installing $unit ($mode mode)"
  sed -e "s|@DOCKER@|$(command -v docker || echo /usr/bin/docker)|g" \
    -e "s|@IMAGE@|$IMAGE|g" -e "s|@BIN@|$BIN|g" \
    "$ROOT/deploy/systemd/$mode.service" >"$unit"
  systemctl --user daemon-reload
  systemctl --user enable "$APP" >/dev/null 2>&1
  systemctl --user restart "$APP"
  log "Done. $APP runs whenever you're logged in."
  echo "    Status:  systemctl --user status $APP"
  echo "    Logs:    journalctl --user -u $APP -f"
else
  plist=$HOME/Library/LaunchAgents/$LABEL.plist
  log_file=$HOME/Library/Logs/$APP.log
  mkdir -p "$(dirname "$plist")" "$(dirname "$log_file")"
  log "Installing $plist"
  sed -e "s|@BIN@|$BIN|g" -e "s|@ENV_FILE@|$ENV_FILE|g" -e "s|@LOG_FILE@|$log_file|g" \
    "$ROOT/deploy/launchd/agent.plist" >"$plist"
  domain=gui/$(id -u)
  launchctl bootout "$domain/$LABEL" >/dev/null 2>&1 || true
  launchctl bootstrap "$domain" "$plist"
  log "Done. $APP runs whenever you're logged in."
  echo "    Status:  launchctl print $domain/$LABEL"
  echo "    Logs:    tail -f $log_file"
fi
echo "    Remove:  scripts/uninstall.sh [--purge]"
