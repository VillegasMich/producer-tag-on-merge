# Producer tags

A producer tag is the short sound a music producer stamps on their tracks so you know who made the
beat. Here it is the sound your computer plays when your PR/MR gets merged.

## What makes a good tag

- **Short:** 1–4 seconds. It interrupts whatever you're doing; anything above ~10 s gets annoying
  (and is cut at `PLAY_TIMEOUT_SECONDS`).
- **Recognizable:** a vocal drop ("…on the beat"), a sample, a chord, a synth stab. In team mode
  everyone should be able to tell whose PR just landed.
- **No silence at the start:** the merge already happened; the sound should hit immediately.
- **Not too loud:** normalize it (recipes below) so it sits at a sane level next to your music
  and calls.
- **Format:** `wav` or `ogg` work with every player. `flac`, `mp3`, `aiff`, `m4a` work with
  `afplay` (macOS) and with `paplay` in the Docker image; `aplay` only plays `wav`.
- Max file size accepted by `tag set`: 5 MB.

## Making one

Any DAW or audio editor works (Audacity, GarageBand, Ableton, FL Studio…). Export a short WAV.

### Text-to-speech vocal tag

macOS:

```bash
say -v Samantha -o tag.aiff "Villegas on the merge"
```

Linux:

```bash
espeak-ng -w tag.wav "Villegas on the merge"
```

### Clean up with ffmpeg

Trim leading silence, cut to 4 s, normalize loudness, stereo 48 kHz WAV:

```bash
ffmpeg -i input.wav \
  -af "silenceremove=start_periods=1:start_threshold=-50dB,loudnorm=I=-16:TP=-1.5:LRA=11" \
  -t 4 -ar 48000 -ac 2 tag.wav
```

Add a little echo, producer-style:

```bash
ffmpeg -i tag.wav -af "aecho=0.8:0.6:120|240:0.4|0.25" tag-echo.wav
```

Cut a slice out of a longer track (from 1:02.5, 3 s), with a short fade out:

```bash
ffmpeg -ss 62.5 -t 3 -i song.mp3 -af "afade=t=out:st=2.6:d=0.4" -ar 48000 -ac 2 tag.wav
```

Only use audio you have the rights to, especially if you share tags with a team.

## Installing your tag

```bash
producer-tag-on-merge tag set ./tag.wav         # becomes TAGS_DIR/default.wav and plays once
producer-tag-on-merge play                      # play it again any time
```

With the Docker setup, `TAGS_DIR` is a host folder mounted into the container
(`~/.config/producer-tag-on-merge/tags` by default), so you can also just copy the file there as
`default.wav`. Changes are picked up on the next merge; no restart needed.

## Where tags live

```text
TAGS_DIR/
├── default.wav             # your tag; also the fallback for authors without one
├── github/
│   ├── octocat.ogg         # tag for GitHub user "octocat"
│   └── villegasmich.wav    # your own GitHub-specific tag (optional)
├── gitlab/
│   └── jdoe.mp3            # tag for GitLab user "jdoe"
└── alice.wav               # "alice" on any platform
```

Lookup for a merge by `<author>` on `<platform>` (first match wins):

1. `TAGS_DIR/<platform>/<author>.<ext>`
2. `TAGS_DIR/<author>.<ext>`
3. `TAGS_DIR/default.<ext>`

Names are lower-cased usernames (GitHub login, GitLab username), not display names.

## Team tags

With `WATCH=repos` every merge in the watched repos plays, and each author gets their own tag:

1. Each developer makes a tag and names it after their username.
2. Share them, for example in a small `team-tags` git repo laid out like the tree above.
3. Everyone clones it and points `TAGS_DIR` at the clone (Docker: mount the clone at `/tags`).
4. `git pull` now and then to get new teammates' tags.

```bash
producer-tag-on-merge tag set ./alice.wav --for github:alice
producer-tag-on-merge tag list
producer-tag-on-merge play --author github:alice
```
