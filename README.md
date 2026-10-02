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

Not built yet: audio playback, controller recording, the MCP/agent
integration, the arranger view, and the custom synth. See `CLAUDE.md` for
the roadmap.

## Building

```bash
cargo run -p compypal
```

On Linux, rinch's desktop backend needs the usual X11/Wayland development
libraries.
