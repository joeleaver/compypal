//! App-wide state. Every project edit goes through [`Store::edit`] so the
//! UI and, later, the agent share one undo history.

use std::path::PathBuf;

use compypal_core::cleanup::{self, Quantize};
use compypal_core::figure::{self, Figure, FigureSettings};
use compypal_core::theory::{self, Chord, Spelling};
use compypal_audio::input::MidiIn;
use compypal_audio::{Engine, Schedule, ScheduleOptions, schedule};
use compypal_core::{Clip, History, Id, MeterChange, Note, PPQ, Project, RawEvent, Session, Tick};
use rinch::prelude::*;

/// Where the chord editor is open: on a figure, or on the slot after the
/// last one, where typing a chord continues the music.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Figure(usize),
    Append,
}

#[derive(Clone, Copy)]
pub struct Store {
    pub project: Signal<Project>,
    pub history: Signal<History>,
    pub selected_track: Signal<Option<Id>>,
    /// Draw the raw take behind clips that came from a session.
    pub show_raw: Signal<bool>,
    /// Horizontal zoom in pixels per quarter note.
    pub zoom: Signal<f64>,
    pub status: Signal<String>,
    /// Figures on the selected track, re-derived whenever notes change.
    pub figures: Memo<Vec<Figure>>,
    pub editing: Signal<Option<Slot>>,
    /// What's typed in the chord editor.
    pub draft: Signal<String>,
    /// Which suggestion Enter would pick.
    pub highlight: Signal<usize>,
    /// Label figures with Roman numerals instead of chord symbols.
    pub roman: Signal<bool>,
    /// `None` when there is no audio device; the app still works, silently.
    pub engine: Option<&'static Engine>,
    /// Transport position in seconds while playing.
    pub playhead: Signal<Option<f64>>,
    /// Where Play starts from.
    pub cursor: Signal<Tick>,
    pub looping: Signal<bool>,
    pub metronome: Signal<bool>,
    /// What the engine is playing through.
    pub audio_status: Signal<String>,
    pub midi: Option<&'static MidiIn>,
    /// The connected input port.
    pub midi_port: Signal<Option<String>>,
    pub recording: Signal<Option<Recording>>,
    /// The running take so far, for drawing as it's played.
    pub live_take: Signal<Vec<RawEvent>>,
    /// Engine time minus song time: nonzero while recording, where the
    /// engine's clock starts at the count-in rather than at the song.
    pub play_offset: Signal<f64>,
}

/// A take in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub track: Id,
    /// Where on the timeline the take's first downbeat lands.
    pub start_tick: Tick,
    /// Count-in length in seconds: time zero of the take is the first click.
    pub lead: f64,
    pub bpm: f64,
    pub meter: MeterChange,
}

impl Recording {
    /// The take as a session, for drawing or keeping.
    pub fn session(&self, id: Id, name: String, events: Vec<RawEvent>) -> Session {
        Session {
            id,
            name,
            recorded_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            click_bpm: Some(self.bpm),
            meter: self.meter,
            downbeat_offset: self.lead,
            start_tick: self.start_tick,
            events,
        }
    }
}

impl Store {
    pub fn new(project: Project, engine: Option<&'static Engine>, midi: Option<&'static MidiIn>) -> Self {
        let first = project.tracks.first().map(|t| t.id);
        let project = Signal::new(project);
        let selected_track = Signal::new(first);
        let figures = Memo::new(move || {
            let track = selected_track.get();
            project.with(|p| {
                let Some(t) = track.and_then(|id| p.track(id)) else { return Vec::new() };
                figure::analyze(&t.absolute_notes(), &p.meter_at(0), &FigureSettings::default())
            })
        });
        Self {
            project,
            history: Signal::new(History::default()),
            selected_track,
            show_raw: Signal::new(true),
            zoom: Signal::new(96.0),
            status: Signal::new(String::new()),
            figures,
            editing: Signal::new(None),
            draft: Signal::new(String::new()),
            highlight: Signal::new(0),
            roman: Signal::new(false),
            engine,
            playhead: Signal::new(None),
            cursor: Signal::new(0),
            looping: Signal::new(true),
            metronome: Signal::new(false),
            audio_status: Signal::new(match engine {
                Some(e) => e.status(),
                None => "No audio output".into(),
            }),
            midi,
            midi_port: Signal::new(midi.and_then(|m| m.connected())),
            recording: Signal::new(None),
            live_take: Signal::new(Vec::new()),
            play_offset: Signal::new(0.0),
        }
    }

    /// Adds a piano track and selects it, ready to record into.
    pub fn add_track(self) {
        let mut id = None;
        self.edit("add track", |p| {
            let name = format!("Track {}", p.tracks.len() + 1);
            id = Some(p.add_track(name, 0));
        });
        if let Some(id) = id {
            self.select_track(id);
        }
    }

    /// Starts over with an empty project (undoable).
    pub fn new_project(self) {
        if let Some(e) = self.engine {
            e.stop();
        }
        self.playhead.set(None);
        self.edit("new project", |p| {
            *p = Project::new("Untitled");
            p.add_track("Piano", 0);
        });
        self.cursor.set(0);
        let first = self.project.with(|p| p.tracks.first().map(|t| t.id));
        if let Some(id) = first {
            self.select_track(id);
        }
    }

    /// Selects a track, and plays live input through its instrument.
    pub fn select_track(self, id: Id) {
        self.close_editor();
        self.selected_track.set(Some(id));
        self.monitor_selected();
    }

    fn monitor_selected(self) {
        let Some((ch, program, drums)) = self.selected_track.get().and_then(|id| {
            self.project.with(|p| p.track(id).map(|t| (t.channel, t.program, t.is_drums())))
        }) else {
            return;
        };
        if let Some(m) = self.midi {
            m.set_monitor_channel(ch);
        }
        if let (Some(e), false) = (self.engine, drums) {
            e.midi([0xc0 | ch, program, 0]);
        }
    }

    pub fn connect_midi(self, port: &str) {
        let Some(m) = self.midi else { return };
        if port.is_empty() {
            m.disconnect();
            self.midi_port.set(None);
            return;
        }
        match m.connect(port) {
            Ok(()) => {
                self.midi_port.set(Some(port.to_string()));
                self.monitor_selected();
                self.status.set(format!("Listening to {port}"));
            }
            Err(e) => self.status.set(e.to_string()),
        }
    }

    /// Which of `input::ports()` is connected.
    pub fn midi_port_index(self) -> Option<usize> {
        let current = self.midi_port.get()?;
        compypal_audio::input::ports().iter().position(|p| *p == current)
    }

    /// Connects port `index` of `input::ports()`, or disconnects it if it
    /// is the one connected.
    pub fn toggle_port(self, index: usize) {
        if self.midi_port_index() == Some(index) {
            self.connect_midi("");
        } else if let Some(name) = compypal_audio::input::ports().get(index) {
            self.connect_midi(name);
        }
    }

    /// Song position in seconds, from the engine's position.
    pub fn song_seconds(self) -> Option<f64> {
        self.playhead.get().map(|s| s - self.play_offset.get())
    }

    pub fn is_recording(self) -> bool {
        self.recording.with(|r| r.is_some())
    }

    /// Starts a take on the selected track from the cursor: a bar of
    /// count-in, then the song plays (with the click, if on) while
    /// everything played on the controller is captured.
    pub fn record(self) {
        let (Some(engine), Some(midi)) = (self.engine, self.midi) else {
            self.status.set("Recording needs audio output and a MIDI input".into());
            return;
        };
        if midi.connected().is_none() {
            self.status.set("Connect a MIDI input to record".into());
            return;
        }
        let Some(track) = self.selected_track.get() else { return };
        engine.stop();
        let start_tick = self.cursor.get();
        let (song, from, bpm, meter) = self.project.with(|p| {
            let until = start_tick + 256 * PPQ as Tick * 4;
            let opts = ScheduleOptions { metronome: self.metronome.get(), until };
            (schedule::build(p, opts), p.tempo.tick_to_seconds(start_tick as f64), p.tempo.bpm_at(start_tick), p.meter_at(start_tick))
        });
        let beat = 60.0 / bpm * 4.0 / meter.denominator as f64;
        let mut take = schedule::with_count_in(&song, from, meter.numerator, beat);
        // Keep going until stopped, however short the song is.
        take.length = f64::INFINITY;
        let lead = meter.numerator as f64 * beat;
        self.monitor_selected();
        midi.start_take(std::time::Instant::now());
        engine.play(take, 0.0, false);
        self.play_offset.set(lead - from);
        self.playhead.set(Some(0.0));
        self.recording.set(Some(Recording { track, start_tick, lead, bpm, meter }));
        self.status.set("Recording… (Space or Rec to stop)".into());
    }

    /// Ends the take: keeps it as a session and lays it down as a clip on
    /// the track it was recorded on, as played.
    pub fn stop_recording(self) {
        let Some(rec) = self.recording.get() else { return };
        if let Some(e) = self.engine {
            e.stop();
        }
        self.playhead.set(None);
        self.play_offset.set(0.0);
        self.recording.set(None);
        self.live_take.set(Vec::new());
        let events = self.midi.map(|m| m.finish_take()).unwrap_or_default();
        if !events.iter().any(|e| matches!(e.msg, compypal_core::RawMsg::NoteOn { .. })) {
            self.status.set("Nothing recorded".into());
            return;
        }
        let mut summary = String::new();
        self.edit("record", |p| {
            let id = p.alloc_id();
            let name = format!("Take {}", p.sessions.len() + 1);
            let session = rec.session(id, name.clone(), events);
            let imported = cleanup::import_session(&session, &p.tempo, true);
            let bar = rec.meter.ticks_per_bar();
            let start = rec.start_tick.saturating_sub(imported.pickup_bars as Tick * bar);
            let end = imported.notes.iter().map(|n| n.end()).max().unwrap_or(0);
            let clip_id = p.alloc_id();
            summary = format!("{name}: {} notes", imported.notes.len());
            if let Some(t) = p.track_mut(rec.track) {
                t.clips.push(Clip {
                    id: clip_id,
                    name: name.clone(),
                    start,
                    length: end.div_ceil(bar).max(1) * bar,
                    notes: imported.notes,
                    source_session: Some(id),
                });
                t.clips.sort_by_key(|c| c.start);
            }
            p.sessions.push(session);
        });
        self.status.set(format!("Recorded {summary}. Tidy it from the session list, or ask the agent."));
    }

    /// Re-derives every clip made from `session` with the default cleanup.
    pub fn tidy_session(self, session: Id) {
        let mut report = cleanup::TidyReport::default();
        self.edit("tidy take", |p| {
            let Some(s) = p.session(session).cloned() else { return };
            let fresh = cleanup::import_session(&s, &p.tempo, true).notes;
            for t in &mut p.tracks {
                for c in t.clips.iter_mut().filter(|c| c.source_session == Some(session)) {
                    let mut notes = fresh.clone();
                    report = cleanup::tidy(&mut notes);
                    c.notes = notes;
                }
            }
        });
        self.status.set(format!(
            "Tidied: {} ghost notes and {} slips removed, {} double strikes merged, {} notes nudged toward 1/16",
            report.ghosts_removed, report.slips_removed, report.double_strikes_merged, report.quantized
        ));
    }

    pub fn schedule(self) -> Schedule {
        let opts = ScheduleOptions { metronome: self.metronome.get(), ..Default::default() };
        self.project.with(|p| schedule::build(p, opts))
    }

    pub fn is_playing(self) -> bool {
        self.playhead.get().is_some()
    }

    pub fn toggle_play(self) {
        let Some(engine) = self.engine else { return };
        if untracked(|| self.is_recording()) {
            self.stop_recording();
            return;
        }
        self.play_offset.set(0.0);
        if untracked(|| self.playhead.get()).is_some() {
            engine.stop();
            self.playhead.set(None);
        } else {
            let from = self.project.with(|p| p.tempo.tick_to_seconds(self.cursor.get() as f64));
            engine.play(self.schedule(), from, self.looping.get());
            // Show the playhead right away rather than on the next poll.
            self.playhead.set(Some(from));
        }
    }

    /// Moves the play cursor, and the music too if it's playing.
    pub fn seek(self, tick: Tick) {
        self.cursor.set(tick);
        if let (Some(engine), true, false) =
            (self.engine, untracked(|| self.playhead.get()).is_some(), untracked(|| self.is_recording()))
        {
            let from = self.project.with(|p| p.tempo.tick_to_seconds(tick as f64));
            engine.play(self.schedule(), from, self.looping.get());
        }
    }

    pub fn toggle_looping(self) {
        self.looping.update(|l| *l = !*l);
        if let Some(e) = self.engine {
            e.set_looping(self.looping.get());
        }
    }

    /// Plays `notes` on `track`'s instrument, now.
    pub fn audition(self, track: Id, notes: &[Note]) {
        let Some(engine) = self.engine else { return };
        let sched = self.project.with(|p| {
            let t = p.track(track)?;
            Some(schedule::audition(notes, t.channel, t.program, p))
        });
        if let Some(s) = sched {
            engine.audition(s);
        }
    }

    /// Plays what the editor's slot would sound like with `chord`.
    fn preview(self, chord: &Chord) {
        let Some(track) = self.selected_track.get() else { return };
        let figures = self.figures.get();
        let notes = self.project.with(|p| match self.editing.get()? {
            Slot::Figure(i) => {
                let f = figures.get(i)?;
                Some(figure::revoice(&f.notes, &f.chord, chord, p.key))
            }
            Slot::Append if !figures.is_empty() => Some(figure::continued(&figures, figures.len() - 1, chord, p).2),
            Slot::Append => Some(
                chord.voicing(55).into_iter().map(|pitch| Note { pitch, velocity: 90, start: 0, duration: PPQ as Tick * 2 }).collect(),
            ),
        });
        if let Some(n) = notes {
            self.audition(track, &n);
        }
    }

    /// A chord as the lane shows it, in the current notation.
    pub fn chord_label(self, chord: &Chord) -> String {
        let key = self.project.with(|p| p.key);
        if self.roman.get() { chord.roman(key) } else { chord.name(Spelling::for_key(key)) }
    }

    /// The chord in whichever notation the lane is not showing.
    pub fn other_label(self, chord: &Chord) -> String {
        let key = self.project.with(|p| p.key);
        if self.roman.get() { chord.name(Spelling::for_key(key)) } else { chord.roman(key) }
    }

    pub fn open_editor(self, slot: Slot) {
        self.editing.set(Some(slot));
        self.draft.set(String::new());
        self.highlight.set(0);
        if let (Slot::Figure(i), Some(track)) = (slot, self.selected_track.get())
            && let Some(f) = self.figures.get().get(i)
        {
            self.audition(track, &f.notes);
        }
    }

    pub fn close_editor(self) {
        self.editing.set(None);
    }

    /// What the editor offers for the current draft, best first.
    pub fn suggestions(self) -> Vec<Chord> {
        let key = self.project.with(|p| p.key);
        let context: Vec<Chord> = match self.editing.get() {
            Some(Slot::Figure(i)) => self
                .figures
                .get()
                .get(i)
                .map(|f| std::iter::once(f.chord).chain(f.alternatives.iter().copied()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        theory::suggest(&self.draft.get(), key, &context, 8)
    }

    pub fn move_highlight(self, delta: i64) {
        let n = self.suggestions().len() as i64;
        if n > 0 {
            self.highlight.update(|h| *h = (*h as i64 + delta).rem_euclid(n) as usize);
            if let Some(c) = self.suggestions().get(self.highlight.get()) {
                self.preview(c);
            }
        }
    }

    /// Applies the highlighted suggestion (or the draft, if nothing is
    /// suggested). With `advance`, moves the editor to the next slot so
    /// a progression can be typed chord, Tab, chord, Tab.
    pub fn commit(self, advance: bool) {
        let Some(slot) = self.editing.get() else { return };
        let Some(track) = self.selected_track.get() else { return };
        let key = self.project.with(|p| p.key);
        let chord = self
            .suggestions()
            .get(self.highlight.get())
            .copied()
            .or_else(|| theory::parse_chord_or_roman(&self.draft.get(), key).ok());
        let figures = self.figures.get();
        let next = match slot {
            Slot::Figure(i) if i + 1 < figures.len() => Slot::Figure(i + 1),
            _ => Slot::Append,
        };
        match chord {
            Some(chord) => self.apply_chord(track, slot, &chord, &figures),
            None if !self.draft.get().trim().is_empty() => {
                self.status.set(format!("Not a chord: {}", self.draft.get()));
                return;
            }
            None => {}
        }
        if advance { self.open_editor(next) } else { self.close_editor() }
    }

    fn apply_chord(self, track: Id, slot: Slot, chord: &Chord, figures: &[Figure]) {
        let s = FigureSettings::default();
        let label = self.chord_label(chord);
        let result = match slot {
            Slot::Figure(i) if figures.get(i).is_some_and(|f| f.chord == *chord) => return,
            Slot::Figure(i) => {
                let mut r = Ok(());
                self.edit(&format!("chord {label}"), |p| r = figure::set_chord(p, track, i, chord, &s));
                r
            }
            Slot::Append if figures.is_empty() => {
                let mut r = Ok(());
                self.edit(&format!("add {label}"), |p| {
                    let bar = p.meter_at(0).ticks_per_bar();
                    r = figure::add_block(p, track, 0, bar, chord);
                });
                r
            }
            Slot::Append => {
                let mut r = Ok(());
                let last = figures.len() - 1;
                self.edit(&format!("then {label}"), |p| r = figure::continue_with(p, track, last, chord, &s));
                r
            }
        };
        if result.is_ok() {
            // Hear what was just written.
            let figures = self.figures.get();
            let written = match slot {
                Slot::Figure(i) => figures.get(i),
                Slot::Append => figures.last(),
            };
            if let Some(f) = written {
                self.audition(track, &f.notes);
            }
        }
        self.status.set(match result {
            Ok(()) => match slot {
                Slot::Figure(_) => format!("Re-voiced to {label}"),
                Slot::Append => format!("Continued with {label}"),
            },
            Err(e) => e.to_string(),
        });
    }

    /// Applies an undoable edit.
    pub fn edit(self, label: &str, f: impl FnOnce(&mut Project)) {
        let before = self.project.get();
        self.project.update(f);
        self.history.update(|h| h.push(label, before));
    }

    pub fn undo(self) {
        let mut p = self.project.get();
        let mut h = self.history.get();
        if let Some(label) = h.undo(&mut p) {
            self.project.set(p);
            self.history.set(h);
            self.status.set(format!("Undid {label}"));
        }
    }

    pub fn redo(self) {
        let mut p = self.project.get();
        let mut h = self.history.get();
        if let Some(label) = h.redo(&mut p) {
            self.project.set(p);
            self.history.set(h);
            self.status.set(format!("Redid {label}"));
        }
    }

    pub fn quantize_selected(self, grid: Tick) {
        let Some(track) = self.selected_track.get() else { return };
        let mut moved = 0;
        self.edit("quantize", |p| {
            if let Some(t) = p.track_mut(track) {
                for c in &mut t.clips {
                    moved += cleanup::quantize(&mut c.notes, &Quantize::new(grid));
                    cleanup::fix_overlaps(&mut c.notes);
                }
            }
        });
        self.status.set(format!("Quantized {moved} notes"));
    }

    pub fn export_midi(self) {
        let p = self.project.get();
        let result = compypal_io::midi::export(&p)
            .map_err(|e| e.to_string())
            .and_then(|bytes| write_export(&p.name, "mid", &bytes));
        self.report_export(result);
    }

    pub fn export_abc(self) {
        let p = self.project.get();
        let abc = compypal_io::abc::export(&p, &Default::default());
        self.report_export(write_export(&p.name, "abc", abc.as_bytes()));
    }

    fn report_export(self, result: Result<PathBuf, String>) {
        self.status.set(match result {
            Ok(path) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        });
    }

    /// The figure lane above the piano roll, in the roll's coordinates.
    pub fn lane(self) -> Lane {
        let px = self.zoom.get() / PPQ as f64;
        let figures = self.figures.get();
        let items: Vec<LaneItem> = figures
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let until = figures.get(i + 1).map_or(f.end, |n| n.start);
                let (left, width) = (f.start as f64 * px, ((until - f.start) as f64 * px).max(24.0));
                let label = self.chord_label(&f.chord);
                LaneItem {
                    key: format!("{i}:{left}:{width}:{label}:{}", f.kind.label()),
                    index: i,
                    left,
                    width,
                    label,
                    kind: f.kind.label(),
                    unsure: f.confidence < 0.6,
                }
            })
            .collect();
        let append_left = items.last().map_or(0.0, |l| l.left + l.width);
        let editor_left = match self.editing.get() {
            Some(Slot::Figure(i)) => items.get(i).map(|l| l.left),
            Some(Slot::Append) => Some(append_left),
            None => None,
        };
        Lane { items, append_left, editor_left }
    }

    /// The piano roll's contents for the selected track.
    pub fn roll(self) -> Roll {
        let zoom = self.zoom.get();
        let show_raw = self.show_raw.get();
        let selected = self.selected_track.get();
        let live = self.recording.get().filter(|r| Some(r.track) == selected).map(|r| {
            let mut events = self.live_take.get();
            // Notes still held are drawn up to now.
            if let Some(now) = self.playhead.get() {
                events.push(RawEvent { t: now, channel: 0, msg: compypal_core::RawMsg::Cc { controller: 119, value: 0 } });
            }
            r.session(Id(0), String::new(), events)
        });
        self.project.with(|p| Roll::build(p, selected, zoom, show_raw, live.as_ref()))
    }
}

fn write_export(name: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = PathBuf::from("exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let path = dir.join(format!("{stem}.{ext}"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub const ROW: f64 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub struct LaneItem {
    pub key: String,
    pub index: usize,
    pub left: f64,
    pub width: f64,
    pub label: String,
    pub kind: &'static str,
    /// The chord reading is a guess.
    pub unsure: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Lane {
    pub items: Vec<LaneItem>,
    pub append_left: f64,
    pub editor_left: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RollNote {
    pub key: String,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub velocity: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RollRow {
    pub key: String,
    pub pitch: u8,
    pub top: f64,
    pub black: bool,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RollBar {
    pub key: String,
    pub number: u64,
    pub left: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Roll {
    pub width: f64,
    pub height: f64,
    pub rows: Vec<RollRow>,
    pub bars: Vec<RollBar>,
    pub notes: Vec<RollNote>,
    /// Notes exactly as played, before cleanup.
    pub raw: Vec<RollNote>,
    /// The take being recorded.
    pub live: Vec<RollNote>,
}

impl Roll {
    fn build(p: &Project, track: Option<Id>, zoom: f64, show_raw: bool, live: Option<&Session>) -> Self {
        let Some(t) = track.and_then(|id| p.track(id)) else {
            return Self::default();
        };
        let notes = t.absolute_notes();
        let raw: Vec<Note> = if show_raw {
            t.clips
                .iter()
                .filter_map(|c| {
                    let s = p.session(c.source_session?)?;
                    let mut ns = cleanup::import_session(s, &p.tempo, true).notes;
                    cleanup::shift(&mut ns, c.start as i64);
                    Some(ns)
                })
                .flatten()
                .collect()
        } else {
            Vec::new()
        };

        let live: Vec<Note> = live
            .map(|s| {
                let imp = cleanup::import_session(s, &p.tempo, true);
                let shift = s.start_tick as i64 - (imp.pickup_bars as u64 * s.meter.ticks_per_bar()) as i64;
                let mut ns = imp.notes;
                cleanup::shift(&mut ns, shift);
                ns
            })
            .unwrap_or_default();

        let pitches = notes.iter().chain(&raw).chain(&live).map(|n| n.pitch);
        let (lo, hi) = pitches.fold((127u8, 0u8), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let (lo, hi) = if lo > hi { (48, 72) } else { (lo.saturating_sub(3), (hi + 3).min(127)) };
        // At least two octaves, centred on what's there.
        let pad = 24u8.saturating_sub(hi - lo) / 2;
        let (lo, hi) = (lo.saturating_sub(pad), (hi + pad).min(127));

        let px = zoom / PPQ as f64;
        let live_end = live.iter().map(|n| n.end()).max().unwrap_or(0);
        let end = p.end_tick().max(live_end).max(PPQ as Tick * 16) + PPQ as Tick * 4;
        // Keys carry the geometry: keyed rows that match are not rebuilt,
        // so a note that moves must get a new key.
        let place = |n: &Note, tag: &str| {
            let (left, top, width) =
                (n.start as f64 * px, (hi - n.pitch) as f64 * ROW, (n.duration as f64 * px).max(2.0));
            RollNote {
                key: format!("{tag}{left}:{top}:{width}:{}", n.velocity),
                left,
                top,
                width,
                velocity: n.velocity,
            }
        };

        let mut bars = Vec::new();
        let mut tick = 0;
        while tick < end {
            let left = tick as f64 * px;
            bars.push(RollBar { key: format!("{}:{left}", bars.len()), number: bars.len() as u64 + 1, left });
            tick += p.meter_at(tick).ticks_per_bar();
        }

        Self {
            width: end as f64 * px,
            height: (hi - lo + 1) as f64 * ROW,
            rows: (lo..=hi)
                .rev()
                .map(|pitch| {
                    let top = (hi - pitch) as f64 * ROW;
                    let label = if t.is_drums() {
                        compypal_core::gm::drum_name(pitch).unwrap_or("").to_string()
                    } else if pitch % 12 == 0 {
                        compypal_core::gm::pitch_name(pitch)
                    } else {
                        String::new()
                    };
                    RollRow {
                        key: format!("{pitch}:{top}:{label}"),
                        pitch,
                        top,
                        black: matches!(pitch % 12, 1 | 3 | 6 | 8 | 10),
                        label,
                    }
                })
                .collect(),
            bars,
            notes: notes.iter().map(|n| place(n, "n")).collect(),
            raw: raw.iter().map(|n| place(n, "r")).collect(),
            live: live.iter().map(|n| place(n, "l")).collect(),
        }
    }
}
