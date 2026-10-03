//! App-wide state. Every project edit goes through [`Store::edit`] so the
//! UI and, later, the agent share one undo history.

use std::path::PathBuf;

use compypal_core::cleanup::{self, Quantize};
use compypal_core::figure::{self, Figure, FigureSettings};
use compypal_core::theory::{self, Chord, Spelling};
use compypal_audio::input::MidiIn;
use compypal_audio::{Engine, Schedule, ScheduleOptions, schedule};
use compypal_core::{Clip, History, Id, MeterChange, Note, PPQ, Project, RawEvent, Session, Tick, arrange, text};
use rinch::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// One track's piano roll, with the chord lane.
    Edit,
    /// Every track across the song, with sections.
    Arrange,
}

/// Where the chord editor is open: on a figure, or on the slot after the
/// last one, where typing a chord continues the music.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Figure(usize),
    Append,
    /// A chord in the song lane: changing it re-voices every track.
    Song(usize),
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
    /// What the user is pointing at, shared with the agent.
    pub selection: Signal<Option<compypal_mcp::Selection>>,
    /// Claude Code sessions attached through /ide.
    pub agents: Signal<usize>,
    pub view: Signal<View>,
    /// Bars selected in the arranger, first and last, one-based.
    pub bar_sel: Signal<Option<(u32, u32)>>,
    pub section_draft: Signal<String>,
    /// Horizontal zoom of the arranger, pixels per quarter note.
    pub arr_zoom: Signal<f64>,
    /// The track whose instrument picker is open.
    pub instrument_edit: Signal<Option<Id>>,
    pub instrument_draft: Signal<String>,
    /// The section-name popover is open.
    pub section_edit: Signal<bool>,
    /// Show every track's piano roll at once, under the song's chords.
    pub stacked: Signal<bool>,
    /// The song's chords, from all pitched tracks together.
    pub harmony: Memo<Vec<Figure>>,
    pub journal: Option<&'static compypal_audio::journal::Journal>,
    /// Logging everything played, whatever else is going on.
    pub listening: Signal<bool>,
    /// Recent jams for the sidebar, refreshed from the journal.
    pub jams: Signal<Vec<JamRow>>,
    /// Notes selected in the piano roll, absolute, on the selected track.
    pub note_sel: Signal<Vec<Note>>,
    pub note_drag: Signal<Option<NoteDrag>>,
    /// Where notes snap to when placed or moved; 0 is off.
    pub snap: Signal<Tick>,
    /// The length new notes get: the last one drawn or resized.
    pub last_len: Signal<Tick>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragMode {
    /// Moving notes: time by the snap, pitch by rows.
    Move,
    /// Stretching notes from their right edge.
    Resize,
    /// Drawing a new note: dragging sets its length.
    Create,
}

/// A note gesture in progress: the notes it started from, and how far the
/// pointer has taken them so far.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteDrag {
    pub mode: DragMode,
    pub origin: Vec<Note>,
    pub dt: i64,
    pub dp: i32,
}

impl NoteDrag {
    /// The notes as they'd be if the gesture ended now.
    pub fn result(&self) -> Vec<Note> {
        self.origin
            .iter()
            .map(|n| match self.mode {
                DragMode::Move => Note {
                    start: n.start.saturating_add_signed(self.dt),
                    pitch: (n.pitch as i32 + self.dp).clamp(0, 127) as u8,
                    ..*n
                },
                DragMode::Resize | DragMode::Create => {
                    Note { duration: (n.duration as i64 + self.dt).max(PPQ as i64 / 32) as Tick, ..*n }
                }
            })
            .collect()
    }
}

/// A jam as the sidebar lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct JamRow {
    pub key: String,
    pub id: u64,
    pub when: String,
    pub detail: String,
    pub live: bool,
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
            own_tempo: false,
            events,
        }
    }
}

impl Store {
    pub fn new(
        project: Project,
        engine: Option<&'static Engine>,
        midi: Option<&'static MidiIn>,
        journal: Option<&'static compypal_audio::journal::Journal>,
    ) -> Self {
        let first = project.tracks.first().map(|t| t.id);
        let project = Signal::new(project);
        let selected_track = Signal::new(first);
        let harmony = Memo::new(move || project.with(|p| figure::harmony(p, &FigureSettings::default())));
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
            selection: Signal::new(None),
            agents: Signal::new(0),
            view: Signal::new(View::Edit),
            bar_sel: Signal::new(None),
            section_draft: Signal::new(String::new()),
            arr_zoom: Signal::new(24.0),
            instrument_edit: Signal::new(None),
            instrument_draft: Signal::new(String::new()),
            section_edit: Signal::new(false),
            stacked: Signal::new(false),
            harmony,
            journal,
            listening: Signal::new(journal.is_some_and(|j| j.listening())),
            jams: Signal::new(Vec::new()),
            note_sel: Signal::new(Vec::new()),
            note_drag: Signal::new(None),
            snap: Signal::new(PPQ as Tick / 4),
            last_len: Signal::new(PPQ as Tick / 2),
        }
    }

    // --- note editing ----------------------------------------------------------

    fn snap_tick(self, t: f64) -> Tick {
        let snap = self.snap.get();
        let t = t.max(0.0);
        if snap == 0 { t.round() as Tick } else { ((t / snap as f64).floor() as Tick) * snap }
    }

    fn snap_delta(self, dt: f64) -> i64 {
        let snap = self.snap.get() as f64;
        if snap == 0.0 { dt.round() as i64 } else { ((dt / snap).round() * snap) as i64 }
    }

    /// Selects notes and tells the agent: "these notes" is now a thing.
    pub fn select_notes(self, notes: Vec<Note>) {
        let track = self.selected_track.get();
        if let (Some(track), false) = (track, notes.is_empty()) {
            let start = notes.iter().map(|n| n.start).min().unwrap();
            let end = notes.iter().map(|n| n.end()).max().unwrap();
            self.selection.set(Some(compypal_mcp::Selection {
                track: Some(track),
                start,
                end,
                figure: None,
                section: None,
                notes: notes.clone(),
            }));
        }
        self.note_sel.set(notes);
    }

    /// A press in the piano roll at (`x`, `y`) pixels from the grid's top
    /// left (after the key column). Returns the gesture to follow, if any.
    pub fn roll_press(self, x: f64, y: f64, shift: bool) -> Option<NoteDrag> {
        let track = self.selected_track.get()?;
        let roll = self.roll();
        let tick = x / roll.px;
        let row = (y / ROW).floor();
        if row < 0.0 {
            return None;
        }
        let pitch = (roll.top_pitch as i32 - row as i32).clamp(0, 127) as u8;
        let notes = self.project.with(|p| p.track(track).map(|t| t.absolute_notes()).unwrap_or_default());
        let hit = notes.iter().rev().find(|n| n.pitch == pitch && (n.start as f64) <= tick && tick < n.end() as f64).copied();
        match hit {
            Some(n) => {
                let mut sel = self.note_sel.get();
                let selected = sel.contains(&n);
                if shift {
                    if selected { sel.retain(|s| *s != n) } else { sel.push(n) }
                    self.select_notes(sel);
                    return None;
                }
                if !selected {
                    sel = vec![n];
                    self.select_notes(sel.clone());
                }
                self.audition(track, &[n]);
                let near_end = (n.end() as f64 - tick) * roll.px < 6.0;
                let mode = if near_end { DragMode::Resize } else { DragMode::Move };
                Some(NoteDrag { mode, origin: sel, dt: 0, dp: 0 })
            }
            None => {
                if !shift && !self.note_sel.get().is_empty() {
                    // A press on empty space clears the selection first;
                    // the next one draws.
                    self.select_notes(Vec::new());
                    return None;
                }
                let n = Note { pitch, velocity: 90, start: self.snap_tick(tick), duration: self.last_len.get() };
                self.audition(track, &[n]);
                Some(NoteDrag { mode: DragMode::Create, origin: vec![n], dt: 0, dp: 0 })
            }
        }
    }

    /// The pointer has moved `dx`, `dy` pixels since the press.
    pub fn roll_drag(self, drag: &mut NoteDrag, dx: f64, dy: f64) {
        let px = self.roll().px;
        drag.dt = self.snap_delta(dx / px);
        if drag.mode == DragMode::Move {
            drag.dp = -(dy / ROW).round() as i32;
        }
        self.note_drag.set(Some(drag.clone()));
    }

    /// Commits the gesture as one undoable edit.
    pub fn roll_release(self, drag: NoteDrag) {
        self.note_drag.set(None);
        let Some(track) = self.selected_track.get() else { return };
        let moved = drag.result();
        let (old, label) = match drag.mode {
            DragMode::Create => (Vec::new(), "add note"),
            DragMode::Move if drag.dt == 0 && drag.dp == 0 => return,
            DragMode::Resize if drag.dt == 0 => return,
            DragMode::Move => (drag.origin.clone(), "move notes"),
            DragMode::Resize => (drag.origin.clone(), "resize notes"),
        };
        if drag.mode != DragMode::Move
            && let Some(n) = moved.first()
        {
            self.last_len.set(n.duration);
        }
        self.edit(label, |p| p.replace_notes(track, &old, &moved));
        if drag.mode == DragMode::Move && drag.dp != 0 {
            self.audition(track, &moved);
        }
        self.select_notes(moved);
    }

    /// Deletes, transposes or moves the selected notes from the keyboard.
    /// Returns whether the key was used.
    pub fn roll_key(self, key: &str, shift: bool, ctrl: bool) -> bool {
        let Some(track) = self.selected_track.get() else { return false };
        if ctrl && key == "a" {
            let all = self.project.with(|p| p.track(track).map(|t| t.absolute_notes()).unwrap_or_default());
            self.select_notes(all);
            return true;
        }
        let sel = self.note_sel.get();
        if sel.is_empty() {
            return false;
        }
        let step = self.snap.get().max(PPQ as Tick / 16) as i64;
        let (label, new): (&str, Vec<Note>) = match key {
            "Delete" | "Backspace" => ("delete notes", Vec::new()),
            "ArrowUp" | "ArrowDown" => {
                let d: i32 = if shift { 12 } else { 1 } * if key == "ArrowUp" { 1 } else { -1 };
                ("transpose notes", sel.iter().map(|n| Note { pitch: (n.pitch as i32 + d).clamp(0, 127) as u8, ..*n }).collect())
            }
            "ArrowLeft" | "ArrowRight" => {
                let d = if key == "ArrowRight" { step } else { -step };
                if sel.iter().any(|n| (n.start as i64 + d) < 0) {
                    return true;
                }
                ("move notes", sel.iter().map(|n| Note { start: n.start.saturating_add_signed(d), ..*n }).collect())
            }
            "Escape" => {
                self.select_notes(Vec::new());
                return true;
            }
            _ => return false,
        };
        self.edit(label, |p| p.replace_notes(track, &sel, &new));
        if key.starts_with("ArrowU") || key.starts_with("ArrowD") {
            self.audition(track, &new);
        }
        self.select_notes(new);
        true
    }

    /// Turns the always-on journal on or off, and remembers the choice.
    pub fn toggle_listening(self) {
        let Some(j) = self.journal else { return };
        let on = !j.listening();
        j.set_listening(on);
        self.listening.set(on);
        crate::autosave::save_settings(&crate::autosave::Settings { listening: on });
        self.status.set(if on {
            "Listening: everything you play is kept in the journal, and jams show up in the sidebar".into()
        } else {
            "Stopped listening".into()
        });
    }

    /// Whether a text field has the keyboard, so shortcuts stand aside. Every
    /// text field lives in a popover, so this is "is one open".
    pub fn is_typing(self) -> bool {
        self.editing.get().is_some() || self.instrument_edit.get().is_some() || self.section_edit.get()
    }

    // --- arranging ----------------------------------------------------------

    /// Selects bar `bar`, or extends the selection to it.
    pub fn select_bar(self, bar: u32, extend: bool) {
        let bar = bar.max(1);
        let sel = match (self.bar_sel.get(), extend) {
            (Some((a, b)), true) => (a.min(bar), b.max(bar)),
            _ => (bar, bar),
        };
        self.bar_sel.set(Some(sel));
        let (from, to, section) = self.project.with(|p| {
            let (from, to) = (text::bar_start(p, sel.0), text::bar_start(p, sel.1 + 1));
            let section = p.sections.iter().find(|s| s.start == from && s.start + s.length == to).map(|s| s.name.clone());
            (from, to, section)
        });
        self.section_draft.set(section.clone().unwrap_or_default());
        // Bars across every track: what "this" means to the agent now.
        self.selection.set(Some(compypal_mcp::Selection { track: None, start: from, end: to, figure: None, section, notes: Vec::new() }));
        let start = self.project.with(|p| text::bar_start(p, sel.0));
        self.cursor.set(start);
    }

    pub fn select_bars(self, first: u32, last: u32) {
        self.select_bar(first, false);
        self.select_bar(last, true);
    }

    /// Duplicates, deletes, or inserts empty bars before, the selection.
    pub fn bars_action(self, action: &str) {
        let Some((a, b)) = self.bar_sel.get() else { return };
        let n = b - a + 1;
        let (from, to) = self.project.with(|p| (text::bar_start(p, a), text::bar_start(p, b + 1)));
        match action {
            "duplicate" => {
                self.edit(&format!("duplicate bars {a}-{b}"), |p| arrange::duplicate_time(p, from, to - from));
                self.select_bars(b + 1, b + n);
                self.status.set(format!("Bars {a}-{b} now repeat as {}-{}", b + 1, b + n));
            }
            "delete" => {
                self.edit(&format!("delete bars {a}-{b}"), |p| arrange::delete_time(p, from, to - from));
                self.bar_sel.set(None);
                self.status.set(format!("Removed bars {a}-{b}"));
            }
            "insert" => {
                self.edit(&format!("insert {n} bars"), |p| arrange::insert_time(p, from, to - from));
                self.status.set(format!("Opened {n} empty bar(s) at bar {a}"));
            }
            _ => {}
        }
    }

    /// Names the selected bars as a section, replacing any section that
    /// starts there; an empty name removes it.
    pub fn name_section(self) {
        let Some((a, b)) = self.bar_sel.get() else { return };
        let name = self.section_draft.get().trim().to_string();
        self.edit("section", |p| {
            let (from, to) = (text::bar_start(p, a), text::bar_start(p, b + 1));
            p.sections.retain(|s| s.start != from);
            if !name.is_empty() {
                let id = p.alloc_id();
                p.sections.push(compypal_core::Section { id, name: name.clone(), start: from, length: to - from });
                p.sections.sort_by_key(|s| s.start);
            }
        });
    }

    pub fn open_clip(self, track: Id, start: Tick) {
        self.select_track(track);
        self.cursor.set(start);
        self.view.set(View::Edit);
    }

    // --- instruments ----------------------------------------------------------

    pub fn instrument_suggestions(self) -> Vec<(u8, &'static str)> {
        let q = self.instrument_draft.get().to_lowercase();
        compypal_core::gm::PROGRAMS
            .iter()
            .enumerate()
            .filter(|(_, n)| q.is_empty() || n.to_lowercase().contains(&q))
            .take(10)
            .map(|(i, n)| (i as u8, *n))
            .collect()
    }

    pub fn set_instrument(self, track: Id, program: u8) {
        self.instrument_edit.set(None);
        self.edit("instrument", |p| {
            if let Some(t) = p.track_mut(track) {
                t.program = program;
            }
        });
        if self.selected_track.get() == Some(track) {
            self.monitor_selected();
        }
        // Let them hear it: a chord on the new instrument.
        let notes: Vec<Note> = [60u8, 64, 67]
            .iter()
            .map(|&pitch| Note { pitch, velocity: 90, start: 0, duration: PPQ as Tick * 2 })
            .collect();
        self.audition(track, &notes);
    }

    /// The arranger's contents.
    pub fn arrangement(self) -> Arrangement {
        let px = self.arr_zoom.get() / PPQ as f64;
        self.project.with(|p| Arrangement::build(p, px))
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
        if self.selected_track.get() != Some(id) {
            self.note_sel.set(Vec::new());
        }
        self.selected_track.set(Some(id));
        self.monitor_selected();
        // The whole track, however long it grows.
        self.selection.set(Some(compypal_mcp::Selection { track: Some(id), start: 0, end: Tick::MAX, figure: None, section: None, notes: Vec::new() }));
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
            let cursor = self.cursor.get();
            let from = self.project.with(|p| p.tempo.tick_to_seconds(cursor as f64));
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

    /// Plays ticks `from..to` of the song, every track.
    pub fn audition_span(self, from: Tick, to: Tick) {
        let p = self.project.get();
        self.audition_project(&p, from, to);
    }

    fn audition_project(self, p: &Project, from: Tick, to: Tick) {
        let Some(engine) = self.engine else { return };
        let song = schedule::build(p, ScheduleOptions::default());
        let (a, b) = (p.tempo.tick_to_seconds(from as f64), p.tempo.tick_to_seconds(to as f64));
        engine.audition(schedule::excerpt(&song, a, b));
    }

    /// Plays what the editor's slot would sound like with `chord`.
    fn preview(self, chord: &Chord) {
        let Some(track) = self.selected_track.get() else { return };
        let figures = self.figures.get();
        let slot = self.editing.get();
        if let Some(Slot::Song(i)) = slot {
            let song = self.harmony.get();
            let Some(f) = song.get(i) else { return };
            let (from, to) = (f.start, song.get(i + 1).map_or(f.end, |n| n.start));
            let mut p = self.project.get();
            figure::set_harmony(&mut p, from, to, chord);
            self.audition_project(&p, from, to);
            return;
        }
        let notes = self.project.with(|p| match slot? {
            Slot::Figure(i) => {
                let f = figures.get(i)?;
                Some(figure::revoice(&f.notes, &f.chord, chord, p.key))
            }
            Slot::Append if !figures.is_empty() => Some(figure::continued(&figures, figures.len() - 1, chord, p).2),
            Slot::Song(_) => None,
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
        if let Slot::Song(i) = slot {
            let song = self.harmony.get();
            if let Some(f) = song.get(i) {
                let (from, to) = (f.start, song.get(i + 1).map_or(f.end, |n| n.start));
                self.audition_span(from, to);
                let section = self.project.with(|p| {
                    p.sections.iter().find(|s| s.start <= from && from < s.start + s.length).map(|s| s.name.clone())
                });
                self.selection.set(Some(compypal_mcp::Selection { track: None, start: from, end: to, figure: None, section, notes: Vec::new() }));
            }
        }
        if let (Slot::Figure(i), Some(track)) = (slot, self.selected_track.get())
            && let Some(f) = self.figures.get().get(i)
        {
            self.selection.set(Some(compypal_mcp::Selection { track: Some(track), start: f.start, end: f.end, figure: Some(i), section: None, notes: Vec::new() }));
        }
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
            Some(Slot::Song(i)) => self
                .harmony
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
            Slot::Song(i) if i + 1 < self.harmony.get().len() => Slot::Song(i + 1),
            Slot::Song(_) => {
                // The end of the song lane: nothing further to type into.
                if let Some(chord) = chord {
                    self.apply_chord(track, slot, &chord, &figures);
                }
                self.close_editor();
                return;
            }
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
        if let Slot::Song(i) = slot {
            let song = self.harmony.get();
            let Some(f) = song.get(i) else { return };
            if f.chord == *chord {
                return;
            }
            let (from, to) = (f.start, song.get(i + 1).map_or(f.end, |n| n.start));
            let mut moved = 0;
            self.edit(&format!("harmony {label}"), |p| moved = figure::set_harmony(p, from, to, chord));
            self.audition_span(from, to);
            self.status.set(format!("{label} across {moved} track(s)"));
            return;
        }
        let result = match slot {
            // Handled above.
            Slot::Song(_) => return,
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
                _ => figures.last(),
            };
            if let Some(f) = written {
                self.audition(track, &f.notes);
            }
        }
        self.status.set(match result {
            Ok(()) => match slot {
                Slot::Append => format!("Continued with {label}"),
                _ => format!("Re-voiced to {label}"),
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

    pub fn undo(self) -> Option<String> {
        let mut p = self.project.get();
        let mut h = self.history.get();
        let label = h.undo(&mut p)?;
        self.project.set(p);
        self.history.set(h);
        self.status.set(format!("Undid {label}"));
        Some(label)
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

    pub fn export_midi(self) -> Result<PathBuf, String> {
        let p = self.project.get();
        let result = compypal_io::midi::export(&p)
            .map_err(|e| e.to_string())
            .and_then(|bytes| write_export(&p.name, "mid", &bytes));
        self.report_export(result)
    }

    pub fn export_abc(self) -> Result<PathBuf, String> {
        let p = self.project.get();
        let abc = compypal_io::abc::export(&p, &Default::default());
        self.report_export(write_export(&p.name, "abc", abc.as_bytes()))
    }

    fn report_export(self, result: Result<PathBuf, String>) -> Result<PathBuf, String> {
        self.status.set(match &result {
            Ok(path) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        });
        result
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
            Some(Slot::Song(_)) | None => None,
        };
        Lane { items, append_left, editor_left }
    }

    /// The song chord lane, in the roll's coordinates.
    pub fn song_lane(self) -> Lane {
        let px = self.zoom.get() / PPQ as f64;
        let figures = self.harmony.get();
        let items: Vec<LaneItem> = figures
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let until = figures.get(i + 1).map_or(f.end, |n| n.start);
                let (left, width) = (f.start as f64 * px, ((until - f.start) as f64 * px).max(24.0));
                let label = self.chord_label(&f.chord);
                LaneItem {
                    key: format!("s{i}:{left}:{width}:{label}"),
                    index: i,
                    left,
                    width,
                    label,
                    kind: "",
                    unsure: f.confidence < 0.6,
                }
            })
            .collect();
        let editor_left = match self.editing.get() {
            Some(Slot::Song(i)) => items.get(i).map(|l| l.left),
            _ => None,
        };
        Lane { items, append_left: 0.0, editor_left }
    }

    /// Every track's notes, stacked, in the roll's coordinates.
    pub fn stack(self) -> Vec<StackRow> {
        let px = self.zoom.get() / PPQ as f64;
        let selected = self.selected_track.get();
        self.project.with(|p| {
            p.tracks
                .iter()
                .map(|t| {
                    let notes = t.absolute_notes();
                    let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.pitch), hi.max(n.pitch)));
                    let (lo, hi) = if lo > hi { (60, 67) } else { (lo.saturating_sub(1), (hi + 1).min(127)) };
                    let (lo, hi) = if hi - lo < 8 { (lo.saturating_sub((8 - (hi - lo)) / 2), hi.max(lo + 8)) } else { (lo, hi) };
                    let instrument = if t.is_drums() { "Drums".to_string() } else { compypal_core::gm::program_name(t.program).to_string() };
                    StackRow {
                        key: format!("{}:{}:{instrument}:{}:{}:{lo}:{hi}:{px}", t.id, t.name, notes.len(), selected == Some(t.id)),
                        track: t.id,
                        name: t.name.clone(),
                        instrument,
                        height: (hi - lo + 1) as f64 * STACK_ROW,
                        notes: notes
                            .iter()
                            .map(|n| {
                                let (left, top, width) =
                                    (n.start as f64 * px, (hi - n.pitch) as f64 * STACK_ROW, (n.duration as f64 * px).max(2.0));
                                RollNote { key: format!("{left}:{top}:{width}:{}", n.velocity), left, top, width, velocity: n.velocity, selected: false }
                            })
                            .collect(),
                        c_lines: (lo..=hi).filter(|p| p % 12 == 0).map(|p| (hi - p) as f64 * STACK_ROW).collect(),
                    }
                })
                .collect()
        })
    }

    /// Sections in the roll's coordinates.
    pub fn roll_sections(self) -> Vec<ArrSection> {
        let px = self.zoom.get() / PPQ as f64;
        self.project.with(|p| Arrangement::build(p, px).sections)
    }

    /// The pitch at the middle of the selected track's notes, for scrolling
    /// the roll to them.
    pub fn center_pitch(self) -> u8 {
        let Some(track) = self.selected_track.get() else { return 60 };
        self.project.with(|p| {
            let notes = p.track(track).map(|t| t.absolute_notes()).unwrap_or_default();
            if notes.is_empty() {
                return if p.track(track).is_some_and(|t| t.is_drums()) { 42 } else { 60 };
            }
            let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.pitch), hi.max(n.pitch)));
            ((lo as u16 + hi as u16) / 2) as u8
        })
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
        let sel = self.note_sel.get();
        let drag = self.note_drag.get();
        self.project.with(|p| Roll::build(p, selected, zoom, show_raw, live.as_ref(), &sel, drag.as_ref()))
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
    pub selected: bool,
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
    /// Pixels per tick, and the pitch of the top row: for turning a click
    /// into a time and a pitch.
    pub px: f64,
    pub top_pitch: u8,
}

impl Roll {
    fn build(
        p: &Project,
        track: Option<Id>,
        zoom: f64,
        show_raw: bool,
        live: Option<&Session>,
        selected: &[Note],
        drag: Option<&NoteDrag>,
    ) -> Self {
        let Some(t) = track.and_then(|id| p.track(id)) else {
            return Self::default();
        };
        let mut notes = t.absolute_notes();
        // While a gesture is under way, show its result in place of its origin.
        let mut highlight: Vec<Note> = selected.to_vec();
        if let Some(d) = drag {
            notes.retain(|n| !d.origin.contains(n));
            let preview = d.result();
            notes.extend(preview.iter().copied());
            highlight = preview;
        }
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

        // A fixed range, scrolled vertically, so the grid never moves under
        // the pointer as notes come and go: the piano, or the GM drum keys.
        let (lo, hi) = if t.is_drums() { (35u8, 81u8) } else { (21u8, 108u8) };

        let px = zoom / PPQ as f64;
        let live_end = live.iter().map(|n| n.end()).max().unwrap_or(0);
        let end = p.end_tick().max(live_end).max(PPQ as Tick * 16) + PPQ as Tick * 4;
        // Keys carry the geometry: keyed rows that match are not rebuilt,
        // so a note that moves must get a new key.
        let place = |n: &Note, tag: &str| {
            let (left, top, width) =
                (n.start as f64 * px, (hi - n.pitch) as f64 * ROW, (n.duration as f64 * px).max(2.0));
            let selected = tag == "n" && highlight.contains(n);
            RollNote {
                key: format!("{tag}{left}:{top}:{width}:{}:{selected}", n.velocity),
                left,
                top,
                width,
                velocity: n.velocity,
                selected,
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
            px,
            top_pitch: hi,
        }
    }
}

pub const ARR_ROW: f64 = 44.0;
pub const STACK_ROW: f64 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub struct StackRow {
    pub key: String,
    pub track: Id,
    pub name: String,
    pub instrument: String,
    pub height: f64,
    pub notes: Vec<RollNote>,
    /// Where each C sits, for orientation.
    pub c_lines: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrNote {
    pub key: String,
    pub left: f64,
    pub top: f64,
    pub width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrClip {
    pub key: String,
    pub track: Id,
    pub start: Tick,
    pub left: f64,
    pub width: f64,
    pub name: String,
    pub recorded: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrRow {
    pub key: String,
    pub track: Id,
    pub name: String,
    pub instrument: String,
    pub muted: bool,
    pub clips: Vec<ArrClip>,
    pub notes: Vec<ArrNote>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrSection {
    pub key: String,
    pub name: String,
    pub left: f64,
    pub width: f64,
    pub first: u32,
    pub last: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Arrangement {
    pub width: f64,
    pub px: f64,
    pub bars: Vec<RollBar>,
    pub sections: Vec<ArrSection>,
    pub rows: Vec<ArrRow>,
}

impl Arrangement {
    fn build(p: &Project, px: f64) -> Self {
        let end = p.end_tick().max(PPQ as Tick * 4 * 16) + PPQ as Tick * 4 * 4;
        let mut bars = Vec::new();
        let mut tick = 0;
        while tick < end {
            let left = tick as f64 * px;
            bars.push(RollBar { key: format!("{}:{left}", bars.len()), number: bars.len() as u64 + 1, left });
            tick += p.meter_at(tick).ticks_per_bar();
        }
        let sections = p
            .sections
            .iter()
            .map(|s| {
                let (left, width) = (s.start as f64 * px, s.length as f64 * px);
                ArrSection {
                    key: format!("{}:{left}:{width}:{}", s.id, s.name),
                    name: s.name.clone(),
                    left,
                    width,
                    first: text::bar_of(p, s.start),
                    last: text::bar_of(p, (s.start + s.length).saturating_sub(1)),
                }
            })
            .collect();
        let rows = p
            .tracks
            .iter()
            .map(|t| {
                let notes = t.absolute_notes();
                let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.pitch), hi.max(n.pitch)));
                let span = (hi.saturating_sub(lo)).max(12) as f64;
                let inner = ARR_ROW - 12.0;
                let instrument =
                    if t.is_drums() { "Drums".to_string() } else { compypal_core::gm::program_name(t.program).to_string() };
                ArrRow {
                    key: format!("{}:{}:{}:{}:{}", t.id, t.name, instrument, t.muted, notes.len()),
                    track: t.id,
                    name: t.name.clone(),
                    instrument,
                    muted: t.muted,
                    clips: t
                        .clips
                        .iter()
                        .map(|c| {
                            let (left, width) = (c.start as f64 * px, (c.length as f64 * px).max(4.0));
                            ArrClip {
                                key: format!("{}:{left}:{width}", c.id),
                                track: t.id,
                                start: c.start,
                                left,
                                width,
                                name: c.name.clone(),
                                recorded: c.source_session.is_some(),
                            }
                        })
                        .collect(),
                    notes: notes
                        .iter()
                        .map(|n| {
                            let (left, width) = (n.start as f64 * px, (n.duration as f64 * px).max(1.5));
                            let top = 6.0 + (hi.saturating_sub(n.pitch)) as f64 / span * (inner - 2.0);
                            ArrNote { key: format!("{left}:{top}:{width}"), left, top, width }
                        })
                        .collect(),
                }
            })
            .collect();
        Self { width: end as f64 * px, px, bars, sections, rows }
    }
}
