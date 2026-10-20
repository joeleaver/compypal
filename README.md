# compypal

A MIDI composer and arranger that works alongside an AI agent: a small
"music IDE", not a DAW.

Play into it from a MIDI controller and every take is kept exactly as you
played it. An agent, connected over MCP the way Claude Code connects to an
editor, then tidies the timing, removes the slips, and keeps the feel. After
that it helps you arrange. A built-in synth handles previews, and you can
export to Standard MIDI Files and ABC notation.

Built in Rust with [rinch](https://github.com/joeleaver/rinch) for the UI.

## Status

Early. Working today:

- Project model with raw, never-modified recording sessions and the clips derived from them
- Cleanup operations: quantize with strength, swing, and a window; drop ghost and grazed notes; merge double strikes; fix overlaps; timing analysis
- MIDI import and export, and ABC export
- A piano roll that overlays the original take on the cleaned notes

- Chord lane: figures (arpeggios, runs, stabs) labelled with chords; retype a chord to re-voice, or continue a shape onto new chords
- All-tracks view: every piano roll stacked under the song's chords; change a chord there and every part follows
- Arranger: sections and clips across tracks; duplicate, insert or delete bars song-wide
- Playback through a General MIDI SoundFont, metronome, looping
- Recording from a MIDI controller into raw sessions, with live display
- Listen mode: leave it running and everything you play is journaled and split into jams; keep the good bits later, or ask the agent to find them
- An agent panel inside the app (Ctrl+L) that runs Claude Code, Gemini CLI, Qwen Code, opencode, goose, or any agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com)
- MCP tools for reading, cleaning, composing and arranging, usable from any MCP client

Not built yet: the custom synth. See `CLAUDE.md` for the roadmap.

## The agent panel

Press **Ctrl+L** (or click **Agent**) to open it. It starts the first agent
it finds on your PATH; pick another from the menu next to its name. The
agent sees what you've selected, uses compypal's tools to change the song,
and every change shows up as it happens and undoes with Undo. Edits to the
song never need approval; anything outside it (shell commands, files, the
web) asks first.

Agents it knows: Claude Code (`claude`), Gemini CLI (`gemini`), Qwen Code
(`qwen`), opencode, goose, Codex (`codex-acp`) and Claude Code through
its ACP adapter (`claude-code-acp`). For any other ACP agent:

```bash
COMPYPAL_AGENT="my-agent --acp" cargo run -p compypal
```

The agent itself has to be set up (signed in, with a model) before the
panel can use it; if a turn ends with nothing said, try it in a terminal.

## Using it from a terminal

Start compypal, then run `claude` in the same directory:

1. Approve the `compypal` MCP server when asked (it comes from `.mcp.json`).
   That gives the agent the tools: `get_project`, `get_session`, `clean_take`,
   `get_figures`, `set_chord`, `continue_with`, `copy_bars`, `play`, and more.
2. Type `/ide` and pick **compypal**. Now whatever you click in the app (a
   figure, a track) is shared, so "clean this up" or "make this a ii-V"
   means what you're looking at.

To use the tools from another directory:

```bash
claude mcp add --transport http -s user compypal http://127.0.0.1:7766/mcp
```

## Building

```bash
cargo run -p compypal
```

On Linux, rinch's desktop backend needs the usual X11/Wayland development
libraries.
