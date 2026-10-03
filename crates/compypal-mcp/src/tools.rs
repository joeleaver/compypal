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
use compypal_core::jam::{self, JamSummary, TempoGuess};
use compypal_core::{Id, KeySignature, MeterChange, Note, PPQ, Project, RawEvent, Session, Tick, gm};
use serde_json::{Value, json};

/// What the user is pointing at in the UI: a track, a figure on a track,
/// or a range of bars across every track (possibly a named section).
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// `None` when the selection spans all tracks.
    pub track: Option<Id>,
    pub start: Tick,
    /// `Tick::MAX` for "to the end".
    pub end: Tick,
    pub figure: Option<usize>,
    pub section: Option<String>,
    /// Particular notes picked out in the piano roll; empty for a range.
    pub notes: Vec<Note>,
}

impl Selection {
    /// Says what's selected in a phrase: `bars 5-8 (section "Chorus")`.
    pub fn describe(&self, p: &Project) -> String {
        let end = if self.end == Tick::MAX { p.end_tick().max(self.start + 1) } else { self.end };
        let bars = {
            let (a, b) = (bar_of(p, self.start), bar_of(p, end.saturating_sub(1).max(self.start)));
            if a == b { format!("bar {a}") } else { format!("bars {a}-{b}") }
        };
        let track = self.track.and_then(|t| p.track(t)).map(|t| t.name.clone());
        if !self.notes.is_empty() {
            return format!("{} selected note(s) on track {:?} ({bars})", self.notes.len(), track.unwrap_or_default());
        }
        match (&track, self.figure, &self.section) {
            (Some(t), Some(f), _) => format!("figure {f} of track {t:?} ({bars})"),
            (Some(t), None, _) if self.end == Tick::MAX => format!("the whole of track {t:?}"),
            (Some(t), None, _) => format!("track {t:?}, {bars}"),
            (None, _, Some(name)) => format!("section {name:?}, {bars}, all tracks"),
            (None, _, None) => format!("{bars}, all tracks"),
        }
    }
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
    /// Jams from the always-on journal, newest first; `true` marks the one
    /// still being played.
    fn jams(&self) -> Vec<(JamSummary, bool)>;
    /// Journal events between two unix times.
    fn jam_events(&self, from: f64, to: f64) -> Vec<RawEvent>;
    /// Plays raw events as played, on a piano.
    fn play_raw(&mut self, events: &[RawEvent]) -> Result<(), String>;
    /// Unix seconds now.
    fn now(&self) -> f64;
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
    let section = json!({"type": "string", "description": "Act on this named section instead of giving bars."});
    let selection = json!({"type": "boolean", "description": "Act on whatever the user has selected in the app instead of giving bars."});
    let jam_ref = json!({"type": ["string", "integer"], "description": "A jam id from list_jams, or \"latest\" (the default)."});
    let secs = |what: &str| json!({"type": "number", "description": what});
    json!([
        {
            "name": "list_jams",
            "description": "Jams from the always-on journal: while listening is on, everything the user plays is logged and cut into jams at silences. Newest first, one line each: id, when, length, notes, tempo guess, key, the chords it moved through. Use this when the user says they played something earlier.",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "Default 10."},
                "since_hours": {"type": "number", "description": "Only jams from the last this many hours."}
            }}
        },
        {
            "name": "get_jam",
            "description": "One jam in detail, to find the part the user means: tempo guess, key, a timeline of chords (mm:ss), activity per 10 seconds (busy or loud stretches are often the idea), and the notes. Give from/to (seconds into the jam) to zoom in.",
            "inputSchema": {"type": "object", "properties": {
                "jam": jam_ref, "from": secs("Seconds into the jam."), "to": secs("Seconds into the jam.")
            }}
        },
        {
            "name": "audition_jam",
            "description": "Plays a stretch of a jam to the user exactly as it was played, to check it's the right bit.",
            "inputSchema": {"type": "object", "properties": {
                "jam": jam_ref, "from": secs("Seconds into the jam."), "to": secs("Seconds into the jam.")
            }}
        },
        {
            "name": "keep_jam",
            "description": "Brings a jam, or seconds from..to of it, into the song as a recorded session and a clip, on the beat grid its tempo implies. Then get_session, clean_take and get_figures work on it like any take. If the song is empty, it adopts the jam's tempo.",
            "inputSchema": {"type": "object", "properties": {
                "jam": jam_ref, "from": secs("Seconds into the jam."), "to": secs("Seconds into the jam."),
                "bpm": {"type": "number", "description": "Override the tempo guess."},
                "track": {"type": ["string", "integer"], "description": "An existing track, or a name for a new one (default: a new track named after the jam)."},
                "instrument": {"type": ["string", "integer"], "description": "For a new track: a General MIDI name or number."},
                "at_bar": {"type": "integer", "description": "Where it starts. Default: the bar after the song ends."}
            }}
        },
        {
            "name": "get_selection",
            "description": "What the user has selected in the app (a figure, a track, or bars across all tracks, possibly a named section), with the chords and notes in it. When the user says \"this\", \"here\" or \"the selection\", start here.",
            "inputSchema": {"type": "object", "properties": {}}
        },
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
                "section": section, "selection": selection,
                "format": {"type": "string", "enum": ["list", "abc"], "description": "Default list."}
            }}
        },
        {
            "name": "get_figures",
            "description": "A track divided into figures: the musical units you'd talk about (a C arpeggio, a run into G, stabs on Am). Each line: index, position, chord (Roman numeral), shape, note count, confidence, other readings. Figure indexes are what set_chord and continue_with take.",
            "inputSchema": {"type": "object", "properties": {"track": track, "section": section, "selection": selection}}
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
                "section": section, "selection": selection,
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
            "name": "get_harmony",
            "description": "The song's chords, read from every pitched track together (bass and comping as one): index, position, chord (Roman numeral), confidence. These indexes are what set_harmony takes. Limit with section, selection or bars.",
            "inputSchema": {"type": "object", "properties": {
                "from_bar": bar("First bar."), "to_bar": bar("Last bar."), "section": section, "selection": selection
            }}
        },
        {
            "name": "set_harmony",
            "description": "Changes chords for the whole band: every pitched track's notes in the span move to the new chord, each part keeping its shape (the bass stays a bass line, the arpeggio stays an arpeggio). Give one chord with a harmony index, bars, a section or the selection; or give chords: a list applied in order to the harmony figures in the range, e.g. section \"Chorus\" with [\"vi\", \"IV\", \"I\", \"V\"].",
            "inputSchema": {"type": "object", "properties": {
                "chord": {"type": "string"},
                "chords": {"type": "array", "items": {"type": "string"}},
                "index": {"type": "integer", "description": "A harmony index from get_harmony."},
                "from_bar": bar("First bar."), "to_bar": bar("Last bar."), "section": section, "selection": selection
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
            , "section": section, "selection": selection}, "required": ["to_bar"]}
        },
        {
            "name": "insert_bars",
            "description": "Arranging: opens empty bars before bar `at`, moving everything after (notes on every track, sections, tempo and meter changes) later.",
            "inputSchema": {"type": "object", "properties": {
                "at": bar("The bar the gap opens before."), "bars": {"type": "integer", "minimum": 1}
            , "section": section, "selection": selection}, "required": []}
        },
        {
            "name": "delete_bars",
            "description": "Arranging: removes bars from_bar..from_bar+bars-1 from the whole song, closing the gap. Notes held into the cut are shortened.",
            "inputSchema": {"type": "object", "properties": {
                "from_bar": bar("First bar to remove."), "bars": {"type": "integer", "minimum": 1}
            , "section": section, "selection": selection}, "required": []}
        },
        {
            "name": "duplicate_bars",
            "description": "Arranging: plays bars from_bar..from_bar+bars-1 twice in a row, on every track, pushing the rest of the song later. Sections inside the range are duplicated too. Use for repeating a verse or chorus.",
            "inputSchema": {"type": "object", "properties": {
                "from_bar": bar("First bar of the passage."), "bars": {"type": "integer", "minimum": 1}
            , "section": section, "selection": selection}, "required": []}
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
            "inputSchema": {"type": "object", "properties": {"track": track, "from_bar": bar("First bar."), "to_bar": bar("Last bar."), "section": section, "selection": selection}}
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
        "get_selection" => get_selection(app),
        "list_jams" => list_jams(app, args),
        "get_jam" => get_jam(app, args),
        "audition_jam" => audition_jam(app, args),
        "keep_jam" => keep_jam(app, args),
        "clean_take" => clean_take(app, args),
        "set_notes" => set_notes(app, args),
        "transform" => transform(app, args),
        "set_chord" => set_chord(app, args),
        "get_harmony" => get_harmony(app, args),
        "set_harmony" => set_harmony(app, args),
        "continue_with" => continue_with(app, args),
        "suggest_chords" => suggest_chords(app, args),
        "analyze_key" => analyze_key(app, args),
        "set_project" => set_project(app, args),
        "add_track" => add_track(app, args),
        "set_track" => set_track(app, args),
        "copy_bars" => copy_bars(app, args),
        "set_sections" => set_sections(app, args),
        "insert_bars" | "delete_bars" | "duplicate_bars" => edit_time(app, name, args),
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
            .and_then(|s| s.track)
            .or_else(|| p.tracks.first().map(|t| t.id))
            .ok_or_else(|| "the project has no tracks".to_string()),
    }
}

/// The range a tool should act on, as ticks: a named `section`, the UI
/// `selection`, or bars `from_bar..=to_bar` (open-ended when omitted).
fn range_arg(app: &dyn App, p: &Project, args: &Value) -> Result<(Tick, Tick), String> {
    if let Some(name) = opt_str(args, "section") {
        let s = find_section(p, name)?;
        return Ok((s.start, s.start + s.length));
    }
    if args.get("selection").and_then(Value::as_bool) == Some(true) {
        let sel = app.selection().ok_or("nothing is selected in the UI")?;
        return Ok((sel.start, sel.end));
    }
    let from = opt_u32(args, "from_bar").map_or(0, |b| bar_start(p, b));
    let to = opt_u32(args, "to_bar").map_or(Tick::MAX, |b| bar_start(p, b + 1));
    Ok((from, to))
}

/// Like [`range_arg`], as first bar and bar count, for whole-bar edits.
/// Falls back to `first_key` and `bars`.
fn bars_arg(app: &dyn App, p: &Project, args: &Value, first_key: &str) -> Result<(u32, u32), String> {
    if opt_str(args, "section").is_some() || args.get("selection").and_then(Value::as_bool) == Some(true) {
        let (from, to) = range_arg(app, p, args)?;
        let to = if to == Tick::MAX { p.end_tick().max(from + 1) } else { to };
        let (a, b) = (bar_of(p, from), bar_of(p, to.saturating_sub(1).max(from)));
        return Ok((a, b - a + 1));
    }
    let first = opt_u32(args, first_key).ok_or_else(|| format!("{first_key} (or section, or selection) is required"))?;
    let bars = opt_u32(args, "bars").ok_or("bars is required")?.max(1);
    Ok((first, bars))
}

fn find_section<'a>(p: &'a Project, name: &str) -> Result<&'a compypal_core::Section, String> {
    let want = name.trim().to_lowercase();
    p.sections
        .iter()
        .find(|s| s.name.to_lowercase() == want)
        .or_else(|| p.sections.iter().find(|s| s.name.to_lowercase().contains(&want)))
        .ok_or_else(|| {
            let names: Vec<String> = p.sections.iter().map(|s| format!("{:?}", s.name)).collect();
            format!("no section {name:?}; sections are {}", if names.is_empty() { "none".into() } else { names.join(", ") })
        })
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
        out.push_str(&format!(
            "\nSelected in the UI: {}. Pass selection: true to a tool to act on it.\n",
            sel.describe(&p)
        ));
    }
    Ok(out)
}

fn get_notes(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    if opt_str(args, "format") == Some("abc") {
        return Ok(compypal_io::abc::export(&p, &Default::default()));
    }
    let track = track_arg(app, &p, args)?;
    let notes = target_notes(app, &p, track, args)?;
    if notes.is_empty() {
        return Ok("No notes in that range.".into());
    }
    Ok(format!("{} notes:\n{}", notes.len(), format_notes(&p, &notes)))
}

fn get_figures(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let (from, to) = range_arg(app, &p, args)?;
    let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
    let figs = figure::analyze(&notes, &p.meter_at(0), &FigureSettings::default());
    if figs.is_empty() {
        return Ok("The track is empty.".into());
    }
    // Indexes stay those of the whole track, so set_chord can use them.
    let described: String = figure::describe(&figs, p.key, &p.meter_at(0))
        .lines()
        .zip(&figs)
        .filter(|(_, f)| f.start >= from && f.start < to)
        .map(|(l, _)| format!("{l}\n"))
        .collect();
    Ok(format!("index  start  chord (numeral in {})  shape  notes  confidence  [alternatives]\n{described}", p.key.name()))
}

// --- jams ------------------------------------------------------------------

/// "14:22" today, "Tue 14:22" this week, else "Oct 3 14:22"; local time.
pub fn when(t: f64, now: f64) -> String {
    let Ok(ts) = jiff::Timestamp::from_second(t as i64) else { return format!("{t:.0}") };
    let zone = jiff::tz::TimeZone::system();
    let at = ts.to_zoned(zone.clone());
    let today = jiff::Timestamp::from_second(now as i64).map(|n| n.to_zoned(zone).date()).ok();
    let ago = now - t;
    let rel = if ago < 3600.0 {
        format!(" ({} min ago)", (ago / 60.0).round() as i64)
    } else if ago < 86_400.0 {
        format!(" ({:.0} h ago)", ago / 3600.0)
    } else {
        String::new()
    };
    let clock = at.strftime("%H:%M").to_string();
    if Some(at.date()) == today {
        format!("{clock}{rel}")
    } else if ago < 6.0 * 86_400.0 {
        format!("{} {clock}", at.strftime("%a"))
    } else {
        format!("{} {clock}", at.strftime("%b %-d"))
    }
}

pub fn mmss(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn jam_line(j: &JamSummary, live: bool, now: f64) -> String {
    let tempo = match j.tempo {
        Some(t) if t.confidence >= 0.35 => format!("~{:.0} BPM", t.bpm),
        Some(_) => "free time".into(),
        None => "too short to time".into(),
    };
    format!(
        "{}  {}  {}  {} notes  {}  {}{}{}",
        j.id,
        if live { "now playing".to_string() } else { when(j.start, now) },
        mmss(j.duration()),
        j.notes,
        tempo,
        j.key.as_deref().unwrap_or("?"),
        if j.chords.is_empty() { String::new() } else { format!("  {}", j.chords.iter().take(12).cloned().collect::<Vec<_>>().join(" ")) },
        if j.sustain_pedal { "  (pedal)" } else { "" }
    )
}

fn find_jam(app: &dyn App, v: Option<&Value>) -> Result<(JamSummary, bool), String> {
    let jams = app.jams();
    if jams.is_empty() {
        return Err("no jams logged yet; listening has to be on while the user plays".into());
    }
    match v {
        None | Some(Value::Null) => Ok(jams[0].clone()),
        Some(Value::String(s)) if s == "latest" => Ok(jams[0].clone()),
        Some(v) => {
            let id = v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or("jam must be an id or \"latest\"")?;
            jams.into_iter().find(|(j, _)| j.id == id).ok_or_else(|| format!("no jam {id}; see list_jams"))
        }
    }
}

/// The jam's events, optionally narrowed to `from..to` seconds into it.
fn jam_slice(app: &dyn App, j: &JamSummary, args: &Value) -> (Vec<RawEvent>, f64, f64) {
    let from = args.get("from").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
    let to = args.get("to").and_then(Value::as_f64).unwrap_or(f64::INFINITY);
    let (a, b) = (j.start + from, (j.start + to).min(j.end + 1.0));
    (app.jam_events(a - 0.01, b), a, b)
}

fn list_jams(app: &mut dyn App, args: &Value) -> ToolResult {
    let now = app.now();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let since = args.get("since_hours").and_then(Value::as_f64).map(|h| now - h * 3600.0);
    let jams: Vec<String> = app
        .jams()
        .iter()
        .filter(|(j, _)| since.is_none_or(|s| j.end >= s))
        .take(limit)
        .map(|(j, live)| jam_line(j, *live, now))
        .collect();
    if jams.is_empty() {
        return Ok("No jams yet. Listening must be on (the Listen button) while the user plays.".into());
    }
    Ok(format!("id  when  length  notes  tempo  key  chords\n{}", jams.join("\n")))
}

fn get_jam(app: &mut dyn App, args: &Value) -> ToolResult {
    let (j, live) = find_jam(app, args.get("jam"))?;
    let (events, from, to) = jam_slice(app, &j, args);
    if events.is_empty() {
        return Err("no notes there".into());
    }
    let ons = jam::onsets(&events);
    let tempo = jam::estimate_tempo(&ons).or(j.tempo);
    let mut out = format!("{}\n", jam_line(&j, live, app.now()));
    if from > j.start + 0.5 || to < j.end {
        out.push_str(&format!("Showing {}–{} of it.\n", mmss(from - j.start), mmss(to.min(j.end) - j.start)));
    }
    if let Some(t) = tempo {
        out.push_str(&format!("Tempo here: {:.1} BPM, confidence {:.2}{}\n", t.bpm, t.confidence, if t.confidence < 0.35 { " (free time)" } else { "" }));
    }
    out.push_str("\nActivity per 10s (start, notes, mean velocity):\n");
    for (t, n, v) in jam::activity(&events, 10.0) {
        let bar = "#".repeat(n.min(60) / 2);
        out.push_str(&format!("  {}  {:>3}  {:>3}  {bar}\n", mmss(from - j.start + t), n, v));
    }
    if let Some(t) = tempo.filter(|t| t.confidence >= 0.3) {
        let notes = jam::jam_notes(&events, Some(&t));
        let figs = figure::analyze(&notes, &MeterChange::default_four(), &FigureSettings::default());
        let beat = 60.0 / t.bpm;
        out.push_str("\nChords (time into the jam):\n");
        let mut line = Vec::new();
        for f in figs {
            let secs = from - j.start + (t.downbeat - from).max(-beat) + f.start as f64 / PPQ as f64 * beat;
            line.push(format!("{} {}", mmss(secs.max(0.0)), f.chord.name(Spelling::Mixed)));
        }
        out.push_str(&format!("  {}\n", line.join(", ")));
    }
    let shown = events.iter().filter(|e| matches!(e.msg, compypal_core::RawMsg::NoteOn { velocity, .. } if velocity > 0)).count();
    if shown <= 160 {
        out.push_str("\nNotes (seconds into the jam):\n");
        for e in &events {
            if let compypal_core::RawMsg::NoteOn { pitch, velocity } = e.msg
                && velocity > 0
            {
                out.push_str(&format!("  {:.2}  {}  vel {velocity}\n", e.t - j.start, gm::pitch_name(pitch)));
            }
        }
    } else {
        out.push_str(&format!("\n{shown} notes here; give from/to to see them.\n"));
    }
    Ok(out)
}

fn audition_jam(app: &mut dyn App, args: &Value) -> ToolResult {
    let (j, _) = find_jam(app, args.get("jam"))?;
    let (events, from, to) = jam_slice(app, &j, args);
    if events.is_empty() {
        return Err("no notes there".into());
    }
    app.play_raw(&events)?;
    Ok(format!("Playing {}–{} of jam {}.", mmss(from - j.start), mmss(to.min(j.end) - j.start), j.id))
}

fn keep_jam(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let (j, _) = find_jam(app, args.get("jam"))?;
    let (events, from, to) = jam_slice(app, &j, args);
    let ons = jam::onsets(&events);
    if ons.len() < 2 {
        return Err("no notes there to keep".into());
    }
    // The beat of this stretch, which may differ from the whole jam's.
    let tempo: Option<TempoGuess> = match args.get("bpm").and_then(Value::as_f64) {
        Some(bpm) => jam::fit_phase(&ons, bpm),
        None => jam::estimate_tempo(&ons).or(j.tempo).filter(|t| t.confidence >= 0.3),
    };
    let label = format!("Jam {}", when(j.start, app.now()).split(' ').next().unwrap_or(""));
    let empty = p.tracks.iter().all(|t| t.clips.iter().all(|c| c.notes.is_empty()));
    let at_bar = match args.get("at_bar").and_then(Value::as_u64) {
        Some(b) => b as u32,
        None if empty => 1,
        None => bar_of(&p, p.end_tick().saturating_sub(1)) + 1,
    };
    let existing = match args.get("track") {
        Some(v) if !v.is_null() => find_track(&p, v).ok(),
        _ => None,
    };
    let new_name = args.get("track").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| label.clone());
    let program = args.get("instrument").map(instrument_arg).transpose()?.unwrap_or(0);
    let mut placed = (0, 0, 0usize, String::new());
    app.edit(&format!("keep {label}"), &mut |p| {
        if let (Some(t), true) = (tempo, empty) {
            p.tempo = compypal_core::TempoMap::constant(t.bpm.round());
        }
        let track = existing.unwrap_or_else(|| p.add_track(new_name.clone(), program));
        let start_tick = bar_start(p, at_bar);
        let id = p.alloc_id();
        let mut session = jam::to_session(&events, from, to, id, label.clone(), tempo.as_ref(), p.meter_at(start_tick), start_tick);
        session.recorded_at = j.start as u64;
        let imported = cleanup::import_session(&session, &p.tempo, true);
        let bar = p.meter_at(start_tick).ticks_per_bar();
        let clip_start = start_tick.saturating_sub(imported.pickup_bars as Tick * bar);
        let end = imported.notes.iter().map(|n| n.end()).max().unwrap_or(0);
        let clip_id = p.alloc_id();
        placed = (
            bar_of(p, clip_start),
            bar_of(p, (clip_start + end).saturating_sub(1)),
            imported.notes.len(),
            p.track(track).map(|t| t.name.clone()).unwrap_or_default(),
        );
        let t = p.track_mut(track).ok_or("no such track")?;
        t.clips.push(compypal_core::Clip {
            id: clip_id,
            name: label.clone(),
            start: clip_start,
            length: end.div_ceil(bar).max(1) * bar,
            notes: imported.notes,
            source_session: Some(id),
        });
        t.clips.sort_by_key(|c| c.start);
        p.sessions.push(session);
        Ok(())
    })?;
    let (first, last, notes, track) = placed;
    Ok(format!(
        "Kept {}–{} of jam {} as session {label:?}: {notes} notes on track {track:?}, bars {first}-{last}, {}. It's as played; get_session and clean_take work on it now.",
        mmss(from - j.start),
        mmss(to.min(j.end) - j.start),
        j.id,
        match tempo {
            Some(t) => format!("at {:.1} BPM (confidence {:.2})", t.bpm, t.confidence),
            None => "in free time (no steady beat found), at the song's tempo".into(),
        }
    ))
}

fn get_selection(app: &mut dyn App) -> ToolResult {
    let p = app.project();
    let sel = app.selection().ok_or("nothing is selected in the UI")?;
    Ok(selection_text(&p, &sel))
}

/// A selection spelled out: what it is, then per track the figures (with
/// their indexes, for set_chord) and, when there aren't too many, the notes.
pub fn selection_text(p: &Project, sel: &Selection) -> String {
    let end = if sel.end == Tick::MAX { p.end_tick() } else { sel.end };
    let mut out = format!("Selected: {}.\n", sel.describe(p));
    let tracks: Vec<&compypal_core::Track> = match sel.track {
        Some(t) => p.track(t).into_iter().collect(),
        None => p.tracks.iter().collect(),
    };
    for t in tracks {
        let all = t.absolute_notes();
        let notes: Vec<Note> = if sel.notes.is_empty() {
            all.iter().copied().filter(|n| n.start >= sel.start && n.start < end).collect()
        } else {
            sel.notes.iter().copied().filter(|n| all.contains(n)).collect()
        };
        out.push_str(&format!("\n{:?}: {} notes", t.name, notes.len()));
        if notes.is_empty() {
            out.push('\n');
            continue;
        }
        if !t.is_drums() {
            let figs = figure::analyze(&all, &p.meter_at(0), &FigureSettings::default());
            let chords: Vec<String> = figs
                .iter()
                .enumerate()
                .filter(|(_, f)| f.start + PPQ as Tick / 8 >= sel.start && f.start < end)
                .map(|(i, f)| format!("[{i}] {} {}", f.chord.name(spell(p)), f.kind.label()))
                .collect();
            out.push_str(&format!(", figures: {}", chords.join(", ")));
        }
        out.push('\n');
        if notes.len() <= 64 {
            out.push_str(&format_notes(p, &notes));
            out.push('\n');
        }
    }
    out
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

/// The notes a tool should touch: the user's picked notes when the call
/// says `selection` and notes are picked, else every note in the range.
fn target_notes(app: &dyn App, p: &Project, track: Id, args: &Value) -> Result<Vec<Note>, String> {
    if args.get("selection").and_then(Value::as_bool) == Some(true)
        && let Some(sel) = app.selection()
        && !sel.notes.is_empty()
    {
        let all = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
        return Ok(sel.notes.into_iter().filter(|n| all.contains(n)).collect());
    }
    let (from, to) = range_arg(app, p, args)?;
    Ok(notes_in(p, track, from, to))
}

fn transform(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let track = track_arg(app, &p, args)?;
    let old = target_notes(app, &p, track, args)?;
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

fn harmony_text(p: &Project, figs: &[figure::Figure], from: Tick, to: Tick) -> String {
    let meter = p.meter_at(0);
    figs.iter()
        .enumerate()
        .filter(|(_, f)| f.start + PPQ as Tick / 8 >= from && f.start < to)
        .map(|(i, f)| {
            format!(
                "{i}  {}  {} ({})  {:.2}",
                compypal_core::cleanup::bar_beat_tick(f.start, &meter),
                f.chord.name(spell(p)),
                f.chord.roman(p.key),
                f.confidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn get_harmony(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let (from, to) = range_arg(app, &p, args)?;
    let figs = figure::harmony(&p, &FigureSettings::default());
    if figs.is_empty() {
        return Ok("No pitched notes yet.".into());
    }
    Ok(format!("index  start  chord (numeral in {})  confidence\n{}", p.key.name(), harmony_text(&p, &figs, from, to)))
}

fn set_harmony(app: &mut dyn App, args: &Value) -> ToolResult {
    let p = app.project();
    let figs = figure::harmony(&p, &FigureSettings::default());
    // Each harmony figure spans from its start to the next one's.
    let span = |i: usize| (figs[i].start, figs.get(i + 1).map_or(figs[i].end, |n| n.start));
    let mut targets: Vec<(Tick, Tick, Chord)> = Vec::new();
    if let Some(list) = args.get("chords").and_then(Value::as_array) {
        let (from, to) = range_arg(app, &p, args)?;
        let in_range: Vec<usize> =
            (0..figs.len()).filter(|&i| figs[i].start + PPQ as Tick / 8 >= from && figs[i].start < to).collect();
        if in_range.len() != list.len() {
            return Err(format!(
                "that range has {} chords and you gave {}; the chords there are:\n{}",
                in_range.len(),
                list.len(),
                harmony_text(&p, &figs, from, to)
            ));
        }
        for (i, c) in in_range.into_iter().zip(list) {
            let (a, b) = span(i);
            targets.push((a, b, chord_arg(&p, c.as_str().unwrap_or_default())?));
        }
    } else {
        let chord = chord_arg(&p, opt_str(args, "chord").ok_or("give chord, or chords")?)?;
        let (a, b) = match args.get("index").and_then(Value::as_u64) {
            Some(i) if (i as usize) < figs.len() => span(i as usize),
            Some(i) => return Err(format!("no harmony figure {i}; there are {}", figs.len())),
            None => {
                let (a, b) = range_arg(app, &p, args)?;
                if b == Tick::MAX {
                    return Err("say where: index, from_bar/to_bar, section or selection".into());
                }
                (a, b)
            }
        };
        targets.push((a, b, chord));
    }
    let names: Vec<String> = targets.iter().map(|t| t.2.name(spell(&p))).collect();
    app.edit(&format!("harmony {}", names.join(" ")), &mut |p| {
        for (a, b, c) in &targets {
            figure::set_harmony(p, *a, *b, c);
        }
        Ok(())
    })?;
    let p = app.project();
    let figs = figure::harmony(&p, &FigureSettings::default());
    let (from, to) = (targets.first().unwrap().0, targets.last().unwrap().1);
    Ok(format!("Harmony there is now:\n{}", harmony_text(&p, &figs, from, to)))
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
    let (from_bar, bars) = bars_arg(app, &p, args, "from_bar")?;
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

fn edit_time(app: &mut dyn App, name: &str, args: &Value) -> ToolResult {
    use compypal_core::arrange;
    let p = app.project();
    let (first, bars) = bars_arg(app, &p, args, if name == "insert_bars" { "at" } else { "from_bar" })?;
    let (from, to) = (bar_start(&p, first), bar_start(&p, first + bars));
    let last = first + bars - 1;
    let (label, done) = match name {
        "insert_bars" => (format!("insert {bars} bars"), format!("Opened {bars} empty bar(s) at bar {first}.")),
        "delete_bars" => (format!("delete bars {first}-{last}"), format!("Removed bars {first}-{last}.")),
        _ => (format!("duplicate bars {first}-{last}"), format!("Bars {first}-{last} now repeat as bars {}-{}.", last + 1, last + bars)),
    };
    app.edit(&label, &mut |p| {
        match name {
            "insert_bars" => arrange::insert_time(p, from, to - from),
            "delete_bars" => arrange::delete_time(p, from, to - from),
            _ => arrange::duplicate_time(p, from, to - from),
        }
        Ok(())
    })?;
    let p = app.project();
    Ok(format!("{done} The song is now {} bars.", bar_of(&p, p.end_tick().saturating_sub(1))))
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
    let (from, to) = range_arg(app, &p, args)?;
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
    /// A journal: every event, unix-timed, and the jams found in it.
    pub journal: Vec<RawEvent>,
    pub clock: f64,
}

impl MemoryApp {
    pub fn new(project: Project) -> Self {
        Self { project, history: Default::default(), selection: None, journal: Vec::new(), clock: 0.0 }
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
    fn jams(&self) -> Vec<(JamSummary, bool)> {
        // Split at gaps, as the real journal does.
        let mut out = Vec::new();
        let mut cur: Vec<RawEvent> = Vec::new();
        for e in &self.journal {
            if cur.last().is_some_and(|l| e.t - l.t > jam::GAP_SECONDS) {
                out.extend(jam::summarize(&cur));
                cur.clear();
            }
            cur.push(*e);
        }
        out.extend(jam::summarize(&cur));
        out.into_iter().rev().map(|j| (j, false)).collect()
    }
    fn jam_events(&self, from: f64, to: f64) -> Vec<RawEvent> {
        self.journal.iter().copied().filter(|e| e.t >= from && e.t <= to).collect()
    }
    fn play_raw(&mut self, _: &[RawEvent]) -> Result<(), String> {
        Ok(())
    }
    fn now(&self) -> f64 {
        self.clock
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
    fn editing_time() {
        let mut app = demo();
        let r = run(&mut app, "duplicate_bars", json!({"from_bar": 1, "bars": 4}));
        assert!(r.contains("now 8 bars"), "{r}");
        run(&mut app, "insert_bars", json!({"at": 5, "bars": 2}));
        let r = run(&mut app, "delete_bars", json!({"from_bar": 5, "bars": 2}));
        assert!(r.contains("now 8 bars"), "{r}");
    }

    #[test]
    fn sections_and_selection_as_references() {
        let mut app = demo();
        run(&mut app, "duplicate_bars", json!({"section": "intro"}));
        run(&mut app, "set_sections", json!({"sections": [{"name": "Verse", "from_bar": 1, "bars": 4}, {"name": "Chorus", "from_bar": 5, "bars": 4}]}));
        let n = run(&mut app, "get_notes", json!({"track": "Bass", "section": "chorus"}));
        assert!(n.starts_with("12 notes") && n.contains("5.1.0"), "{n}");
        run(&mut app, "transform", json!({"track": "Keys", "section": "Chorus", "transpose": 12}));
        app.selection = Some(Selection { track: None, start: 4 * 3840, end: 6 * 3840, figure: None, section: None, notes: vec![] });
        let s = run(&mut app, "get_selection", json!({}));
        assert!(s.contains("bars 5-6, all tracks") && s.contains("\"Keys\": 16 notes, figures: [4] C"), "{s}");
        let r = run(&mut app, "delete_bars", json!({"selection": true}));
        assert!(r.contains("Removed bars 5-6"), "{r}");
        let e = call(&mut app, "get_notes", &json!({"section": "Bridge"})).unwrap_err();
        assert!(e.contains("\"Verse\", \"Chorus\""), "{e}");
    }

    #[test]
    fn band_wide_harmony() {
        let mut app = demo();
        let h = run(&mut app, "get_harmony", json!({}));
        assert!(h.contains("C (I)") && h.contains("G (V)"), "{h}");
        let r = run(&mut app, "set_harmony", json!({"section": "Intro", "chords": ["vi", "IV", "I", "V"]}));
        let chords: Vec<&str> = r.lines().skip(1).map(|l| l.split("  ").nth(2).unwrap()).collect();
        assert_eq!(chords, ["Am (vi)", "F (IV)", "C (I)", "G (V)"], "{r}");
        let e = call(&mut app, "set_harmony", &json!({"section": "Intro", "chords": ["I"]})).unwrap_err();
        assert!(e.contains("has 4 chords and you gave 1"), "{e}");
    }

    /// A morning of noodling: a short free-time doodle, then a proper jam
    /// at 92 BPM, Am F C G arpeggios, with a stray touch between.
    fn journal() -> Vec<RawEvent> {
        use compypal_core::RawMsg::{NoteOff, NoteOn};
        let mut ev = Vec::new();
        let mut note = |t: f64, pitch: u8, velocity: u8, len: f64| {
            ev.push(RawEvent { t, channel: 0, msg: NoteOn { pitch, velocity } });
            ev.push(RawEvent { t: t + len, channel: 0, msg: NoteOff { pitch } });
        };
        let t0 = 1_800_000_000.0;
        for (i, p) in [60u8, 67, 64, 72, 65, 62, 71, 59, 69].iter().enumerate() {
            note(t0 + i as f64 * 0.43 + (i % 3) as f64 * 0.17, *p, 70, 0.3);
        }
        note(t0 + 30.0, 40, 50, 0.1);
        let start = t0 + 60.0;
        let eighth = 30.0 / 92.0;
        let chords = [[57u8, 60, 64, 69], [53, 57, 60, 65], [48, 52, 55, 60], [55, 59, 62, 67]];
        for i in 0..64 {
            let jit = ((i * 37 % 11) as f64 - 5.0) * 0.003;
            let p = chords[(i / 8) % 4][[0, 1, 2, 3, 2, 1, 2, 1][i % 8]];
            note(start + i as f64 * eighth + jit, p, if i % 2 == 0 { 96 } else { 74 }, eighth * 0.8);
        }
        ev.sort_by(|a, b| a.t.total_cmp(&b.t));
        ev
    }

    #[test]
    fn finding_and_keeping_a_jam() {
        let mut app = MemoryApp::new(Project::new("Empty"));
        app.journal = journal();
        app.clock = 1_800_000_000.0 + 3600.0;
        let list = run(&mut app, "list_jams", json!({}));
        let lines: Vec<&str> = list.lines().skip(1).collect();
        assert_eq!(lines.len(), 2, "doodle and jam; the stray note isn't one: {list}");
        assert!(lines[0].contains("~92 BPM") && lines[0].contains("Am F C G"), "{list}");
        let detail = run(&mut app, "get_jam", json!({"jam": "latest"}));
        assert!(detail.contains("Chords (time into the jam)") && detail.contains("0:00 Am"), "{detail}");
        let kept = run(&mut app, "keep_jam", json!({"from": 0, "to": 10.3}));
        assert!(kept.contains("bars 1-4"), "{kept}");
        // The empty song took the jam's tempo, and the take reads as the jam.
        assert_eq!(app.project.tempo.bpm_at(0), 92.0);
        let figs = run(&mut app, "get_figures", json!({}));
        assert!(figs.contains("Am (vi)") && figs.contains("G (V)"), "{figs}");
        let s = run(&mut app, "get_session", json!({}));
        assert!(s.contains("32 notes"), "{s}");
    }

    #[test]
    fn picked_notes_are_the_selection() {
        let mut app = demo();
        let bass = app.project.tracks[1].clone();
        let picked: Vec<Note> = bass.absolute_notes().into_iter().take(2).collect();
        app.selection = Some(Selection { track: Some(bass.id), start: 0, end: 3840, figure: None, section: None, notes: picked.clone() });
        let s = run(&mut app, "get_selection", json!({}));
        assert!(s.contains("2 selected note(s)") && s.contains("\"Bass\": 2 notes"), "{s}");
        run(&mut app, "transform", json!({"selection": true, "transpose": 2}));
        let after = app.project.tracks[1].absolute_notes();
        assert_eq!(after.iter().filter(|n| n.pitch == 38).count(), 2, "only the two picked notes moved");
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
