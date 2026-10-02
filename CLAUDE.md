# compypal

A MIDI composer/arranger built to work alongside an agent: a "music IDE".
Not a DAW. Record from a controller into a **session**; an agent cleans up
timing and mistakes while keeping the intent, then helps arrange. Preview
with a built-in synth; export MIDI and ABC.

## Layout

| Crate | What it holds |
|---|---|
| `compypal-core` | Project model (ticks, `PPQ = 960`), raw `Session`s (seconds), cleanup ops, snapshot undo, GM names, demo project. No I/O, no UI. |
| `compypal-io` | SMF import/export (`midly`), ABC export. |
| `compypal` | The rinch desktop app. `store.rs` is all app state; every project edit goes through `Store::edit` so UI and agent share undo. |

## Model rules

- **Sessions are immutable.** They are the record of what was actually played.
  Clips are derived from them (`cleanup::import_session` → cleanup ops) and
  keep `source_session` so the original is always recoverable and can be
  overlaid (the orange outlines in the piano roll).
- Cleanup ops are small, deterministic, and return change counts, so an
  agent can compose them, inspect `timing_report`, and retry. Add new ones
  in the same style rather than one big "fix it" function.
- Clip note times are relative to the clip start.

## Building

- rinch is a path dependency on `../dev/rinch`. Our root `Cargo.toml` copies
  rinch's `[patch.crates-io]` section (winit/wgpu forks), because patches
  don't propagate to dependent workspaces. If rinch changes its patches,
  copy them again. If the build fails inside rinch's winit code, re-seed
  `Cargo.lock` from `../dev/rinch/Cargo.lock`.
- The app enables rinch's `debug` feature, so a running app can be driven by
  the rinch MCP server (`/home/joe/dev/rinch/target/release/rinch-mcp-server`).

## rinch gotchas hit so far

- Keyed `for` items whose key matches are **not rebuilt**. Piano-roll keys
  include the geometry, so a moved or zoomed note gets a new key.
- `Memo` has no `.with()`; use `.get()`.
- Global CSS lives in `src/style.css`, injected via `style { {CSS} }`.

## Roadmap

1. **Audio**: `compypal-audio` crate with a cpal output stream, `rustysynth`
   SoundFont playback (GM), a transport/sequencer, and a metronome.
2. **Recording**: `midir` input into a new `Session`, with click, count-in, and
   live display. The session stores `downbeat_offset` and `click_bpm`.
3. **Agent / music IDE**: an MCP server embedded in the app on localhost,
   advertised to Claude Code the way editor extensions are (a lockfile in
   `~/.claude/ide/` that `/ide` discovers). Check the current lockfile and
   auth format against Claude Code before implementing. Tools: read the
   project/session (JSON and ABC), `timing_report`, import + cleanup ops,
   write clip notes, sections/arrangement, transport, and the user's current
   piano-roll selection as context. All edits go through `Store::edit`
   (from the MCP thread via `Signal::send`/`update_send`).
4. **Arranger view**: section/clip timeline across tracks.
5. **Own synth**: a wavetable/mod-matrix synth in the spirit of Vital, to sit
   alongside the SoundFont player.
