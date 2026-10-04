#!/usr/bin/env bash
# Stop and remove the producer-tag-on-merge service (systemd user unit or LaunchAgent).
#
#   scripts/uninstall.sh [--purge]
#
# By default the env file (tokens), tags, state, binary and Docker image are kept, so a
# reinstall picks up where it left off. --purge deletes them too (asks first when interactive).
# Nothing on GitHub/GitLab is touched; revoke the tokens there yourself if you want.
set -euo pipefail

readonly APP=producer-tag-on-merge
readonly LABEL=com.villegasmich.$APP

purge=false
case ${1:-} in
  "") ;;
  --purge) purge=true ;;
  -h | --help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) echo "error: unknown argument '$1' (try --help)" >&2; exit 1 ;;
esac

if [[ $(uname -s) == Darwin ]]; then
  launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
  rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
  data_dir="$HOME/Library/Application Support/$APP"
  extra=("$HOME/Library/Logs/$APP.log")
else
  systemctl --user disable --now "$APP" >/dev/null 2>&1 || true
  rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$APP.service"
  systemctl --user daemon-reload >/dev/null 2>&1 || true
  data_dir=${XDG_DATA_HOME:-$HOME/.local/share}/$APP
  extra=()
fi
if command -v docker >/dev/null 2>&1; then
  docker rm --force "$APP" >/dev/null 2>&1 || true
fi
echo "==> Service removed"

if [[ $purge == true ]]; then
  targets=("$HOME/.config/$APP" "$data_dir" "$HOME/.local/bin/$APP" ${extra[@]+"${extra[@]}"})
  if [[ -t 0 ]]; then
    printf 'This deletes your tokens, tags and state:\n'
    printf '  %s\n' "${targets[@]}"
    read -r -p "Continue? [y/N] " answer
    [[ $answer =~ ^[Yy]$ ]] || { echo "Kept everything else."; exit 0; }
  fi
  rm -rf "${targets[@]}"
  if command -v docker >/dev/null 2>&1; then
    docker image rm "$APP:latest" >/dev/null 2>&1 || true
  fi
  echo "==> Purged env file, tags, state, binary and Docker image"
fi
