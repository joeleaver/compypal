//! Jams: stretches of playing caught by the always-on journal.
//!
//! Nothing here touches files or clocks. Events carry absolute times in
//! seconds (unix time in the journal). This module finds the beat in free
//! playing, summarises a jam so an agent can tell jams apart without
//! reading them, and turns a stretch of one into a [`Session`] that lands on
//! the timeline like any recorded take.

use serde::{Deserialize, Serialize};

use crate::figure::{self, FigureSettings};
use crate::model::{Id, MeterChange, Note, TempoMap, Tick};
use crate::session::{RawEvent, RawMsg, Session};
use crate::theory::{self, Spelling};

/// A silence this long ends a jam.
pub const GAP_SECONDS: f64 = 8.0;
/// Fewer notes than this is a stray touch, not a jam.
pub const MIN_NOTES: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoGuess {
    pub bpm: f64,
    /// A time (same clock as the onsets) where a beat falls, taken as a
    /// downbeat: the beat nearest the first strong note.
    pub downbeat: f64,
    /// 0..1: how firmly the notes sit on this grid. Below ~0.35, treat the
    /// playing as free time.
    pub confidence: f64,
}

/// Finds a beat grid in free playing from note onsets (seconds) and
/// velocities. For each tempo from 50 to 200 BPM it asks two things: do
/// the notes sit on this tempo's eighth-note grid, and do the accents
/// (notes louder than average) fall on its beats? The grid settles the
/// speed of the pulse; the accents settle which level of it is the beat,
/// the usual half/double-time ambiguity. A gentle preference for moderate
/// tempos breaks remaining ties.
pub fn estimate_tempo(onsets: &[(f64, u8)]) -> Option<TempoGuess> {
    if onsets.len() < 8 {
        return None;
    }
    let n = onsets.len() as f64;
    let mean_v = onsets.iter().map(|(_, v)| *v as f64).sum::<f64>() / n;
    let accent_total: f64 = onsets.iter().map(|(_, v)| (*v as f64 - mean_v).abs()).sum();
    // Length of the mean unit vector of onsets on a circle of `period`,
    // each weighted by `w`; and its angle.
    let align = |period: f64, w: &dyn Fn(u8) -> f64, total: f64| -> (f64, f64) {
        let (mut re, mut im) = (0.0, 0.0);
        for (t, v) in onsets {
            let a = std::f64::consts::TAU * t / period;
            re += w(*v) * a.cos();
            im += w(*v) * a.sin();
        }
        if total <= 0.0 { (0.0, 0.0) } else { ((re * re + im * im).sqrt() / total, im.atan2(re)) }
    };
    let plain = |_: u8| 1.0;
    let accent = |v: u8| v as f64 - mean_v;
    let mut best: Option<(f64, f64, f64)> = None; // score, bpm, grid fit
    let mut bpm = 50.0;
    while bpm <= 200.0 {
        let beat = 60.0 / bpm;
        let (grid, _) = align(beat / 2.0, &plain, n);
        let (acc, _) = align(beat, &accent, accent_total);
        let prior = -0.12 * (bpm / 105.0).log2().powi(2);
        let score = 0.6 * grid + 0.4 * acc + prior;
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, bpm, grid));
        }
        bpm += 0.25;
    }
    let (_, bpm, grid) = best?;
    let beat = 60.0 / bpm;
    // The beat grid's phase, from all the notes; the downbeat is the beat
    // nearest the first note of at least average loudness.
    let (_, phase) = align(beat, &|v| v as f64, onsets.iter().map(|(_, v)| *v as f64).sum());
    let first = onsets.iter().find(|(_, v)| *v as f64 >= mean_v).unwrap_or(&onsets[0]).0;
    let grid0 = phase / std::f64::consts::TAU * beat;
    let k = ((first - grid0) / beat).round();
    Some(TempoGuess { bpm, downbeat: grid0 + k * beat, confidence: grid.clamp(0.0, 1.0) })
}

/// Places a beat grid of a known tempo on the onsets: for when the player
/// (or an agent) knows the tempo better than the guess.
pub fn fit_phase(onsets: &[(f64, u8)], bpm: f64) -> Option<TempoGuess> {
    let first = onsets.first()?.0;
    let beat = 60.0 / bpm;
    let (mut re, mut im, mut w) = (0.0, 0.0, 0.0);
    for (t, v) in onsets {
        let a = std::f64::consts::TAU * t / beat;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
        w += *v as f64;
    }
    let grid0 = im.atan2(re) / std::f64::consts::TAU * beat;
    let k = ((first - grid0) / beat).round();
    Some(TempoGuess { bpm, downbeat: grid0 + k * beat, confidence: ((re * re + im * im).sqrt() / w).clamp(0.0, 1.0) })
}

/// Onsets (time, velocity) of the notes in `events`.
pub fn onsets(events: &[RawEvent]) -> Vec<(f64, u8)> {
    events
        .iter()
        .filter_map(|e| match e.msg {
            RawMsg::NoteOn { velocity, .. } if velocity > 0 => Some((e.t, velocity)),
            _ => None,
        })
        .collect()
}

/// What an agent (or a list in the UI) needs to tell jams apart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JamSummary {
    /// Unix seconds of the first event; also the jam's id.
    pub id: u64,
    pub start: f64,
    pub end: f64,
    pub notes: usize,
    pub lowest: u8,
    pub highest: u8,
    pub mean_velocity: u8,
    pub tempo: Option<TempoGuess>,
    pub key: Option<String>,
    /// The chords it moved through, in order, repeats collapsed. Only
    /// meaningful when there is a tempo.
    pub chords: Vec<String>,
    pub sustain_pedal: bool,
}

impl JamSummary {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// The notes of a jam as ticks on a grid, using its tempo guess (or 120 if
/// there is none). Time zero is the jam's first event.
pub fn jam_notes(events: &[RawEvent], tempo: Option<&TempoGuess>) -> Vec<Note> {
    let Some(t0) = events.first().map(|e| e.t) else { return Vec::new() };
    let s = to_session(events, t0, f64::INFINITY, Id(0), String::new(), tempo, MeterChange::default_four(), 0);
    let bpm = tempo.map_or(120.0, |t| t.bpm);
    let imported = crate::cleanup::import_session(&s, &TempoMap::constant(bpm), true);
    imported.notes
}

pub fn summarize(events: &[RawEvent]) -> Option<JamSummary> {
    let start = events.first()?.t;
    let end = events.last()?.t;
    let onsets: Vec<(f64, u8)> = events
        .iter()
        .filter_map(|e| match e.msg {
            RawMsg::NoteOn { velocity, .. } if velocity > 0 => Some((e.t - start, velocity)),
            _ => None,
        })
        .collect();
    if onsets.len() < MIN_NOTES {
        return None;
    }
    let pitches: Vec<u8> = events
        .iter()
        .filter_map(|e| match e.msg {
            RawMsg::NoteOn { pitch, velocity } if velocity > 0 => Some(pitch),
            _ => None,
        })
        .collect();
    let tempo = estimate_tempo(&onsets).map(|t| TempoGuess { downbeat: t.downbeat + start, ..t });
    let notes = jam_notes(events, tempo.as_ref());
    let key = theory::detect_key(&notes).map(|(k, _)| k.name().to_string());
    let spell = Spelling::Mixed;
    let mut chords: Vec<String> = Vec::new();
    if tempo.is_some_and(|t| t.confidence >= 0.3) {
        let bar = MeterChange::default_four();
        for f in figure::analyze(&notes, &bar, &FigureSettings::default()) {
            let name = f.chord.name(spell);
            if chords.last() != Some(&name) {
                chords.push(name);
            }
            if chords.len() >= 24 {
                break;
            }
        }
    }
    Some(JamSummary {
        id: start as u64,
        start,
        end,
        notes: onsets.len(),
        lowest: *pitches.iter().min()?,
        highest: *pitches.iter().max()?,
        mean_velocity: (onsets.iter().map(|(_, v)| *v as u32).sum::<u32>() / onsets.len() as u32) as u8,
        tempo,
        key,
        chords,
        sustain_pedal: events.iter().any(|e| matches!(e.msg, RawMsg::Cc { controller: 64, value } if value >= 64)),
    })
}

/// Notes per window of `window` seconds across the jam: where it got busy
/// (or loud) is often where the idea is.
pub fn activity(events: &[RawEvent], window: f64) -> Vec<(f64, usize, u8)> {
    let Some(start) = events.first().map(|e| e.t) else { return Vec::new() };
    let end = events.last().map_or(start, |e| e.t);
    let n = ((end - start) / window).ceil().max(1.0) as usize;
    let mut out: Vec<(f64, usize, u32)> = (0..n).map(|i| (i as f64 * window, 0, 0)).collect();
    for e in events {
        if let RawMsg::NoteOn { velocity, .. } = e.msg
            && velocity > 0
        {
            let i = (((e.t - start) / window) as usize).min(n - 1);
            out[i].1 += 1;
            out[i].2 += velocity as u32;
        }
    }
    out.into_iter().map(|(t, c, v)| (t, c, if c > 0 { (v / c as u32) as u8 } else { 0 })).collect()
}

/// Turns `from..to` (seconds, the events' clock) of a jam into a session
/// whose downbeat lands on `start_tick`. With a tempo guess the take sits
/// on that grid; without one it is laid down at 120 BPM as played.
#[allow(clippy::too_many_arguments)]
pub fn to_session(
    events: &[RawEvent],
    from: f64,
    to: f64,
    id: Id,
    name: String,
    tempo: Option<&TempoGuess>,
    meter: MeterChange,
    start_tick: Tick,
) -> Session {
    let slice: Vec<RawEvent> = events
        .iter()
        .filter(|e| e.t >= from && e.t <= to)
        .map(|e| RawEvent { t: e.t - from, ..*e })
        .collect();
    // The take starts at `from`; its first downbeat is the grid beat
    // nearest the first note.
    let downbeat = match tempo {
        Some(t) => {
            let beat = 60.0 / t.bpm;
            let first = slice.first().map_or(0.0, |e| e.t);
            let rel = t.downbeat - from;
            rel + ((first - rel) / beat).round() * beat
        }
        None => slice.first().map_or(0.0, |e| e.t),
    };
    Session {
        id,
        name,
        recorded_at: from as u64,
        click_bpm: tempo.map(|t| t.bpm),
        meter,
        downbeat_offset: downbeat.max(0.0),
        start_tick,
        own_tempo: tempo.is_some(),
        events: slice,
    }
}

impl MeterChange {
    pub fn default_four() -> Self {
        MeterChange { tick: 0, numerator: 4, denominator: 4 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A free-time performance: eighth notes at `bpm` starting at unix-ish
    /// time `t0`, with deterministic jitter, accents on beats.
    fn played(bpm: f64, t0: f64, bars: usize, jitter: f64) -> Vec<RawEvent> {
        let eighth = 30.0 / bpm;
        let chord = [[57u8, 60, 64, 69], [53, 57, 60, 65], [48, 52, 55, 60], [55, 59, 62, 67]];
        let mut x = 0.37f64;
        let mut ev = Vec::new();
        for i in 0..bars * 8 {
            x = (x * 7.31 + 0.13).fract();
            let t = t0 + i as f64 * eighth + (x - 0.5) * 2.0 * jitter;
            let pitch = chord[(i / 8) % 4][[0, 1, 2, 3, 2, 1, 2, 1][i % 8]];
            let velocity = if i % 2 == 0 { 96 } else { 72 };
            ev.push(RawEvent { t, channel: 0, msg: RawMsg::NoteOn { pitch, velocity } });
            ev.push(RawEvent { t: t + eighth * 0.8, channel: 0, msg: RawMsg::NoteOff { pitch } });
        }
        ev.sort_by(|a, b| a.t.total_cmp(&b.t));
        ev
    }

    fn onsets(ev: &[RawEvent]) -> Vec<(f64, u8)> {
        ev.iter()
            .filter_map(|e| match e.msg {
                RawMsg::NoteOn { velocity, .. } => Some((e.t, velocity)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn finds_the_tempo_of_free_playing() {
        for bpm in [72.0, 96.0, 118.0, 140.0] {
            let ev = played(bpm, 1_000.0, 8, 0.015);
            let g = estimate_tempo(&onsets(&ev)).unwrap();
            assert!((g.bpm - bpm).abs() < 1.5, "{bpm}: {g:?}");
            assert!(g.confidence > 0.5, "{bpm}: {g:?}");
            // The downbeat is on the first note, give or take the jitter.
            assert!((g.downbeat - 1_000.0).abs() < 0.03, "{bpm}: {g:?}");
        }
    }

    #[test]
    fn random_noodling_is_low_confidence() {
        let mut x = 0.5f64;
        let ev: Vec<(f64, u8)> = (0..60)
            .map(|i| {
                x = (x * 9.17 + 0.29).fract();
                (i as f64 * 0.31 + x * 0.29, 80)
            })
            .collect();
        let g = estimate_tempo(&ev).unwrap();
        assert!(g.confidence < 0.45, "{g:?}");
    }

    #[test]
    fn summary_reads_key_and_chords() {
        let ev = played(100.0, 5_000.0, 8, 0.012);
        let s = summarize(&ev).unwrap();
        assert_eq!(s.id, 5_000);
        assert_eq!(s.notes, 64);
        assert!((s.tempo.unwrap().bpm - 100.0).abs() < 1.5);
        assert_eq!(s.key.as_deref(), Some("C"));
        assert_eq!(&s.chords[..4], ["Am", "F", "C", "G"], "{s:?}");
    }

    #[test]
    fn a_stretch_becomes_a_session_on_the_grid() {
        let ev = played(100.0, 5_000.0, 8, 0.01);
        let s = summarize(&ev).unwrap();
        // Bars 3-4 of the jam (seconds 4.8-9.6 at 100 BPM), placed at bar 9.
        let from = 5_000.0 + 4.8 - 0.05;
        let session = to_session(&ev, from, from + 4.8, Id(7), "Jam".into(), s.tempo.as_ref(), MeterChange::default_four(), 8 * 3840);
        let notes = crate::cleanup::import_session(&session, &TempoMap::constant(100.0), false).notes;
        assert_eq!(notes.len(), 16);
        // The first note (the C chord's root) lands on the downbeat.
        assert!(notes[0].start < 40, "{:?}", &notes[..2]);
        assert_eq!(notes[0].pitch, 48);
    }
}
