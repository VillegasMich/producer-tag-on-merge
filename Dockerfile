# syntax=docker/dockerfile:1

# Builder and runtime share the same Debian release so the binary's glibc matches.
FROM rust:1-slim-trixie AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
# Build dependencies in their own layer so source edits don't rebuild them.
RUN mkdir src \
 && echo 'fn main() {}' > src/main.rs \
 && touch src/lib.rs \
 && cargo build --release --locked \
 && rm -rf src
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:trixie-slim
# pulseaudio-utils: paplay (plays through the host's PulseAudio / PipeWire-pulse socket,
# libsndfile decodes wav/ogg/flac/mp3) and pactl (preflight check of the sound server).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates pulseaudio-utils tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/producer-tag-on-merge /usr/local/bin/producer-tag-on-merge
# Normally run with the host user's uid/gid (`--user`), which needs no passwd entry here. The
# default user is only for running without --user.
RUN useradd --create-home --uid 1000 app \
 && mkdir -p /data /tags \
 && chown app:app /data /tags
USER app
# HOME=/tmp: libpulse keeps per-user files under $HOME, which must be writable for any --user.
ENV DATA_DIR=/data \
    TAGS_DIR=/tags \
    PLAYER=paplay \
    PULSE_SERVER=unix:/run/pulse/native \
    HOME=/tmp
VOLUME /data
# tini is PID 1: forwards SIGTERM to the service and reaps the player processes.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/producer-tag-on-merge"]
CMD ["daemon"]
