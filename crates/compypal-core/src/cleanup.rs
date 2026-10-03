//! Turning a performance into clean notes.
//!
//! Each operation is small and deterministic so an agent can compose them,
//! look at the result, and try again with different settings. None of them
//! touch the session they came from. Operations that drop or move notes
//! return how many they changed so the caller can report it.

use serde::{Deserialize, Serialize};

use crate::model::{MeterChange, Note, PPQ, TempoMap, Tick};
use crate::session::Session;

#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub notes: Vec<Note>,
    /// Whole bars added to the front so pickup notes before the downbeat
    /// do not land at negative time. Zero if nothing was early.
    pub pickup_bars: u32,
}

/// Places a session's notes on the timeline: `downbeat_offset` seconds into
/// the take lands on `start_tick`, and the rest follows `tempo`. Ticks come
/// back relative to `start_tick` (plus any pickup bars), ready to be a
/// clip's notes. With `sounding` set, durations include the sustain pedal.
pub fn import_session(session: &Session, tempo: &TempoMap, sounding: bool) -> Imported {
    let raw = session.notes();
    let origin_secs = tempo.tick_to_seconds(session.start_tick as f64);
    let origin = session.start_tick as f64;
    let at = |t: f64| tempo.seconds_to_tick(origin_secs + t - session.downbeat_offset) - origin;
    let ticks: Vec<(f64, f64, u8, u8)> = raw
        .iter()
        .map(|n| {
            let len = if sounding { n.sounding } else { n.held };
            (at(n.start), at(n.start + len), n.pitch, n.velocity)
        })
        .collect();

    let earliest = ticks.iter().map(|t| t.0).fold(0.0, f64::min);
    let bar = session.meter.ticks_per_bar() as f64;
    let pickup_bars = if earliest < 0.0 { (-earliest / bar).ceil() as u32 } else { 0 };
    let shift = pickup_bars as f64 * bar;

    let mut notes: Vec<Note> = ticks
        .into_iter()
        .map(|(s, e, pitch, velocity)| {
            let start = (s + shift).round().max(0.0) as Tick;
            let end = (e + shift).round().max(0.0) as Tick;
            Note { pitch, velocity, start, duration: end.saturating_sub(start).max(1) }
        })
        .collect();
    sort(&mut notes);
    Imported { notes, pickup_bars }
}

pub fn sort(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.start, n.pitch));
}

/// What [`tidy`] did, for reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TidyReport {
    pub ghosts_removed: usize,
    pub slips_removed: usize,
    pub double_strikes_merged: usize,
    pub quantized: usize,
    pub overlaps_fixed: usize,
}

/// A conservative first pass over a fresh take: drop ghost and grazed
/// notes, merge double strikes, pull timing most of the way to a sixteenth
/// grid while leaving deliberate pushes alone, and untangle overlaps. Meant
/// as a starting point; an agent or the player refines from there.
pub fn tidy(notes: &mut Vec<Note>) -> TidyReport {
    let q = PPQ as Tick;
    TidyReport {
        ghosts_removed: remove_quiet(notes, 20),
        slips_removed: remove_short(notes, q / 32),
        double_strikes_merged: merge_double_strikes(notes, q / 16),
        quantized: quantize(notes, &Quantize { strength: 0.8, window: 0.5, ends: false, ..Quantize::new(q / 4) }),
        overlaps_fixed: fix_overlaps(notes),
    }
}

/// Grid sizes by name, for tools and UI. Triplet grids end in `t`.
pub fn grid_ticks(name: &str) -> Option<Tick> {
    let q = PPQ as Tick;
    Some(match name {
        "1/1" => q * 4,
        "1/2" => q * 2,
        "1/4" => q,
        "1/8" => q / 2,
        "1/16" => q / 4,
        "1/32" => q / 8,
        "1/4t" => q * 2 / 3,
        "1/8t" => q / 3,
        "1/16t" => q / 6,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quantize {
    pub grid: Tick,
    /// 1.0 snaps fully onto the grid; 0.5 moves each note halfway there.
    pub strength: f64,
    /// Delays every other grid line by this fraction of a grid step.
    /// 0.0 is straight; about 0.33 is a triplet shuffle.
    pub swing: f64,
    /// Only move notes within this fraction of a grid step of their target.
    /// 1.0 moves everything; smaller values leave deliberate pushes alone.
    pub window: f64,
    /// Also snap note ends, keeping at least one grid step of length.
    pub ends: bool,
}

impl Quantize {
    pub fn new(grid: Tick) -> Self {
        Self { grid, strength: 1.0, swing: 0.0, window: 1.0, ends: true }
    }

    fn line(&self, i: u64) -> f64 {
        let base = (i * self.grid) as f64;
        if i % 2 == 1 { base + self.swing * self.grid as f64 } else { base }
    }

    /// The grid line nearest to `t`.
    fn nearest(&self, t: Tick) -> f64 {
        let i = t / self.grid;
        [i.saturating_sub(1), i, i + 1, i + 2]
            .into_iter()
            .map(|i| self.line(i))
            .min_by(|a, b| (a - t as f64).abs().total_cmp(&(b - t as f64).abs()))
            .unwrap()
    }
}

pub fn quantize(notes: &mut [Note], q: &Quantize) -> usize {
    if q.grid == 0 {
        return 0;
    }
    let mut moved = 0;
    for n in notes.iter_mut() {
        let target = q.nearest(n.start);
        let dist = target - n.start as f64;
        if dist.abs() > q.window * q.grid as f64 {
            continue;
        }
        let end = n.end();
        let start = (n.start as f64 + dist * q.strength).round().max(0.0) as Tick;
        let end = if q.ends {
            let target_end = q.nearest(end).max(target + q.grid as f64);
            let end = end as f64 + (target_end - end as f64) * q.strength;
            (end.round() as Tick).max(start + 1)
        } else {
            start + n.duration
        };
        if start != n.start || end != n.end() {
            moved += 1;
        }
        n.start = start;
        n.duration = end - start;
    }
    sort(notes);
    moved
}

/// Drops notes shorter than `min` ticks: grazed keys and slips.
pub fn remove_short(notes: &mut Vec<Note>, min: Tick) -> usize {
    let before = notes.len();
    notes.retain(|n| n.duration >= min);
    before - notes.len()
}

/// Drops notes softer than `min` velocity: ghost brushes of neighbouring keys.
pub fn remove_quiet(notes: &mut Vec<Note>, min: u8) -> usize {
    let before = notes.len();
    notes.retain(|n| n.velocity >= min);
    before - notes.len()
}

/// Merges repeated strikes of one pitch that start within `window` ticks of
/// each other, keeping the louder velocity and the longer reach.
pub fn merge_double_strikes(notes: &mut Vec<Note>, window: Tick) -> usize {
    sort(notes);
    let mut out: Vec<Note> = Vec::with_capacity(notes.len());
    let mut merged = 0;
    for n in notes.drain(..) {
        let twin = out
            .iter_mut()
            .rev()
            .take_while(|p| n.start - p.start <= window)
            .find(|p| p.pitch == n.pitch);
        match twin {
            Some(p) => {
                let end = p.end().max(n.end());
                p.velocity = p.velocity.max(n.velocity);
                p.duration = end - p.start;
                merged += 1;
            }
            None => out.push(n),
        }
    }
    *notes = out;
    merged
}

/// Cuts each note short where the next note of the same pitch begins, so
/// no pitch ever sounds twice at once.
pub fn fix_overlaps(notes: &mut [Note]) -> usize {
    sort(notes);
    let mut fixed = 0;
    let mut last: [Option<usize>; 128] = [None; 128];
    for i in 0..notes.len() {
        let p = notes[i].pitch as usize & 0x7f;
        if let Some(j) = last[p] {
            let start = notes[i].start;
            if notes[j].end() > start {
                notes[j].duration = (start - notes[j].start).max(1);
                fixed += 1;
            }
        }
        last[p] = Some(i);
    }
    fixed
}

/// Moves every note by `delta` ticks, dropping any that would start before 0.
pub fn shift(notes: &mut Vec<Note>, delta: i64) {
    notes.retain_mut(|n| match n.start.checked_add_signed(delta) {
        Some(s) => {
            n.start = s;
            true
        }
        None => false,
    });
}

pub fn transpose(notes: &mut [Note], semitones: i8) {
    for n in notes {
        n.pitch = (n.pitch as i16 + semitones as i16).clamp(0, 127) as u8;
    }
}

/// Pulls velocities toward their mean. 0.0 leaves them alone; 1.0 flattens
/// every note to the mean.
pub fn compress_velocity(notes: &mut [Note], amount: f64) {
    if notes.is_empty() {
        return;
    }
    let mean = notes.iter().map(|n| n.velocity as f64).sum::<f64>() / notes.len() as f64;
    for n in notes {
        let v = n.velocity as f64 + (mean - n.velocity as f64) * amount.clamp(0.0, 1.0);
        n.velocity = v.round().clamp(1.0, 127.0) as u8;
    }
}

/// How far a performance sits from a grid: the numbers an agent needs to
/// decide how hard to quantize and whether the player was ahead or behind.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimingReport {
    pub grid: Tick,
    pub notes: usize,
    /// Mean distance from the nearest line, in ticks.
    pub mean_abs_deviation: f64,
    /// Positive means late on average, negative early.
    pub mean_signed_deviation: f64,
    /// Share of notes within a 1/64 note of a line.
    pub tight_fraction: f64,
    /// How much later the offbeat lines run than the straight grid, as a
    /// fraction of a grid step. A starting point for `Quantize::swing`.
    pub measured_swing: f64,
}

pub fn timing_report(notes: &[Note], grid: Tick) -> TimingReport {
    let straight = Quantize { swing: 0.0, ..Quantize::new(grid) };
    let tight = PPQ as f64 / 16.0;
    let (mut abs, mut signed, mut tight_n) = (0.0, 0.0, 0);
    let (mut off_sum, mut off_n) = (0.0, 0);
    for n in notes {
        let target = straight.nearest(n.start);
        let d = n.start as f64 - target;
        abs += d.abs();
        signed += d;
        if d.abs() <= tight {
            tight_n += 1;
        }
        if grid > 0 && (target as u64 / grid) % 2 == 1 {
            off_sum += d;
            off_n += 1;
        }
    }
    let count = notes.len().max(1) as f64;
    let on_beat_mean = signed / count;
    TimingReport {
        grid,
        notes: notes.len(),
        mean_abs_deviation: abs / count,
        mean_signed_deviation: on_beat_mean,
        tight_fraction: tight_n as f64 / count,
        measured_swing: if off_n > 0 && grid > 0 {
            ((off_sum / off_n as f64 - on_beat_mean) / grid as f64).max(0.0)
        } else {
            0.0
        },
    }
}

/// Bars and beats, one-based, for display and for the agent: "3.2.120".
pub fn bar_beat_tick(tick: Tick, meter: &MeterChange) -> String {
    let rel = tick - meter.tick.min(tick);
    let bar = rel / meter.ticks_per_bar();
    let beat = rel % meter.ticks_per_bar() / meter.ticks_per_beat();
    let t = rel % meter.ticks_per_beat();
    format!("{}.{}.{}", bar + 1, beat + 1, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Id;
    use crate::session::{RawEvent, RawMsg};

    fn n(pitch: u8, start: Tick, duration: Tick) -> Note {
        Note { pitch, velocity: 100, start, duration }
    }

    #[test]
    fn quantize_snaps_starts_and_ends() {
        let mut notes = vec![n(60, 230, 470), n(62, 1010, 200)];
        quantize(&mut notes, &Quantize::new(240));
        assert_eq!(notes, vec![n(60, 240, 480), n(62, 960, 240)]);
    }

    #[test]
    fn quantize_strength_and_window() {
        let mut notes = vec![n(60, 200, 240), n(62, 600, 240)];
        let q = Quantize { strength: 0.5, window: 0.25, ends: false, ..Quantize::new(240) };
        quantize(&mut notes, &q);
        // 200 is 40 from 240: inside the window, moves halfway.
        // 600 is 120 from both 480 and 720: outside, left alone.
        assert_eq!(notes, vec![n(60, 220, 240), n(62, 600, 240)]);
    }

    #[test]
    fn quantize_swing_moves_offbeats() {
        let mut notes = vec![n(60, 0, 100), n(62, 330, 100)];
        let q = Quantize { swing: 1.0 / 3.0, ends: false, ..Quantize::new(240) };
        quantize(&mut notes, &q);
        assert_eq!(notes[1].start, 320);
    }

    #[test]
    fn double_strikes_merge() {
        let mut notes = vec![n(60, 0, 100), n(60, 20, 300), n(64, 10, 100)];
        assert_eq!(merge_double_strikes(&mut notes, 40), 1);
        assert_eq!(notes, vec![n(60, 0, 320), n(64, 10, 100)]);
    }

    #[test]
    fn overlaps_truncate() {
        let mut notes = vec![n(60, 0, 500), n(60, 240, 100)];
        assert_eq!(fix_overlaps(&mut notes), 1);
        assert_eq!(notes[0].duration, 240);
    }

    #[test]
    fn import_handles_pickups() {
        let s = Session {
            id: Id(1),
            name: "t".into(),
            recorded_at: 0,
            click_bpm: Some(120.0),
            meter: MeterChange { tick: 0, numerator: 4, denominator: 4 },
            downbeat_offset: 2.0,
            start_tick: 0,
            events: vec![
                // A beat before the downbeat.
                RawEvent { t: 1.5, channel: 0, msg: RawMsg::NoteOn { pitch: 60, velocity: 90 } },
                RawEvent { t: 2.0, channel: 0, msg: RawMsg::NoteOff { pitch: 60 } },
            ],
        };
        let imp = import_session(&s, &TempoMap::constant(120.0), false);
        assert_eq!(imp.pickup_bars, 1);
        assert_eq!(imp.notes, vec![Note { pitch: 60, velocity: 90, start: 2880, duration: 960 }]);
    }

    #[test]
    fn import_from_later_in_the_song() {
        // Recording from bar 3 (tick 7680) at 120 BPM, with a 2s count-in:
        // a note 2.5s into the take is a beat after the downbeat.
        let s = Session {
            id: Id(1),
            name: "t".into(),
            recorded_at: 0,
            click_bpm: Some(120.0),
            meter: MeterChange { tick: 0, numerator: 4, denominator: 4 },
            downbeat_offset: 2.0,
            start_tick: 7680,
            events: vec![
                RawEvent { t: 2.5, channel: 0, msg: RawMsg::NoteOn { pitch: 60, velocity: 90 } },
                RawEvent { t: 3.0, channel: 0, msg: RawMsg::NoteOff { pitch: 60 } },
            ],
        };
        let imp = import_session(&s, &TempoMap::constant(120.0), false);
        assert_eq!(imp.notes, vec![Note { pitch: 60, velocity: 90, start: 960, duration: 960 }]);
    }

    #[test]
    fn report_finds_late_player() {
        let notes: Vec<_> = (0..8).map(|i| n(60, i * 480 + 30, 100)).collect();
        let r = timing_report(&notes, 240);
        assert!((r.mean_signed_deviation - 30.0).abs() < 1e-9);
        assert_eq!(bar_beat_tick(4 * 960 + 960 + 5, &MeterChange { tick: 0, numerator: 4, denominator: 4 }), "2.2.5");
    }
}
