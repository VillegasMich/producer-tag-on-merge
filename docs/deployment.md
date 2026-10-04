# Deployment

The service runs on the developer's own computer, in the background, started at login. It must
run **inside the user's session** because that is where the audio server lives: a systemd
**user** unit on Linux, a launchd **LaunchAgent** on macOS. A system-wide service (root,
`multi-user.target`, LaunchDaemon) has no speakers to talk to.

| Platform | Mode               | What runs                                        | Host requirements                                   |
| -------- | ------------------ | ------------------------------------------------ | --------------------------------------------------- |
| Linux    | `docker` (default) | `docker run … producer-tag-on-merge` from `systemctl --user` | systemd, Docker (user in `docker` group or rootless), PulseAudio or PipeWire-pulse |
| Linux    | `native`           | the binary from `systemctl --user`               | systemd, `paplay`/`pw-play`/`aplay`, (cargo)        |
| macOS    | `native` (default) | the binary as a LaunchAgent                      | (cargo); `afplay` ships with macOS                  |
| macOS    | `docker` (experimental) | container + a PulseAudio server on the host  | Docker Desktop, `brew install pulseaudio`           |

Why native is the macOS default: Docker Desktop runs containers in a Linux VM that has no access to
CoreAudio, so a container can't reach the Mac's speakers directly.

## Files on the host

Same layout on both platforms, so docs and scripts don't fork:

| Path                                       | Content                                      |
| ------------------------------------------ | -------------------------------------------- |
| `~/.config/producer-tag-on-merge/env`      | Settings and tokens, mode `600`. Same format as `.env.example`. |
| `~/.config/producer-tag-on-merge/tags/`    | Tag files (`TAGS_DIR`). See [tags.md](tags.md). |
| `~/.local/share/producer-tag-on-merge/` (Linux), `~/Library/Application Support/producer-tag-on-merge/` (macOS) | `state.json` (`DATA_DIR`) |

## Tokens

- **GitHub:** classic PAT with `repo`, or fine-grained with *Pull requests: Read* + *Metadata:
  Read*. Shortcut with the GitHub CLI: `gh auth token`.
- **GitLab:** PAT with `read_api` (User settings → Access tokens). Shortcut with the GitLab CLI:
  `glab auth status -t` shows the token.

Store them only in the env file (mode `600`). Don't bake them into the image and don't pass them
with `-e TOKEN=…` on the command line (shell history, `ps`).

## Install script (planned)

```bash
scripts/install.sh            # docker on Linux, native on macOS
scripts/install.sh native     # force a mode
scripts/install.sh docker
scripts/install.sh --reconfigure   # rewrite the env file
scripts/uninstall.sh          # stop + remove unit/agent; keeps env, tags and state unless --purge
```

Steps:

1. Detect the OS and check the host requirements for the mode.
2. Ask which accounts to link. Tokens are read from `GITHUB_TOKEN` / `GITLAB_TOKEN`, else offered
   from `gh auth token` / `glab`, else prompted (hidden input).
3. Write `~/.config/producer-tag-on-merge/env` (mode `600`). Kept on re-runs unless
   `--reconfigure`.
4. Create the tags dir. If it has no `default.*`, ask for a file (or install the bundled sample
   tag so it works out of the box).
5. Build the image (`docker build`) or install the binary to `~/.local/bin` (`cargo build
   --release`).
6. Install and start the unit (Linux) or agent (macOS), then run `check` and `play` so you hear
   the tag once.

## Linux

### Docker image (planned)

Multi-stage build, same shape as the sibling projects:

1. **Builder** – `rust:1-slim-trixie`, `cargo build --release --locked`, dependencies cached in
   their own layer.
2. **Runtime** – `debian:trixie-slim` with `ca-certificates`, `tini` and `pulseaudio-utils`
   (`paplay`, `pactl`; libsndfile decodes wav/ogg/flac/mp3). Binary at
   `/usr/local/bin/producer-tag-on-merge`, `ENTRYPOINT ["tini", "--", "producer-tag-on-merge"]`,
   `CMD ["daemon"]`, `ENV DATA_DIR=/data TAGS_DIR=/tags PLAYER=paplay
   PULSE_SERVER=unix:/run/pulse/native`.

The container runs with the **host user's uid/gid** (`--user`), otherwise the PulseAudio socket
refuses it and the bind-mounted dirs are not writable.

### Running the container by hand

```bash
docker build -t producer-tag-on-merge .

docker run -d --name producer-tag-on-merge --restart unless-stopped \
  --user "$(id -u):$(id -g)" \
  --env-file ~/.config/producer-tag-on-merge/env \
  -v "$XDG_RUNTIME_DIR/pulse/native:/run/pulse/native" \
  -v ~/.config/producer-tag-on-merge/tags:/tags:ro \
  -v ~/.local/share/producer-tag-on-merge:/data \
  producer-tag-on-merge

docker exec producer-tag-on-merge producer-tag-on-merge play     # hear it
docker logs -f producer-tag-on-merge
```

Classic PulseAudio (not PipeWire) may also need its auth cookie:
`-v ~/.config/pulse/cookie:/run/pulse/cookie:ro -e PULSE_COOKIE=/run/pulse/cookie`.

Check that the socket exists first: `ls $XDG_RUNTIME_DIR/pulse/native` (on PipeWire systems this
needs `pipewire-pulse`, installed by default on current Ubuntu and Fedora).

### systemd user unit (docker mode)

`~/.config/systemd/user/producer-tag-on-merge.service`, installed by `scripts/install.sh`
(`deploy/systemd/docker.service`, planned):

```ini
[Unit]
Description=producer-tag-on-merge (Docker)
Documentation=https://github.com/VillegasMich/producer-tag-on-merge
After=pipewire-pulse.service pulseaudio.service

[Service]
Type=simple
Environment=CONTAINER=producer-tag-on-merge
Environment=IMAGE=producer-tag-on-merge:latest
# Remove a container left over from an unclean stop.
ExecStartPre=-/usr/bin/docker rm --force ${CONTAINER}
ExecStart=/usr/bin/docker run --rm --name ${CONTAINER} --user %U:%G \
  --env-file %h/.config/producer-tag-on-merge/env \
  --volume %t/pulse/native:/run/pulse/native \
  --volume %h/.config/producer-tag-on-merge/tags:/tags:ro \
  --volume %h/.local/share/producer-tag-on-merge:/data \
  ${IMAGE}
ExecStop=/usr/bin/docker stop --time 20 ${CONTAINER}
Restart=always
RestartSec=30
TimeoutStopSec=30

[Install]
WantedBy=default.target
```

(`%h` = home, `%t` = `$XDG_RUNTIME_DIR`, `%U`/`%G` = uid/gid. The installer replaces
`/usr/bin/docker` with `command -v docker`.) A user unit can't order itself after the system
`docker.service`; if Docker isn't up yet the run fails and `Restart=always` retries.

### systemd user unit (native mode)

`deploy/systemd/native.service` (planned):

```ini
[Unit]
Description=producer-tag-on-merge (native)
After=pipewire-pulse.service pulseaudio.service

[Service]
Type=simple
ExecStart=%h/.local/bin/producer-tag-on-merge --env-file %h/.config/producer-tag-on-merge/env daemon
Restart=always
RestartSec=30
TimeoutStopSec=30

[Install]
WantedBy=default.target
```

### Managing it

```bash
systemctl --user status producer-tag-on-merge
systemctl --user restart producer-tag-on-merge
journalctl --user -u producer-tag-on-merge -f
```

User units start at login and stop at logout, which is what we want: no one is there to hear the
tag otherwise. (`loginctl enable-linger` is not needed and not recommended.)

## macOS

### Native LaunchAgent (default)

`~/Library/LaunchAgents/com.villegasmich.producer-tag-on-merge.plist`
(`deploy/launchd/agent.plist`, planned; the installer fills in the paths):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.villegasmich.producer-tag-on-merge</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOU/.local/bin/producer-tag-on-merge</string>
    <string>--env-file</string>
    <string>/Users/YOU/.config/producer-tag-on-merge/env</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>/Users/YOU/Library/Logs/producer-tag-on-merge.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/YOU/Library/Logs/producer-tag-on-merge.log</string>
</dict>
</plist>
```

launchd has no `EnvironmentFile`, which is why the binary takes `--env-file`. Tokens stay out of
the plist.

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.villegasmich.producer-tag-on-merge.plist
launchctl kickstart -k gui/$(id -u)/com.villegasmich.producer-tag-on-merge   # restart
launchctl print gui/$(id -u)/com.villegasmich.producer-tag-on-merge          # status
launchctl bootout gui/$(id -u)/com.villegasmich.producer-tag-on-merge        # stop + unload
tail -f ~/Library/Logs/producer-tag-on-merge.log
```

macOS may show a "background item added" notification the first time; it can be managed in
System Settings → General → Login Items.

### Docker on macOS (experimental)

Only if you really want the container. The container plays to a PulseAudio server running on the
Mac, over TCP:

```bash
brew install pulseaudio
pulseaudio --daemonize=yes --exit-idle-time=-1 \
  --load="module-native-protocol-tcp listen=127.0.0.1 auth-ip-acl=127.0.0.1 auth-anonymous=1"

docker run -d --name producer-tag-on-merge --restart unless-stopped \
  --env-file ~/.config/producer-tag-on-merge/env \
  -e PULSE_SERVER=tcp:host.docker.internal:4713 \
  -v ~/.config/producer-tag-on-merge/tags:/tags:ro \
  -v producer-tag-on-merge-data:/data \
  producer-tag-on-merge
```

Caveats:

- PulseAudio itself must also be kept running (its own LaunchAgent), so this is two services
  instead of one.
- Never listen on a LAN address with `auth-anonymous=1`: anyone on the network could play sound
  on your machine.
- Verify during implementation that Docker Desktop's `host.docker.internal` traffic arrives from
  `127.0.0.1`; adjust `auth-ip-acl` if not.

## Running without a service

```bash
cargo build --release
./target/release/producer-tag-on-merge --env-file .env check
./target/release/producer-tag-on-merge --env-file .env play
./target/release/producer-tag-on-merge --env-file .env        # daemon in the foreground
```

## Updating

```bash
git pull
scripts/install.sh            # rebuilds image/binary and restarts; env, tags and state are kept
```
