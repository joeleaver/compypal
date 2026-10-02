//! App-wide state. Every project edit goes through [`Store::edit`] so the
//! UI and, later, the agent share one undo history.

use std::path::PathBuf;

use compypal_core::cleanup::{self, Quantize};
use compypal_core::{History, Id, Note, PPQ, Project, Tick};
use rinch::prelude::*;

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
}

impl Store {
    pub fn new(project: Project) -> Self {
        let first = project.tracks.first().map(|t| t.id);
        Self {
            project: Signal::new(project),
            history: Signal::new(History::default()),
            selected_track: Signal::new(first),
            show_raw: Signal::new(true),
            zoom: Signal::new(96.0),
            status: Signal::new(String::new()),
        }
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
