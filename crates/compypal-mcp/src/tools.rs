//! The tools an agent gets. Each reads or edits the project through
//! [`App`], speaks in bars and note names, and answers in short text that
//! reads well to a model.
//!
//! Sessions (raw takes) are never modified. Cleanup tools always start
//! again from the raw take, so trying different settings is safe.

use compypal_core::cleanup::{self, Quantize};
use compypal_core::figure::{self, FigureSettings};
use compypal_core::text::{self, bar_of, bar_start, format_notes, format_position};
use compypal_core::theory::{self, Chord, Spelling};
use compypal_core::{Id, KeySignature, MeterChange, Note, PPQ, Project, Session, Tick, gm};
use serde_json::{Value, json};

/// What the user is pointing at in the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub track: Id,
    pub start: Tick,
    pub end: Tick,
    pub figure: Option<usize>,
}

/// The app, as tools see it. Implemented by the UI (on its thread) and by
/// a plain in-memory project in tests.
pub trait App {
    fn project(&self) -> Project;
    /// Applies an undoable edit. An error leaves the project unchanged.
    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut Project) -> Result<(), String>) -> Result<(), String>;
    fn undo(&mut self) -> Option<String>;
    fn selection(&self) -> Option<Selection>;
    fn play(&mut self, from: Option<Tick>) -> Result<(), String>;
    fn stop(&mut self);
    fn audition(&mut self, track: Id, notes: &[Note]) -> Result<(), String>;
    /// Writes the project in `format` ("midi" or "abc"); returns the path.
    fn export(&mut self, format: &str) -> Result<String, String>;
}

type ToolResult = Result<String, String>;

pub fn list() -> Value {
    let track = json!({"type": ["string", "integer"], "description": "Track name or id. Defaults to the track selected in the UI."});
    let bar = |what: &str| json!({"type": "integer", "minimum": 1, "description": what});
    let quantize = json!({
        "type": "object",
        "description": "Pull note starts toward a grid.",
        "properties": {
            "grid": {"type": "string", "description": "Note value: 1/4, 1/8, 1/16, 1/8t, 1/16t ..."},
            "strength": {"type": "number", "description": "0..1, how far to move. Default 1."},
            "swing": {"type": "number", "description": "0..~0.33, delays every other grid line. Default 0."},
            "window": {"type": "number", "description": "0..1 of a grid step; notes further off are deliberate and left alone. Default 1."},
            "ends": {"type": "boolean", "description": "Also snap note ends. Default false."}
        },
        "required": ["grid"]
    });
    json!([
        {
            "name": "get_project",
            "description": "Overview of the project: tempo, meter, key, tracks (with instruments and note counts), recorded sessions, sections, and what the user has selected. Start here.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "get_notes",
            "description": "A track's notes, one per line as `bar.beat.tick  pitch  vel  duration`, or as ABC notation for the whole project.",
            "inputSchema": {"type": "object", "properties": {
                "track": track, "from_bar": bar("First bar, inclusive."), "to_bar": bar("Last bar, inclusive."),
                "format": {"type": "string", "enum": ["list", "abc"], "description": "Default list."}
            }}
        },
        {
            "name": "get_figures",
            "description": "A track divided into figures: the musical units you'd talk about (a C arpeggio, a run into G, stabs on Am). Each line: index, position, chord (Roman numeral), shape, note count, confidence, other readings. Figure indexes are what set_chord and continue_with take.",
            "inputSchema": {"type": "object", "properties": {"track": track}}
        },
        {
            "name": "get_session",
            "description": "A raw recorded take, exactly as played: timing against the grid (mean deviation, whether the player sits ahead or behind, measured swing), suspected mistakes (ghost notes, grazed keys, double strikes), and the notes. Use this before clean_take.",
            "inputSchema": {"type": "object", "properties": {
                "session": {"type": ["string", "integer"], "description": "Session name or id. Defaults to the latest."}
            }}
        },
        {
            "name": "clean_take",
            "description": "Rebuilds the clip(s) made from a session, starting again from the raw take each time, so it is safe to retry with different settings. Steps run in this order: remove_quiet, remove_short, merge_double_strikes, quantize, fix_overlaps. Keep the player's intent: prefer partial strength and a window over hard quantizing, and check get_session's timing first. Reports what changed and the timing afterwards.",
            "inputSchema": {"type": "object", "properties": {
                "session": {"type": ["string", "integer"]},
                "remove_quiet": {"type": "integer", "description": "Drop notes softer than this velocity (ghosts are often < 25)."},
                "remove_short": {"type": "string", "description": "Drop notes shorter than this note value, e.g. 1/64."},
                "merge_double_strikes": {"type": "string", "description": "Merge repeats of a pitch starting within this note value, e.g. 1/32."},
                "quantize": quantize,
                "fix_overlaps": {"type": "boolean", "description": "Default true."},
                "sustain": {"type": "boolean", "description": "Use pedal-sustained durations (default true) or key-held ones."}
            }}
        },
        {
            "name": "set_notes",
            "description": "Replaces every note starting in bars from_bar..=to_bar of a track with the given notes. Use for writing or rewriting a passage.",
            "inputSchema": {"type": "object", "properties": {
                "track": track, "from_bar": bar("First bar to replace."), "to_bar": bar("Last bar to replace, inclusive."),
                "notes": {"type": "array", "items": {"type": "object", "properties": {
                    "at": {"type": "string", "description": "bar.beat.tick, e.g. 3.2.240 (960 ticks per beat)."},
                    "pitch": {"type": ["string", "integer"], "description": "C4 = 60. Sharps or flats: F#3, Bb2."},
                    "dur": {"type": "string", "description": "Note value (1/8, 1/4., 1/8t) or ticks."},
                    "vel": {"type": "integer", "description": "1..127, default 90."}
                }, "required": ["at", "pitch", "dur"]}}
            }, "required": ["from_bar", "to_bar", "notes"]}
        },
        {
            "name": "transform",
            "description": "Edits the notes in a bar range of a track (all bars if omitted): transpose, shift in time, scale or flatten velocities, quantize, remove quiet or short notes, or delete.",
            "inputSchema": {"type": "object", "properties": {
                "track": track, "from_bar": bar("First bar."), "to_bar": bar("Last bar, inclusive."),
                "transpose": {"type": "integer", "description": "Semitones."},
                "shift": {"type": "string", "description": "Move in time by a note value or ticks; prefix - for earlier, e.g. -1/16."},
                "velocity_scale": {"type": "number"},
                "velocity_compress": {"type": "number", "description": "0..1, pull velocities toward their mean."},
                "quantize": quantize,
                "remove_quiet": {"type": "integer"},
                "remove_short": {"type": "string"},
                "delete": {"type": "boolean", "description": "Delete the notes in the range."}
            }}
        },
        {
            "name": "set_chord",
            "description": "Re-voices one figure to a new chord, keeping its shape: every note keeps its role (root, third, fifth, seventh, passing tone) and stays near where it was. Accepts chord symbols (Am7, F/A, Bbmaj7, Dø) or Roman numerals in the project key (vi, V7, bVII, V/V).",
            "inputSchema": {"type": "object", "properties": {
                "track": track, "figure": {"type": "integer", "description": "Index from get_figures."},
                "chord": {"type": "string"}
            }, "required": ["figure", "chord"]}
        },
        {
            "name": "continue_with",
            "description": "Composes forward: repeats a figure's shape on each chord in turn, right after it. Each new figure replaces whatever started in its slot. Use to extend a progression quickly: chords [\"Am\", \"F\", \"G\"].",
            "inputSchema": {"type": "object", "properties": {
                "track": track,
                "figure": {"type": "integer", "description": "The figure to continue from. Defaults to the last."},
                "chords": {"type": "array", "items": {"type": "string"}}
            }, "required": ["chords"]}
        },
        {
            "name": "suggest_chords",
            "description": "Chords that fit: completions of partial text, other readings of a figure's notes, and the key's diatonic chords.",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"}, "track": track, "figure": {"type": "integer"}
            }}
        },
        {
            "name": "analyze_key",
            "description": "Estimates the key of a track, or of every track together if none is given.",
            "inputSchema": {"type": "object", "properties": {"track": track}}
        },
        {
            "name": "set_project",
            "description": "Sets project-wide properties.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}, "bpm": {"type": "number"},
                "key": {"type": "string", "description": "e.g. C, F#m, Bb"},
                "meter": {"type": "string", "description": "e.g. 4/4, 3/4, 6/8"}
            }}
        },
        {
            "name": "add_track",
            "description": "Adds a track. Instruments are General MIDI: give a name fragment (\"nylon\", \"strings\", \"rhodes\" won't match but \"electric piano\" will) or a program number 0-127.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}, "instrument": {"type": ["string", "integer"]},
                "drums": {"type": "boolean", "description": "A General MIDI drum track (channel 10)."}
            }, "required": ["name"]}
        },
        {
            "name": "set_track",
            "description": "Changes a track's name, instrument, volume (0-127), pan (0-127, 64 centre), mute or solo.",
            "inputSchema": {"type": "object", "properties": {
                "track": track, "name": {"type": "string"}, "instrument": {"type": ["string", "integer"]},
                "volume": {"type": "integer"}, "pan": {"type": "integer"}, "mute": {"type": "boolean"}, "solo": {"type": "boolean"}
            }}
        },
        {
            "name": "copy_bars",
            "description": "Arranging: copies bars from_bar..from_bar+bars-1 to start at to_bar, on the given tracks (default all). The destination is cleared first unless merge is true.",
            "inputSchema": {"type": "object", "properties": {
                "from_bar": bar("First bar to copy."), "bars": {"type": "integer", "minimum": 1}, "to_bar": bar("Where the copy starts."),
                "tracks": {"type": "array", "items": {"type": ["string", "integer"]}},
                "merge": {"type": "boolean"}
            }, "required": ["from_bar", "bars", "to_bar"]}
        },
        {
            "name": "set_sections",
            "description": "Replaces the arrangement's section markers (intro, verse, chorus...).",
            "inputSchema": {"type": "object", "properties": {
                "sections": {"type": "array", "items": {"type": "object", "properties": {
                    "name": {"type": "string"}, "from_bar": {"type": "integer"}, "bars": {"type": "integer"}
                }, "required": ["name", "from_bar", "bars"]}}
            }, "required": ["sections"]}
        },
        {
            "name": "play",
            "description": "Plays the project for the user from a bar (default the start), looping.",
            "inputSchema": {"type": "object", "properties": {"from_bar": bar("Where to start.")}}
        },
        {"name": "stop", "description": "Stops playback.", "inputSchema": {"type": "object", "properties": {}}},
        {
            "name": "audition",
            "description": "Plays just one track's bars for the user, once, on its instrument.",
            "inputSchema": {"type": "object", "properties": {"track": track, "from_bar": bar("First bar."), "to_bar": bar("Last bar.")}}
        },
        {
            "name": "undo",
            "description": "Undoes the last edit, whoever made it.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "export",
            "description": "Writes the project to the exports folder as a Standard MIDI File or ABC notation.",
            "inputSchema": {"type": "object", "properties": {"format": {"type": "string", "enum": ["midi", "abc"]}}, "required": ["format"]}
        }
    ])
}

pub fn call(app: &mut dyn App, name: &str, args: &Value) -> ToolResult {
    match name {
        "get_project" => get_project(app),
        "get_notes" => get_notes(app, args),
        "get_figures" => get_figures(app, args),
        "get_session" => get_session(app, args),
        "clean_take" => clean_take(app, args),
        "set_notes" => set_notes(app, args),
        "transform" => transform(app, args),
        "set_chord" => set_chord(app, args),
        "continue_with" => continue_with(app, args),
        "suggest_chords" => suggest_chords(app, args),
        "analyze_key" => analyze_key(app, args),
        "set_project" => set_project(app, args),
        "add_track" => add_track(app, args),
        "set_track" => set_track(app, args),
        "copy_bars" => copy_bars(app, args),
        "set_sections" => set_sections(app, args),
        "play" => {
            let p = app.project();
            let from = opt_u32(args, "from_bar").map(|b| bar_start(&p, b));
            app.play(from)?;
            Ok(format!("Playing from bar {}", from.map_or(1, |t| bar_of(&p, t))))
        }
        "stop" => {
            app.stop();
            Ok("Stopped".into())
        }
        "audition" => audition(app, args),
        "undo" => Ok(app.undo().map_or("Nothing to undo".into(), |l| format!("Undid {l}"))),
        "export" => {
            let f = args.get("format").and_then(Value::as_str).unwrap_or("midi");
            app.export(f).map(|p| format!("Wrote {p}"))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

// --- argument helpers ------------------------------------------------------

fn opt_u32(args: &Value, key: &str) -> Option<u32> {
    args.get(key).and_then(Value::as_u64).map(|n| n as u32)
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn find_track(p: &Project, v: &Value) -> Result<Id, String> {
    if let Some(n) = v.as_u64()
        && p.track(Id(n)).is_some()
    {
        return Ok(Id(n));
    }
    let name = v.as_str().map(str::to_lowercase).unwrap_or_else(|| v.to_string());
    p.tracks
        .iter()
        .find(|t| t.name.to_lowercase() == name)
        .or_else(|| p.tracks.iter().find(|t| t.name.to_lowercase().contains(&name)))
        .map(|t| t.id)
        .ok_or_else(|| format!("no track {v}; tracks are {}", track_names(p)))
}

fn track_names(p: &Project) -> String {
    p.tracks.iter().map(|t| format!("{:?} ({})", t.name, t.id)).collect::<Vec<_>>().join(", ")
}

/// The `track` argument, else the UI's selected track.
fn track_arg(app: &dyn App, p: &Project, args: &Value) -> Result<Id, String> {
    match args.get("track") {
        Some(v) if !v.is_null() => find_track(p, v),
        _ => app
            .selection()
            .map(|s| s.track)
            .or_else(|| p.tracks.first().map(|t| t.id))
            .ok_or_else(|| "the project has no tracks".to_string()),
    }
}

/// Bars `from_bar..=to_bar` as a tick range; open-ended when omitted.
fn bar_range(p: &Project, args: &Value) -> (Tick, Tick) {
    let from = opt_u32(args, "from_bar").map_or(0, |b| bar_start(p, b));
    let to = opt_u32(args, "to_bar").map_or(Tick::MAX, |b| bar_start(p, b + 1));
    (from, to)
}

fn find_session(p: &Project, v: Option<&Value>) -> Result<Session, String> {
    let found = match v {
        None | Some(Value::Null) => p.sessions.last(),
        Some(v) => v
            .as_u64()
            .and_then(|n| p.session(Id(n)))
            .or_else(|| {
                let name = v.as_str().unwrap_or_default().to_lowercase();
                p.sessions.iter().find(|s| s.name.to_lowercase() == name)
            }),
    };
    found.cloned().ok_or_else(|| {
        let names: Vec<String> = p.sessions.iter().map(|s| format!("{:?} ({})", s.name, s.id)).collect();
        format!("no such session; sessions are {}", if names.is_empty() { "none".into() } else { names.join(", ") })
    })
}

fn duration_arg(args: &Value, key: &str) -> Result<Option<Tick>, String> {
    match opt_str(args, key) {
        None => Ok(None),
        Some(s) => text::parse_duration(s).map(Some).ok_or_else(|| format!("{key}: can't read duration {s:?}")),
    }
}

fn quantize_arg(v: &Value) -> Result<Quantize, String> {
    let grid = v.get("grid").and_then(Value::as_str).ok_or("quantize needs a grid")?;
    let grid = text::parse_duration(grid).ok_or_else(|| format!("can't read grid {grid:?}"))?;
    let num = |k: &str, d: f64| v.get(k).and_then(Value::as_f64).unwrap_or(d);
    Ok(Quantize {
        grid,
        strength: num("strength", 1.0).clamp(0.0, 1.0),
        swing: num("swing", 0.0).clamp(0.0, 0.5),
        window: num("window", 1.0).clamp(0.0, 1.0),
        ends: v.get("ends").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn instrument_arg(v: &Value) -> Result<u8, String> {
    if let Some(n) = v.as_u64() {
        return Ok((n as u8).min(127));
    }
    let s = v.as_str().unwrap_or_default();
    gm::find_program(s).ok_or_else(|| format!("no General MIDI instrument matches {s:?}"))
}

fn chord_arg(p: &Project, s: &str) -> Result<Chord, String> {
    theory::parse_chord_or_roman(s, p.key).map_err(|e| e.to_string())
}

fn spell(p: &Project) -> Spelling {
    Spelling::for_key(p.key)
}

fn notes_in(p: &Project, track: Id, from: Tick, to: Tick) -> Vec<Note> {
    p.track(track)
        .map(|t| t.absolute_notes().into_iter().filter(|n| n.start >= from && n.start < to).collect())
        .unwrap_or_default()
}

// --- reading ---------------------------------------------------------------

fn get_project(app: &mut dyn App) -> ToolResult {
    let p = app.project();
    let m = p.meter_at(0);
    let bars = bar_of(&p, p.end_tick().saturating_sub(1));
    let mut out = format!(
        "{}: {:.0} BPM, {}, key {}, {} bars ({} ticks per beat)\n",
        p.name,
        p.tempo.bpm_at(0),
        text::meter_text(&m),
        p.key.name(),
        if p.end_tick() == 0 { 0 } else { bars },
        PPQ
    );
    out.push_str("\nTracks:\n");
    for t in &p.tracks {
        let notes = t.absolute_notes();
        let span = match (notes.first(), notes.iter().map(|n| n.end()).max()) {
            (Some(a), Some(b)) => format!("bars {}-{}", bar_of(&p, a.start), bar_of(&p, b.saturating_sub(1))),
            _ => "empty".into(),
        };
        let mut flags = Vec::new();
        if t.muted {
            flags.push("muted");
        }
        if t.solo {
            flags.push("solo");
        }
        out.push_str(&format!(
            "  {} {:?}: {}, ch {}, {} notes, {}{}\n",
            t.id,
            t.name,
            if t.is_drums() { "Drums".to_string() } else { gm::program_name(t.program).to_string() },
            t.channel + 1,
            notes.len(),
            span,
            if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) }
        ));
    }
    out.push_str("\nSessions (raw takes):\n");
    if p.sessions.is_empty() {
        out.push_str("  none\n");
    }
    for s in &p.sessions {
        let used: Vec<String> = p
            .tracks
            .iter()
            .filter(|t| t.clips.iter().any(|c| c.source_session == Some(s.id)))
            .map(|t| format!("{:?}", t.name))
            .collect();
        out.push_str(&format!(
            "  {} {:?}: {} notes, {:.1}s, from bar {}, on {}\n",
            s.id,
            s.name,
            s.notes().len(),
            s.duration(),
            bar_of(&p, s.start_tick),
            if used.is_empty() { "no track".into() } else { used.join(", ") }
        ));
    }
    if !p.sections.is_empty() {
        out.push_str("\nSections:\n");
        for s in &p.sections {
            out.push_str(&format!(
                "  {}: bars {}-{}\n",
                s.name,
                bar_of(&p, s.start),
                bar_of(&p, (s.start + s.length).saturating_sub(1))
            ));
        }
    }
    if let Some(sel) = app.selection() {
        let name = p.track(sel.track).map_or("?".into(), |t| t.name.clone());
        let what = match sel.figure {
            Some(f) => format!("figure {f}, {} to {}", format_position(&p, sel.start), format_position(&p, sel.end)),
            None if sel.end == Tick::MAX => "the whole track".to_string(),
            None => format!("{} to {}", format_position(&p, sel.start), format_position(&p, sel.end)),
        };
        out.push_str(&format!("\nSelected in the UI: {name:?}, {what}\n"));
    }
    Ok(out)
}

fn get_notes(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    if opt_str(args, "format") == Some("abc") {
        return Ok(compypal_io::abc::export(&p, &Default::default()));
    }
    let track = track_arg(app, &p, args)?;
    let (from, to) = bar_range(&p, args);
    let notes = notes_in(&p, track, from, to);
    if notes.is_empty() {
        return Ok("No notes in that range.".into());
    }
    Ok(format!("{} notes:\n{}", notes.len(), format_notes(&p, &notes)))
}

fn get_figures(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
    let figs = figure::analyze(&notes, &p.meter_at(0), &FigureSettings::default());
    if figs.is_empty() {
        return Ok("The track is empty.".into());
    }
    let described = figure::describe(&figs, p.key, &p.meter_at(0));
    Ok(format!("index  start  chord (numeral in {})  shape  notes  confidence  [alternatives]\n{described}", p.key.name()))
}

fn get_session(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let s = find_session(&p, args.get("session"))?;
    let imported = cleanup::import_session(&s, &p.tempo, true);
    let shift = s.start_tick as i64 - (imported.pickup_bars as u64 * s.meter.ticks_per_bar()) as i64;
    let mut notes = imported.notes;
    cleanup::shift(&mut notes, shift);
    let q = PPQ as Tick;
    let mut out = format!(
        "{:?} ({}): {} notes over {:.1}s, recorded from bar {} at {} BPM{}\n",
        s.name,
        s.id,
        notes.len(),
        s.duration(),
        bar_of(&p, s.start_tick),
        s.click_bpm.map_or("free time".into(), |b| format!("{b:.0}")),
        if imported.pickup_bars > 0 { format!(", with {} pickup bar(s)", imported.pickup_bars) } else { String::new() }
    );
    out.push_str("\nTiming (ticks; 960 per beat; + means late):\n");
    for (name, grid) in [("1/8", q / 2), ("1/16", q / 4)] {
        let r = cleanup::timing_report(&notes, grid);
        out.push_str(&format!(
            "  vs {name}: mean distance {:.0}, mean offset {:+.0}, {:.0}% within a 1/64, offbeat lag {:.2} of a step\n",
            r.mean_abs_deviation,
            r.mean_signed_deviation,
            r.tight_fraction * 100.0,
            r.measured_swing
        ));
    }
    let mut suspects = Vec::new();
    // A restruck key cuts the first strike short; report it once, as the
    // double strike it is, not also as a slip.
    let doubled: Vec<bool> = notes
        .iter()
        .enumerate()
        .map(|(i, a)| notes[i + 1..].iter().take_while(|b| b.start - a.start < q / 8).any(|b| b.pitch == a.pitch))
        .collect();
    for (n, &twice) in notes.iter().zip(&doubled) {
        let at = format!("{} {}", format_position(&p, n.start), gm::pitch_name(n.pitch));
        if twice {
            suspects.push(format!("  {at}: struck twice within a 1/32"));
        } else if n.velocity < 25 {
            suspects.push(format!("  {at} vel {}: very quiet (ghost/grazed key?)", n.velocity));
        } else if n.duration < q / 16 {
            suspects.push(format!("  {at} {} ticks: very short (slip?)", n.duration));
        }
    }
    out.push_str(&format!("\nSuspected mistakes ({}):\n", suspects.len()));
    out.push_str(if suspects.is_empty() { "  none\n" } else { "" });
    for l in suspects {
        out.push_str(&l);
        out.push('\n');
    }
    if let Some((key, r)) = theory::detect_key(&notes) {
        out.push_str(&format!("\nKey estimate: {} (confidence {r:.2})\n", key.name()));
    }
    out.push_str(&format!("\nNotes as played:\n{}", format_notes(&p, &notes)));
    Ok(out)
}

// --- cleanup ---------------------------------------------------------------

fn clean_take(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let s = find_session(&p, args.get("session"))?;
    let sustain = args.get("sustain").and_then(Value::as_bool).unwrap_or(true);
    let quiet = args.get("remove_quiet").and_then(Value::as_u64).map(|v| v as u8);
    let short = duration_arg(args, "remove_short")?;
    let doubles = duration_arg(args, "merge_double_strikes")?;
    let quant = args.get("quantize").map(quantize_arg).transpose()?;
    let overlaps = args.get("fix_overlaps").and_then(Value::as_bool).unwrap_or(true);

    let imported = cleanup::import_session(&s, &p.tempo, sustain);
    let mut notes = imported.notes;
    let before = notes.len();
    let mut report = Vec::new();
    if let Some(v) = quiet {
        report.push(format!("{} quiet notes removed", cleanup::remove_quiet(&mut notes, v)));
    }
    if let Some(d) = short {
        report.push(format!("{} short notes removed", cleanup::remove_short(&mut notes, d)));
    }
    if let Some(w) = doubles {
        report.push(format!("{} double strikes merged", cleanup::merge_double_strikes(&mut notes, w)));
    }
    if let Some(q) = &quant {
        report.push(format!("{} notes moved toward the grid", cleanup::quantize(&mut notes, q)));
    }
    if overlaps {
        report.push(format!("{} overlaps fixed", cleanup::fix_overlaps(&mut notes)));
    }

    let mut clips = 0;
    let fresh = notes.clone();
    app.edit(&format!("clean {}", s.name), &mut |p| {
        clips = 0;
        for t in &mut p.tracks {
            for c in t.clips.iter_mut().filter(|c| c.source_session == Some(s.id)) {
                c.notes = fresh.clone();
                clips += 1;
            }
        }
        if clips == 0 {
            return Err(format!("no clip comes from {:?}; record it onto a track first", s.name));
        }
        Ok(())
    })?;

    let q = PPQ as Tick;
    let grid = quant.map_or(q / 4, |q| q.grid);
    let r = cleanup::timing_report(&notes, grid);
    Ok(format!(
        "Rebuilt {clips} clip(s) from {:?}: {before} notes in, {} out. {}.\nNow vs {}: mean distance {:.0} ticks, mean offset {:+.0}, {:.0}% within a 1/64.",
        s.name,
        notes.len(),
        report.join(", "),
        text::format_duration(grid),
        r.mean_abs_deviation,
        r.mean_signed_deviation,
        r.tight_fraction * 100.0
    ))
}

// --- writing ---------------------------------------------------------------

fn set_notes(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let from_bar = opt_u32(args, "from_bar").ok_or("from_bar is required")?;
    let to_bar = opt_u32(args, "to_bar").ok_or("to_bar is required")?;
    let (from, to) = (bar_start(&p, from_bar), bar_start(&p, to_bar + 1));
    let mut new = Vec::new();
    for (i, n) in args.get("notes").and_then(Value::as_array).ok_or("notes must be an array")?.iter().enumerate() {
        let at = n.get("at").and_then(Value::as_str).ok_or_else(|| format!("note {i}: missing at"))?;
        let start = text::parse_position(&p, at).ok_or_else(|| format!("note {i}: can't read position {at:?}"))?;
        let pitch = match n.get("pitch") {
            Some(Value::Number(x)) => x.as_u64().filter(|&x| x <= 127).map(|x| x as u8),
            Some(Value::String(s)) => text::parse_pitch(s),
            _ => None,
        }
        .ok_or_else(|| format!("note {i}: can't read pitch {}", n.get("pitch").unwrap_or(&Value::Null)))?;
        let dur = match n.get("dur") {
            Some(Value::Number(x)) => x.as_u64(),
            Some(Value::String(s)) => text::parse_duration(s),
            _ => None,
        }
        .ok_or_else(|| format!("note {i}: can't read dur"))?;
        if start < from || start >= to {
            return Err(format!("note {i} at {at} is outside bars {from_bar}-{to_bar}"));
        }
        let vel = n.get("vel").and_then(Value::as_u64).unwrap_or(90).clamp(1, 127) as u8;
        new.push(Note { pitch, velocity: vel, start, duration: dur.max(1) });
    }
    let old = notes_in(&p, track, from, to);
    let count = new.len();
    app.edit("set notes", &mut |p| {
        p.replace_notes(track, &old, &new);
        Ok(())
    })?;
    Ok(format!("Bars {from_bar}-{to_bar}: replaced {} notes with {count}.", old.len()))
}

fn transform(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let (from, to) = bar_range(&p, args);
    let old = notes_in(&p, track, from, to);
    let mut notes = old.clone();
    let mut did = Vec::new();
    if args.get("delete").and_then(Value::as_bool) == Some(true) {
        notes.clear();
        did.push(format!("deleted {}", old.len()));
    }
    if let Some(v) = args.get("remove_quiet").and_then(Value::as_u64) {
        did.push(format!("{} quiet removed", cleanup::remove_quiet(&mut notes, v as u8)));
    }
    if let Some(d) = duration_arg(args, "remove_short")? {
        did.push(format!("{} short removed", cleanup::remove_short(&mut notes, d)));
    }
    if let Some(st) = args.get("transpose").and_then(Value::as_i64) {
        cleanup::transpose(&mut notes, st.clamp(-127, 127) as i8);
        did.push(format!("transposed {st:+}"));
    }
    if let Some(s) = opt_str(args, "shift") {
        let (neg, body) = s.strip_prefix('-').map_or((false, s), |b| (true, b));
        let d = text::parse_duration(body).ok_or_else(|| format!("can't read shift {s:?}"))? as i64;
        cleanup::shift(&mut notes, if neg { -d } else { d });
        did.push(format!("shifted {s}"));
    }
    if let Some(k) = args.get("velocity_scale").and_then(Value::as_f64) {
        for n in &mut notes {
            n.velocity = (n.velocity as f64 * k).round().clamp(1.0, 127.0) as u8;
        }
        did.push(format!("velocity x{k}"));
    }
    if let Some(a) = args.get("velocity_compress").and_then(Value::as_f64) {
        cleanup::compress_velocity(&mut notes, a);
        did.push(format!("velocity compressed {a}"));
    }
    if let Some(q) = args.get("quantize") {
        let q = quantize_arg(q)?;
        did.push(format!("{} quantized", cleanup::quantize(&mut notes, &q)));
    }
    if did.is_empty() {
        return Err("nothing to do: give at least one change".into());
    }
    app.edit("transform", &mut |p| {
        p.replace_notes(track, &old, &notes);
        Ok(())
    })?;
    Ok(format!("{} notes: {}.", old.len(), did.join(", ")))
}

fn set_chord(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let index = args.get("figure").and_then(Value::as_u64).ok_or("figure is required")? as usize;
    let chord = chord_arg(&p, opt_str(args, "chord").ok_or("chord is required")?)?;
    app.edit(&format!("chord {}", chord.name(spell(&p))), &mut |p| {
        figure::set_chord(p, track, index, &chord, &FigureSettings::default()).map_err(|e| e.to_string())
    })?;
    audition_figure(app, track, index);
    Ok(format!("Figure {index} is now {} ({}).", chord.name(spell(&p)), chord.roman(p.key)))
}

fn continue_with(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let chords: Vec<Chord> = args
        .get("chords")
        .and_then(Value::as_array)
        .ok_or("chords must be an array")?
        .iter()
        .map(|c| chord_arg(&p, c.as_str().unwrap_or_default()))
        .collect::<Result<_, _>>()?;
    let s = FigureSettings::default();
    let start_index = match args.get("figure").and_then(Value::as_u64) {
        Some(i) => i as usize,
        None => {
            let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
            let n = figure::analyze(&notes, &p.meter_at(0), &s).len();
            if n == 0 {
                return Err("the track has no figure to continue from; write or record one first".into());
            }
            n - 1
        }
    };
    let names: Vec<String> = chords.iter().map(|c| c.name(spell(&p))).collect();
    app.edit(&format!("then {}", names.join(" ")), &mut |p| {
        // Each new figure becomes the one the next chord continues from.
        let mut from = start_index;
        for c in &chords {
            let before = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
            let figs = figure::analyze(&before, &p.meter_at(0), &s);
            let (at, _, _) = figure::continued(&figs, from.min(figs.len().saturating_sub(1)), c, p);
            figure::continue_with(p, track, from, c, &s).map_err(|e| e.to_string())?;
            let after = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
            let figs = figure::analyze(&after, &p.meter_at(0), &s);
            from = figs.iter().position(|f| f.start + PPQ as Tick / 8 >= at).unwrap_or(figs.len() - 1);
        }
        Ok(())
    })?;
    let p = app.project();
    let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
    let figs = figure::analyze(&notes, &p.meter_at(0), &s);
    Ok(format!(
        "Continued with {}. Figures now:\n{}",
        names.join(", "),
        figure::describe(&figs, p.key, &p.meter_at(0))
    ))
}

fn suggest_chords(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let mut context = Vec::new();
    if let Some(i) = args.get("figure").and_then(Value::as_u64) {
        let track = track_arg(app, &p, args)?;
        let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
        let figs = figure::analyze(&notes, &p.meter_at(0), &FigureSettings::default());
        if let Some(f) = figs.get(i as usize) {
            context.push(f.chord);
            context.extend(f.alternatives.iter().copied());
        }
    }
    let list = theory::suggest(opt_str(args, "text").unwrap_or(""), p.key, &context, 12);
    Ok(list.iter().map(|c| format!("{} ({})", c.name(spell(&p)), c.roman(p.key))).collect::<Vec<_>>().join(", "))
}

fn analyze_key(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let notes: Vec<Note> = match args.get("track") {
        Some(v) if !v.is_null() => p.track(find_track(&p, v)?).map(|t| t.absolute_notes()).unwrap_or_default(),
        _ => p.tracks.iter().filter(|t| !t.is_drums()).flat_map(|t| t.absolute_notes()).collect(),
    };
    match theory::detect_key(&notes) {
        Some((k, r)) => Ok(format!(
            "{} (correlation {r:.2}); the project key is set to {}.",
            k.name(),
            p.key.name()
        )),
        None => Ok("No pitched notes to judge from.".into()),
    }
}

fn set_project(app: &mut dyn App, args: &Value) -> ToolResult {
    let key = opt_str(args, "key").map(|k| KeySignature::parse(k).ok_or_else(|| format!("can't read key {k:?}"))).transpose()?;
    let meter = opt_str(args, "meter")
        .map(|m| {
            let (n, d) = m.split_once('/').ok_or_else(|| format!("can't read meter {m:?}"))?;
            let (n, d): (u8, u8) = (n.trim().parse().map_err(|_| "bad meter")?, d.trim().parse().map_err(|_| "bad meter")?);
            if n == 0 || !d.is_power_of_two() {
                return Err(format!("can't use meter {m}"));
            }
            Ok(MeterChange { tick: 0, numerator: n, denominator: d })
        })
        .transpose()?;
    let bpm = args.get("bpm").and_then(Value::as_f64);
    if bpm.is_some_and(|b| !(20.0..=400.0).contains(&b)) {
        return Err("bpm must be between 20 and 400".into());
    }
    let name = opt_str(args, "name").map(str::to_string);
    app.edit("project settings", &mut |p| {
        if let Some(n) = &name {
            p.name = n.clone();
        }
        if let Some(b) = bpm {
            p.tempo.set(0, b);
        }
        if let Some(k) = key {
            p.key = k;
        }
        if let Some(m) = meter {
            p.meter.retain(|x| x.tick != 0);
            p.meter.insert(0, m);
        }
        Ok(())
    })?;
    let p = app.project();
    Ok(format!("{}: {:.0} BPM, {}, key {}", p.name, p.tempo.bpm_at(0), text::meter_text(&p.meter_at(0)), p.key.name()))
}

fn add_track(app: &mut dyn App, args: &Value) -> ToolResult {
    let name = opt_str(args, "name").ok_or("name is required")?.to_string();
    let drums = args.get("drums").and_then(Value::as_bool).unwrap_or(false);
    let program = match args.get("instrument") {
        Some(v) if !drums => instrument_arg(v)?,
        _ => 0,
    };
    let mut id = Id(0);
    app.edit(&format!("add {name}"), &mut |p| {
        id = if drums { p.add_drum_track(name.clone()) } else { p.add_track(name.clone(), program) };
        Ok(())
    })?;
    Ok(format!(
        "Added track {id} {name:?}: {}",
        if drums { "Drums" } else { gm::program_name(program) }
    ))
}

fn set_track(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let program = args.get("instrument").map(instrument_arg).transpose()?;
    let name = opt_str(args, "name").map(str::to_string);
    let byte = |k: &str| args.get(k).and_then(Value::as_u64).map(|v| v.min(127) as u8);
    let (volume, pan) = (byte("volume"), byte("pan"));
    let (mute, solo) = (args.get("mute").and_then(Value::as_bool), args.get("solo").and_then(Value::as_bool));
    app.edit("track settings", &mut |p| {
        let t = p.track_mut(track).ok_or("no such track")?;
        if let Some(n) = &name {
            t.name = n.clone();
        }
        if let Some(pr) = program {
            t.program = pr;
        }
        if let Some(v) = volume {
            t.volume = v;
        }
        if let Some(v) = pan {
            t.pan = v;
        }
        if let Some(v) = mute {
            t.muted = v;
        }
        if let Some(v) = solo {
            t.solo = v;
        }
        Ok(())
    })?;
    let p = app.project();
    let t = p.track(track).unwrap();
    Ok(format!(
        "{:?}: {}, volume {}, pan {}{}{}",
        t.name,
        if t.is_drums() { "Drums" } else { gm::program_name(t.program) },
        t.volume,
        t.pan,
        if t.muted { ", muted" } else { "" },
        if t.solo { ", solo" } else { "" }
    ))
}

fn copy_bars(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let from_bar = opt_u32(args, "from_bar").ok_or("from_bar is required")?;
    let bars = opt_u32(args, "bars").ok_or("bars is required")?.max(1);
    let to_bar = opt_u32(args, "to_bar").ok_or("to_bar is required")?;
    let merge = args.get("merge").and_then(Value::as_bool).unwrap_or(false);
    let tracks: Vec<Id> = match args.get("tracks").and_then(Value::as_array) {
        Some(list) => list.iter().map(|v| find_track(&p, v)).collect::<Result<_, _>>()?,
        None => p.tracks.iter().map(|t| t.id).collect(),
    };
    let (src, src_end) = (bar_start(&p, from_bar), bar_start(&p, from_bar + bars));
    let (dst, dst_end) = (bar_start(&p, to_bar), bar_start(&p, to_bar + bars));
    let mut copied = 0;
    app.edit(&format!("copy bars {from_bar}-{}", from_bar + bars - 1), &mut |p| {
        copied = 0;
        for &t in &tracks {
            let source = notes_in(p, t, src, src_end);
            let cleared = if merge { Vec::new() } else { notes_in(p, t, dst, dst_end) };
            let moved: Vec<Note> = source.iter().map(|n| Note { start: n.start - src + dst, ..*n }).collect();
            copied += moved.len();
            p.replace_notes(t, &cleared, &moved);
        }
        Ok(())
    })?;
    Ok(format!(
        "Copied bars {from_bar}-{} to {to_bar}-{} on {} track(s): {copied} notes.",
        from_bar + bars - 1,
        to_bar + bars - 1,
        tracks.len()
    ))
}

fn set_sections(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let list = args.get("sections").and_then(Value::as_array).ok_or("sections must be an array")?;
    let mut sections = Vec::new();
    for s in list {
        let name = s.get("name").and_then(Value::as_str).ok_or("each section needs a name")?.to_string();
        let from = s.get("from_bar").and_then(Value::as_u64).ok_or("each section needs from_bar")? as u32;
        let bars = s.get("bars").and_then(Value::as_u64).ok_or("each section needs bars")? as u32;
        let start = bar_start(&p, from);
        sections.push((name, start, bar_start(&p, from + bars) - start));
    }
    let n = sections.len();
    app.edit("sections", &mut |p| {
        p.sections.clear();
        for (name, start, length) in &sections {
            let id = p.alloc_id();
            p.sections.push(compypal_core::Section { id, name: name.clone(), start: *start, length: *length });
        }
        Ok(())
    })?;
    Ok(format!("{n} section(s) set."))
}

fn audition(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let (from, to) = bar_range(&p, args);
    let notes = notes_in(&p, track, from, to);
    if notes.is_empty() {
        return Err("no notes there to play".into());
    }
    app.audition(track, &notes)?;
    Ok(format!("Playing {} notes.", notes.len()))
}

fn audition_figure(app: &mut dyn App, track: Id, index: usize) {
    let p = app.project();
    let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
    if let Some(f) = figure::analyze(&notes, &p.meter_at(0), &FigureSettings::default()).get(index) {
        let _ = app.audition(track, &f.notes);
    }
}

/// An [`App`] over a bare project, with real undo: for tests, and for
/// running tools with no UI.
pub struct MemoryApp {
    pub project: Project,
    pub history: compypal_core::History,
    pub selection: Option<Selection>,
}

impl MemoryApp {
    pub fn new(project: Project) -> Self {
        Self { project, history: Default::default(), selection: None }
    }
}

impl App for MemoryApp {
    fn project(&self) -> Project {
        self.project.clone()
    }
    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut Project) -> Result<(), String>) -> Result<(), String> {
        let mut next = self.project.clone();
        f(&mut next)?;
        self.history.push(label, std::mem::replace(&mut self.project, next));
        Ok(())
    }
    fn undo(&mut self) -> Option<String> {
        self.history.undo(&mut self.project)
    }
    fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }
    fn play(&mut self, _: Option<Tick>) -> Result<(), String> {
        Err("no audio here".into())
    }
    fn stop(&mut self) {}
    fn audition(&mut self, _: Id, _: &[Note]) -> Result<(), String> {
        Ok(())
    }
    fn export(&mut self, _: &str) -> Result<String, String> {
        Err("no exports here".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> MemoryApp {
        MemoryApp::new(compypal_core::demo::project())
    }

    fn run(app: &mut MemoryApp, name: &str, args: Value) -> String {
        call(app, name, &args).unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    #[test]
    fn every_listed_tool_dispatches() {
        let names: Vec<String> =
            list().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        let mut app = demo();
        for n in names {
            // Missing arguments are fine; an unknown tool is not.
            if let Err(e) = call(&mut app, &n, &json!({})) {
                assert!(!e.starts_with("unknown tool"), "{n}");
            }
        }
    }

    #[test]
    fn overview_and_reading() {
        let mut app = demo();
        let o = run(&mut app, "get_project", json!({}));
        assert!(o.contains("100 BPM") && o.contains("\"Keys\"") && o.contains("Arpeggio take 1"), "{o}");
        let n = run(&mut app, "get_notes", json!({"track": "bass", "from_bar": 2, "to_bar": 2}));
        assert!(n.starts_with("3 notes") && n.contains("2.1.0  A1"), "{n}");
        let f = run(&mut app, "get_figures", json!({"track": "Keys"}));
        assert!(f.contains("C (I)  arpeggio") && f.contains("G (V)"), "{f}");
    }

    #[test]
    fn session_report_finds_the_planted_mistakes() {
        let mut app = demo();
        let s = run(&mut app, "get_session", json!({}));
        assert!(s.contains("Suspected mistakes (3)"), "{s}");
        assert!(s.contains("C#4 vel 22"), "{s}");
    }

    #[test]
    fn clean_take_rebuilds_from_raw_and_undoes() {
        let mut app = demo();
        let before = app.project.tracks[0].clips[0].notes.clone();
        let r = run(
            &mut app,
            "clean_take",
            json!({"remove_quiet": 25, "merge_double_strikes": "1/32", "quantize": {"grid": "1/8", "strength": 1.0}}),
        );
        assert!(r.contains("35 notes in, 32 out"), "{r}");
        let after = &app.project.tracks[0].clips[0].notes;
        assert!(after.iter().all(|n| n.start % 480 == 0), "fully quantized");
        run(&mut app, "undo", json!({}));
        assert_eq!(app.project.tracks[0].clips[0].notes, before);
    }

    #[test]
    fn writing_and_composing() {
        let mut app = demo();
        let r = run(
            &mut app,
            "set_notes",
            json!({"track": "Bass", "from_bar": 1, "to_bar": 1, "notes": [
                {"at": "1.1", "pitch": "C2", "dur": "1/2"},
                {"at": "1.3", "pitch": "G1", "dur": "1/2", "vel": 70}
            ]}),
        );
        assert!(r.contains("replaced 3 notes with 2"), "{r}");
        run(&mut app, "set_chord", json!({"track": "Keys", "figure": 1, "chord": "iii"}));
        let r = run(&mut app, "continue_with", json!({"track": "Keys", "chords": ["Am", "Dm7", "G7"]}));
        assert!(r.contains("C (I)") && r.contains("Em (iii)"), "{r}");
        let names: Vec<_> = r.lines().skip(1).map(|l| l.split("  ").nth(2).unwrap().to_string()).collect();
        assert_eq!(names, ["C (I)", "Em (iii)", "F (IV)", "G (V)", "Am (vi)", "Dm7 (ii7)", "G7 (V7)"], "{r}");
    }

    #[test]
    fn arranging_and_settings() {
        let mut app = demo();
        run(&mut app, "copy_bars", json!({"from_bar": 1, "bars": 4, "to_bar": 5}));
        assert_eq!(app.project.end_tick(), 8 * 3840);
        run(&mut app, "set_sections", json!({"sections": [{"name": "A", "from_bar": 1, "bars": 4}, {"name": "A'", "from_bar": 5, "bars": 4}]}));
        let r = run(&mut app, "add_track", json!({"name": "Strings", "instrument": "string ensemble"}));
        assert!(r.contains("String Ensemble 1"), "{r}");
        run(&mut app, "set_track", json!({"track": "Strings", "volume": 70, "mute": true}));
        let r = run(&mut app, "set_project", json!({"bpm": 92, "key": "Am", "meter": "4/4"}));
        assert!(r.contains("92 BPM") && r.contains("Am"), "{r}");
        let t = run(&mut app, "transform", json!({"track": "Keys", "from_bar": 5, "to_bar": 8, "transpose": -12}));
        assert!(t.contains("transposed -12"), "{t}");
    }

    #[test]
    fn helpful_errors() {
        let mut app = demo();
        let e = call(&mut app, "get_notes", &json!({"track": "Trumpet"})).unwrap_err();
        assert!(e.contains("\"Keys\""), "{e}");
        let e = call(&mut app, "set_chord", &json!({"figure": 0, "chord": "Xm"})).unwrap_err();
        assert!(e.contains("note name"), "{e}");
    }
}
