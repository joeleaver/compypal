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
| `compypal-mcp` | The agent interface: MCP tools over an `App` trait (`tools.rs`, testable on `MemoryApp`), JSON-RPC handling, an HTTP server for tools, the Claude Code IDE WebSocket link, and `agent` (running coding agents: ACP and Claude Code backends, the panel's conversation model). |
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
- Laying music over music (recording, adding a sketch) never mixes it in:
  `Track::add_over` cuts the clips underneath at its edges and mutes the
  covered parts, kept as alternatives, each remembering what muted it
  (`muted_by`); `Track::prefer` (`use_sketch`, "Use this one", a click in
  the roll's Takes lanes) switches, per bar range. `Project::take_out`
  removes a sketch's clips and brings back exactly what they muted.
  `Track::absolute_notes` is what's heard; `muted_notes` is what's under
  it (dashed in the roll, hatched in the arranger, listed as lanes above
  the roll). A split clip keeps `source_offset`, its place in its take, so
  `Clip::refill` (Tidy, `clean_take`) refills only its window.

## Figures

A figure is the unit you'd talk about: "a C arpeggio, then a run into G".
It has a chord and a shape (block, arpeggio, run, melody, note, mixed).
`figure::analyze` derives figures from notes; they are never stored.

- Onsets within `chord_window` are one event (human spread).
- A DP splits events into figures: chord misfit (duration-weighted) plus a
  per-figure cost that is cheapest on barlines, plus a small cost per tick
  for each tone past a triad (C, Am, F in a row also spell Fmaj9). Passing tones (stepwise in
  and out) count a quarter. Rests past `split_gap` always split.
- `revoice` maps each note to its role in the new chord, near its old
  pitch, keeping the contour. A new chord's tones that no note plays take a
  doubled note (top root, then fifth), so typing G7 over a G triad works.
- `set_chord`, `continue_with` ("and then X": copy the shape onto a new
  chord) and `describe` (one line per figure) are the API the UI uses and
  the MCP tools should use too.
- The lane above the piano roll is the UI: click a figure, type a chord
  or numeral, Tab moves on, and "+" appends.
- Song harmony: `figure::harmony` reads chords from every pitched track
  together; `figure::set_harmony` re-voices all of them in a span from that
  song chord (so the bass's role is judged against the band, not alone).
  The "All tracks" view stacks every roll under a song chord lane that uses
  it; the agent has `get_harmony`/`set_harmony` (one chord, or a list for a
  section: `["vi","IV","I","V"]`).

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
- Looping is `player::Looping` (`Off`, `Song`, `Range`): Loop goes round
  the selected bars if there are any (`Store::loop_mode`; shift-click the
  roll's ruler, or select in the arranger; Escape lets go), else the song.
  Changing either while playing takes effect at once; the rulers show the
  range as a green band. An event exactly on a loop's end belongs after it.
- Rolls and the arranger follow the playhead a page at a time, and when
  stopped keep the cursor in view.
- `compypal-audio` and `rustysynth` build at opt-level 3 even in dev;
  unoptimized, the synth can't keep up with the callback.
- `cargo run -p compypal-audio --example play_demo` checks the device
  without the UI.

## Note editing

- The grid has one `onmousedown` (`grid_press`) that hit-tests notes itself;
  notes, rows and barlines are `pointer-events: none`. A press arms a
  `Drag::absolute()`; `Store::roll_press/roll_drag/roll_release` turn it into
  a select / move / resize / draw, previewed live (`note_drag`) and committed
  as one undoable edit.
- The roll shows a fixed range (A0–C8, or GM drums) inside a vertical
  scroller, so the grid never re-lays itself out under the pointer.
- Picked notes are a `Selection` with `notes`; tools given `selection: true`
  touch exactly those notes.

## Recording

- `input::MidiIn` (midir) connects at launch to `COMPYPAL_MIDI_IN` (a
  substring) or else the first port that isn't a DAW/loopback port. Its
  callback timestamps with `Instant`, monitors through the engine on the
  selected track's channel, and appends to the running take.
- Rec plays one bar of count-in, then the song from the cursor with an
  open end (`Schedule::length = INFINITY`). The session records
  `downbeat_offset` (the count-in) and `start_tick` (the cursor), which is
  all `import_session` needs to put the take back on the timeline.
- On stop: the session is kept, and a clip is laid down as played
  (sounding durations, so pedalled piano sounds right). "Tidy" re-derives
  it with `cleanup::tidy`; the agent is meant to do better.
- No hardware needed for testing: `cargo run -p compypal-audio --example
  fake_keys -- 10` opens a virtual port and plays an arpeggio after 10s;
  run the app with `COMPYPAL_MIDI_IN=compypal-fake-keys`.
- Latency: a take's clock starts at Rec, but the first click is heard a
  buffer or two later plus the device's output latency. The audio callback
  passes cpal's playback timestamp to `Player::set_clock`, the player
  records when position zero of a Play is heard (`Engine::heard_zero`),
  and `stop_recording` shifts the take by the difference (~56 ms here).
  MIDI input latency itself (~1 ms over USB) is not corrected.
- **Free** (next to Rec): no click, no count-in, nothing playing. On stop
  the pulse is found (`jam::estimate_tempo`, refined by a least-squares
  line through the eighths so long takes don't drift off the grid) and the
  take is laid on the song's grid by its own beats at the cursor's bar
  (`jam::to_session`, as `add_sketch` does). An empty song takes the tempo.
- Not yet: the KeyLab's DAW-port transport buttons.

## Agent (MCP)

The agent is any coding agent; compypal is its editor. Everything it does to
the song goes through MCP tools, whichever way it's connected:

- **Tools**: MCP over HTTP at `127.0.0.1:7766/mcp` (`COMPYPAL_MCP_PORT`),
  registered for this repo by `.mcp.json`. Requests must have a local
  Host/Origin and a JSON content type (blocks DNS rebinding and CSRF from
  web pages). `compypal --mcp-stdio <url>` serves the same tools over stdio
  (`agent::bridge`), for agents that only start MCP servers as processes.
- **Agent panel** (Ctrl+L, `chat.rs`): the app runs the agent as a
  subprocess, the way an editor hosts one. `compypal_mcp::agent` has the
  conversation model (`Event` → `Chat`, no I/O, tested) and two backends
  behind `Backend`: `acp` (the Agent Client Protocol: JSON-RPC over stdio;
  Gemini CLI, Qwen Code, opencode, goose, adapters for Codex and Claude
  Code) and `claude` (Claude Code's own `stream-json` protocol, so it needs
  no adapter). The agent gets compypal's MCP server and nothing else from
  us (no fs/terminal capabilities); each message carries a `<selection>`
  block (in full when it changed, else just its name). The UI says
  "Agent", never a product name.
- **Permissions**: compypal's own tools never ask: they only do what the
  user could do in the editor, and every one undoes (`--allowedTools
  mcp__compypal` for Claude Code; `agent::is_ours` auto-answers ACP
  requests). Everything else (shell, files, web) shows a card.
- **Picking the agent**: `COMPYPAL_AGENT` (an id like `gemini`, or any ACP
  command line) wins, then the one picked in the panel (`settings.json`
  `agent`), then the first known agent on PATH (`agent::installed`).
  `COMPYPAL_AGENT_MODEL` passes a model to Claude Code. Agents run in their
  own process group, killed with the session, so no helper outlives it.
- **Debugging an agent**: `COMPYPAL_AGENT_LOG=<file>` appends every ACP
  message both ways. `cargo build -p compypal-mcp --example fake_agent`,
  then `COMPYPAL_AGENT=$PWD/target/debug/examples/fake_agent` runs a
  scripted agent (no model): it reads the harmony, sets the tempo if asked
  ("tempo"), and asks to run a command ("shell"). `FAKE_AGENT_NO_HTTP=1`
  makes it take the stdio bridge.
- **IDE link** (for `claude` in a terminal): a WebSocket MCP server on a
  random port, advertised by `~/.claude/ide/<port>.lock` (`pid`,
  `workspaceFolders` = compypal's cwd, `ideName`, `transport: "ws"`,
  `authToken`, checked against the `X-Claude-Code-Ide-Authorization`
  header). `claude` started inside that folder offers it under `/ide`. It
  carries `selection_changed` notifications only: Claude Code hides an IDE
  connection's tools from the model (only
  `mcp__ide__executeCode`/`getDiagnostics` get through; checked in the
  2.1.x binary), so tools always come over HTTP. The lockfile is removed
  on exit, and stale ones are swept at startup.
- Tool calls run on the UI thread (`run_on_main_thread` + a thread-local
  store) through the same `Store::edit` as the UI, so agent edits are
  undoable and show up live, labelled "agent: ...".
- A selection is a track, a figure, or bars across all tracks (maybe a
  named section). Tools that take bars also take `section: "Chorus"` or
  `selection: true`; `get_selection`, the panel and the `/ide` push share
  `tools::selection_text`.
- Resources (`resources.rs`): `compypal://song.abc` and
  `compypal://track/<id>.abc`, read through `Dispatch::project()`.
- Add tools in `compypal-mcp/src/tools.rs`: schema in `list()`, a match arm
  in `call()`, a test against `MemoryApp`. Keep answers short, positional
  (`bar.beat.tick`), and in note names.

## Sketches (the journal)

A sketch is one idea you played; it's the only word the UI uses for raw
playing (not take, jam or session).

- With Listen on (remembered in `settings.json`), every MIDI message goes to
  `journal/<utc-day>.jsonl` with unix time, whatever the transport is doing.
  The writer thread (`compypal-audio::journal`) cuts the stream into jams at
  8 s silences, splits each jam into sketches (`jam::split_sketches`:
  pauses of 2.5 s, and long stretches at a phrase break where the key or
  tempo changes; scraps join a neighbour), and appends each sketch's
  summary to `journal/sketches.jsonl`. An old `jams.jsonl` is split once.
  Sketches of the jam still going are listed as they finish.
- `jam::sketch_name` names a sketch by its music ("Am F C G loop"); takes
  recorded with Rec are named the same way.
- `core::jam` has no I/O: `estimate_tempo` (grid fit on eighths plus accent
  alignment on beats, refined by a least-squares line through the eighths),
  `summarize`, `activity`, `to_session` (a sketch starts on its first note
  when that note falls on the found grid's off-beat: without accents the
  beat can't be told from the off-beat).
- The Sketches tab (`sketches.rs`) lists the journal's sketches and the
  song's sessions together by day, each once (a take recorded while
  listening is in both), with a thumbnail; the selected one is drawn whole,
  fitted to the width, with its chords, and can be heard, asked about, or
  added to the song on a chosen track at the end or at the cursor. A song's
  sketch can be taken out (it stays a sketch) or, unused, removed; a
  journal sketch can be hidden (`journal/hidden.json`; the playing stays,
  and the agent doesn't see it). Filters: All, Not in the song, Hidden.
- Added sketches are sessions with `own_tempo`: imported by their own pulse
  (beats, not seconds), so they sit on the song's grid at any song tempo.
- Agent: `list_sketches`, `get_sketch` (chord timeline in mm:ss, activity
  map), `hear_sketch`, `add_sketch` (a stretch, re-timed on that stretch),
  `use_sketch`.
- To test without disturbing your own journal, run with
  `XDG_DATA_HOME=/some/tmp` and the `fake_keys` example (`fake_keys 12 3 4`
  plays three ideas a step apart with 4 s pauses: three sketches).

## Persistence

Each project is `$XDG_DATA_HOME/compypal/projects/<stem>.json`, written on
every change (atomic rename). `settings.json` remembers the open project
(`current`) and whether the journal is listening. The old single
`autosave.json` is migrated into the projects folder on first launch
(kept as `autosave.json.migrated`). Switching projects resets undo. The
projects popover (click the project name) opens, renames, duplicates and
creates projects; the agent has `list_projects` and `open_project`.

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
- In `rsx!`, attribute values and `class`/`style` blocks become `move`
  closures, and a `for` item's `key:` does too: they move whatever
  non-`Copy` value they touch. Give loop items a `Copy` key, and use only
  `Copy` data in attribute closures.
- A `Vec<NodeHandle>` child is wrapped in a `display: contents` box, so
  `<option>`s built that way are not seen by a native `<select>`.
- Never call a `Memo`'s `.get()`, or even a `Signal`'s, inside another
  `Signal::with`: rinch panics with "RefCell already borrowed". Read
  signals and memos first, then borrow.
- An `if let Some(x) = sig.get() { … }` in rsx keeps its old body while it
  stays `Some`: to redraw for a new value, iterate it as a keyed `for`
  (`for x in sig.get().into_iter() { div { key: … } }`).
- `autofocus` only works inside Modal/Popover.
- Inline `<span>`s get no layout box, so they can't be clicked: make
  clickable spans `inline-block`.
- There are no focus/blur events on raw inputs. Text fields live in
  popovers, and `Store::is_typing` (any popover open) keeps shortcuts such
  as Space out of the way. The agent panel's box is always there, so it
  asks `NodeHandle::active_element()` instead (`chat::composer_focused`).
  Build the input as its own node and call `.focus()` on it.
- Keyboard handling for a popup: `overlay_dismiss::arm_keys_while_open`
  (it gets keys only while focus is inside the owner).

## Roadmap

1. ~~Audio~~ (done), with playhead follow and loop ranges.
2. ~~Recording~~ (done). Follow-ups listed under Recording.
3. ~~Agent / music IDE~~ (done), with an in-app agent panel for any ACP
   agent or Claude Code. Follow-ups: `session/load`
   to resume an ACP conversation, rendering replies as Markdown.
4. ~~Arranger view~~ (done): sections, clip rows, bar selection, duplicate /
   insert / delete bars (`core::arrange`), section naming.
4b. ~~Sketches~~ (done): raw playing split into ideas, one list, alternative
   lanes under music, picked per bar range in the roll, hiding and taking
   out. Follow-ups: audio sketches (a guitar via the mic, transcribed to
   notes).
5. **Own synth**: a wavetable/mod-matrix synth in the spirit of Vital, to sit
   alongside the SoundFont player.
