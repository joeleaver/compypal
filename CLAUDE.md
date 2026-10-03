# compypal

A MIDI composer/arranger built to work alongside an agent: a "music IDE".
Not a DAW. Record from a controller into a **session**; an agent cleans up
timing and mistakes while keeping the intent, then helps arrange. Preview
with a built-in synth; export MIDI and ABC.

## Layout

| Crate | What it holds |
|---|---|
| `compypal-core` | Project model (ticks, `PPQ = 960`), raw `Session`s (seconds), cleanup ops, snapshot undo, GM names, `theory` (chords, Roman numerals, keys), `figure` (musical units), demo project. No I/O, no UI. |
| `compypal-io` | SMF import/export (`midly`), ABC export. |
| `compypal-audio` | `Engine` (cpal output on its own thread), `Synth` trait with SoundFont (`rustysynth`) and built-in fallback, pure `schedule::build` from a project, realtime `Player`. |
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

## Figures

A figure is the unit you'd talk about: "a C arpeggio, then a run into G".
It has a chord and a shape (block, arpeggio, run, melody, note, mixed).
`figure::analyze` derives figures from notes; they are never stored.

- Onsets within `chord_window` are one event (human spread).
- A DP splits events into figures: chord misfit (duration-weighted) plus a
  per-figure cost that is cheapest on barlines. Passing tones (stepwise in
  and out) count a quarter. Rests past `split_gap` always split.
- `revoice` maps each note to its role in the new chord, near its old
  pitch, keeping the contour. A new chord's tones that no note plays take a
  doubled note (top root, then fifth), so typing G7 over a G triad works.
- `set_chord`, `continue_with` ("and then X": copy the shape onto a new
  chord) and `describe` (one line per figure) are the API the UI uses and
  the MCP tools should use too.
- The lane above the piano roll is the UI: click a figure, type a chord
  or numeral, Tab moves on, and "+" appends.

## Audio

- The engine starts on `BasicSynth` and swaps to a GM SoundFont once it has
  loaded in the background (`COMPYPAL_SOUNDFONT`, else the usual system
  paths; FluidR3_GM on this machine).
- The audio thread only receives `Cmd`s over a channel and publishes the
  position through atomics. The UI polls it from a plain thread and
  `send()`s into signals. Never lock or allocate in `Player::process` beyond
  what is there.
- Any project change while playing rebuilds the schedule and sends
  `Cmd::Update`, which keeps the position (an Effect in `app`).
- `compypal-audio` and `rustysynth` build at opt-level 3 even in dev;
  unoptimized, the synth can't keep up with the callback.
- `cargo run -p compypal-audio --example play_demo` checks the device
  without the UI.

## Building

- rinch comes from GitHub (`joeleaver/rinch`, branch `main`); `Cargo.lock`
  pins the commit. Pull a newer rinch with `cargo update -p rinch`. Our root
  `Cargo.toml` copies rinch's `[patch.crates-io]` section (winit/wgpu forks),
  because patches don't propagate to dependent workspaces. If rinch changes
  its patches, copy them again. If the build fails inside rinch's winit code,
  the lockfile has drifted from rinch's: re-seed it from rinch's `Cargo.lock`.
- To hack on rinch and compypal together locally, temporarily add
  `[patch."https://github.com/joeleaver/rinch"] rinch = { path = "../dev/rinch/crates/rinch" }`
  and don't commit it.
- The app enables rinch's `debug` feature, so a running app can be driven by
  the rinch MCP server (`rinch-mcp-server`, built from the rinch repo).

## rinch gotchas hit so far

- Keyed `for` items whose key matches are **not rebuilt**. Piano-roll keys
  include the geometry, so a moved or zoomed note gets a new key.
- `Memo` has no `.with()`; use `.get()`.
- Global CSS lives in `src/style.css`, injected via `style { {CSS} }`.
- `autofocus` only works inside Modal/Popover. Elsewhere, build the input as
  its own node and call `.focus()` on it.
- Keyboard handling for a popup: `overlay_dismiss::arm_keys_while_open`
  (it gets keys only while focus is inside the owner).

## Roadmap

1. ~~Audio~~ (done). Follow-ups: playhead auto-scroll, a loop range.
2. **Recording**: `midir` input into a new `Session`, with click, count-in, and
   live display. The session stores `downbeat_offset` and `click_bpm`.
3. **Agent / music IDE**: an MCP server embedded in the app on localhost,
   advertised to Claude Code the way editor extensions are (a lockfile in
   `~/.claude/ide/` that `/ide` discovers). Check the current lockfile and
   auth format against Claude Code before implementing. Tools: read the
   project/session (JSON and ABC), `timing_report`, import + cleanup ops,
   figures (`describe`, `set_chord`, `continue_with`, `theory::suggest`),
   write clip notes, sections/arrangement, transport, and the user's current
   piano-roll selection as context. All edits go through `Store::edit`
   (from the MCP thread via `Signal::send`/`update_send`).
4. **Arranger view**: section/clip timeline across tracks.
5. **Own synth**: a wavetable/mod-matrix synth in the spirit of Vital, to sit
   alongside the SoundFont player.
