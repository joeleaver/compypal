//! App-wide state. Every project edit goes through [`Store::edit`] so the
//! UI and, later, the agent share one undo history.

use std::path::PathBuf;

use compypal_core::cleanup::{self, Quantize};
use compypal_core::figure::{self, Figure, FigureSettings};
use compypal_core::theory::{self, Chord, Spelling};
use compypal_core::{History, Id, Note, PPQ, Project, Tick};
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
}

impl Store {
    pub fn new(project: Project) -> Self {
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
        self.project.with(|p| Roll::build(p, selected, zoom, show_raw))
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
}

impl Roll {
    fn build(p: &Project, track: Option<Id>, zoom: f64, show_raw: bool) -> Self {
        let Some(t) = track.and_then(|id| p.track(id)) else {
            return Self::default();
        };
        let notes = t.absolute_notes();
        let raw: Vec<Note> = if show_raw {
            t.clips
                .iter()
                .filter_map(|c| {
                    let s = p.session(c.source_session?)?;
                    let mut ns = cleanup::import_session(s, &p.tempo, false).notes;
                    cleanup::shift(&mut ns, c.start as i64);
                    Some(ns)
                })
                .flatten()
                .collect()
        } else {
            Vec::new()
        };

        let pitches = notes.iter().chain(&raw).map(|n| n.pitch);
        let (lo, hi) = pitches.fold((127u8, 0u8), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let (lo, hi) = if lo > hi { (48, 72) } else { (lo.saturating_sub(3), (hi + 3).min(127)) };
        // At least two octaves, centred on what's there.
        let pad = 24u8.saturating_sub(hi - lo) / 2;
        let (lo, hi) = (lo.saturating_sub(pad), (hi + pad).min(127));

        let px = zoom / PPQ as f64;
        let end = p.end_tick().max(PPQ as Tick * 16) + PPQ as Tick * 4;
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
        }
    }
}
