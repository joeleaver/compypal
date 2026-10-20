//! Jams and sketches: playing caught by the always-on journal.
//!
//! A jam is a stretch of playing between long silences; it is cut into
//! sketches, one idea each, at shorter pauses and where the music changes.
//! Sketches are what people see. Nothing here touches files or clocks.
//! Events carry absolute times in seconds (unix time in the journal). This
//! module finds the beat in free playing, summarises a sketch so it can be
//! told apart without reading it, and turns one into a [`Session`] that
//! lands on the timeline like any recorded take.

use serde::{Deserialize, Serialize};

use crate::figure::{self, FigureSettings};
use crate::model::{Id, MeterChange, Note, TempoMap, Tick};
use crate::session::{RawEvent, RawMsg, Session};
use crate::theory::{self, Spelling};

/// A silence this long ends a jam.
pub const GAP_SECONDS: f64 = 8.0;
/// Fewer notes than this is a stray touch, not a jam.
pub const MIN_NOTES: usize = 6;
/// A pause this long between notes starts a new sketch.
pub const SKETCH_PAUSE: f64 = 2.5;
/// A sketch longer than this is looked at for a change of key or tempo
/// at a phrase break.
const LONG_SKETCH: f64 = 40.0;
/// A pause this long is a phrase break: somewhere a long sketch may split.
const PHRASE_BREAK: f64 = 0.6;

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
    let (_, coarse, grid) = best?;
    // The beat grid's phase, from all the notes; the downbeat is the beat
    // nearest the first note of at least average loudness.
    let (_, phase) = align(60.0 / coarse, &|v| v as f64, onsets.iter().map(|(_, v)| *v as f64).sum());
    let (bpm, grid0) = refine(onsets, coarse, phase / std::f64::consts::TAU * 60.0 / coarse);
    let beat = 60.0 / bpm;
    let first = onsets.iter().find(|(_, v)| *v as f64 >= mean_v).unwrap_or(&onsets[0]).0;
    let k = ((first - grid0) / beat).round();
    Some(TempoGuess { bpm, downbeat: grid0 + k * beat, confidence: grid.clamp(0.0, 1.0) })

}

/// Sharpens a tempo found on a coarse search: puts each note on its nearest
/// eighth of that grid and fits a line through (eighth, time) by least
/// squares. Over a long take a fraction of a BPM is the difference between
/// sitting on the grid and drifting off it at both ends. Returns the BPM
/// and a time where a beat falls; keeps the coarse answer if the fit
/// wanders off it.
fn refine(onsets: &[(f64, u8)], bpm: f64, beat0: f64) -> (f64, f64) {
    let (mut step, mut zero) = (30.0 / bpm, beat0);
    for _ in 0..3 {
        let pts: Vec<(f64, f64)> = onsets.iter().map(|(t, _)| (((t - zero) / step).round(), *t)).collect();
        let n = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
        if sxx <= 0.0 {
            break;
        }
        let slope = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / sxx;
        if !(slope > 0.0) || (slope / step - 1.0).abs() > 0.03 {
            return (bpm, beat0);
        }
        // An eighth-note line through the notes; a beat is every other one,
        // so keep the one nearest the coarse beat.
        let line0 = my - slope * mx;
        let k = ((beat0 - line0) / (2.0 * slope)).round();
        (step, zero) = (slope, line0 + k * 2.0 * slope);
    }
    (30.0 / step, zero)
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

/// A sketch: one idea out of a stretch of playing, summed up so an agent
/// (or a list in the UI) can tell sketches apart without reading them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sketch {
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

impl Sketch {
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

pub fn summarize(events: &[RawEvent]) -> Option<Sketch> {
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
    Some(Sketch {
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

/// A note as played: start and end in the events' clock, pitch, velocity.
#[derive(Clone, Copy, Debug)]
struct Played {
    start: f64,
    end: f64,
    pitch: u8,
    velocity: u8,
}

/// Notes as played, from the raw stream.
fn played_notes(events: &[RawEvent]) -> Vec<Played> {
    let mut open: Vec<Played> = Vec::new();
    let mut out = Vec::new();
    for e in events {
        match e.msg {
            RawMsg::NoteOn { pitch, velocity } if velocity > 0 => open.push(Played { start: e.t, end: e.t, pitch, velocity }),
            RawMsg::NoteOn { pitch, .. } | RawMsg::NoteOff { pitch } => {
                if let Some(i) = open.iter().position(|n| n.pitch == pitch) {
                    out.push(Played { end: e.t, ..open.remove(i) });
                }
            }
            _ => {}
        }
    }
    let last = events.last().map_or(0.0, |e| e.t);
    out.extend(open.into_iter().map(|n| Played { end: last, ..n }));
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

fn end_of(piece: &[Played]) -> f64 {
    piece.iter().map(|n| n.end).fold(f64::MIN, f64::max)
}

/// Where the ideas in a stretch of playing begin and end, in its clock:
/// cut at pauses of [`SKETCH_PAUSE`], and long stretches again at a phrase
/// break where the key or the tempo changes. Scraps too small to be an idea
/// join the sketch nearest them.
pub fn split_sketches(events: &[RawEvent]) -> Vec<(f64, f64)> {
    let notes = played_notes(events);
    if notes.is_empty() {
        return Vec::new();
    }
    let mut pieces: Vec<Vec<Played>> = vec![Vec::new()];
    let mut until = f64::NEG_INFINITY;
    for n in &notes {
        if n.start - until >= SKETCH_PAUSE && !pieces.last().unwrap().is_empty() {
            pieces.push(Vec::new());
        }
        pieces.last_mut().unwrap().push(*n);
        until = until.max(n.end);
    }
    let mut out: Vec<Vec<Played>> = Vec::new();
    for p in pieces {
        split_long(p, &mut out);
    }
    // Scraps join a neighbour, whichever is closer in time.
    while out.len() > 1 {
        let Some(i) = out.iter().position(|p| p.len() < MIN_NOTES) else { break };
        let before = (i > 0).then(|| out[i][0].start - end_of(&out[i - 1]));
        let after = out.get(i + 1).map(|next| next[0].start - end_of(&out[i]));
        let scrap = out.remove(i);
        let into = match (before, after) {
            (Some(b), Some(a)) if a < b => i,
            (Some(_), _) => i - 1,
            _ => i,
        };
        out[into].extend(scrap);
        out[into].sort_by(|a, b| a.start.total_cmp(&b.start));
    }
    out.into_iter().filter(|p| !p.is_empty()).map(|p| (p[0].start, end_of(&p))).collect()
}

/// Splits a long piece at the phrase break where the music changes most,
/// and again in each half, until no break changes anything.
fn split_long(piece: Vec<Played>, out: &mut Vec<Vec<Played>>) {
    let length = end_of(&piece) - piece.first().map_or(0.0, |n| n.start);
    if length < LONG_SKETCH {
        out.push(piece);
        return;
    }
    let mut until = f64::NEG_INFINITY;
    let mut best: Option<(usize, f64)> = None;
    for i in 1..piece.len() {
        until = until.max(piece[i - 1].end);
        // Each side needs enough music to say what key and tempo it's in.
        if piece[i].start - until < PHRASE_BREAK || i < 16 || piece.len() - i < 16 {
            continue;
        }
        let score = change(&piece[..i], &piece[i..]);
        if score > 0.0 && best.is_none_or(|b| score > b.1) {
            best = Some((i, score));
        }
    }
    match best {
        Some((i, _)) => {
            let mut piece = piece;
            let after = piece.split_off(i);
            split_long(piece, out);
            split_long(after, out);
        }
        None => out.push(piece),
    }
}

/// How differently two stretches go: a change of key counts 1, a change of
/// tempo by more than 8% counts by how much. Zero when they're alike.
fn change(a: &[Played], b: &[Played]) -> f64 {
    let key = |side: &[Played]| {
        let notes: Vec<Note> = side
            .iter()
            .map(|n| Note { pitch: n.pitch, velocity: n.velocity, start: 0, duration: ((n.end - n.start) * 960.0).max(1.0) as Tick })
            .collect();
        theory::detect_key(&notes).filter(|(_, confidence)| *confidence >= 0.6).map(|(k, _)| k.name().to_string())
    };
    let tempo = |side: &[Played]| {
        let ons: Vec<(f64, u8)> = side.iter().map(|n| (n.start, n.velocity)).collect();
        estimate_tempo(&ons).filter(|t| t.confidence >= 0.4)
    };
    let mut score = 0.0;
    if let (Some(x), Some(y)) = (key(a), key(b))
        && x != y
    {
        score += 1.0;
    }
    if let (Some(x), Some(y)) = (tempo(a), tempo(b)) {
        let r = (x.bpm / y.bpm).max(y.bpm / x.bpm);
        if r > 1.08 {
            score += r - 1.0;
        }
    }
    score
}

/// A name for a sketch, from its music: "Am F C G loop", "C G Am F…",
/// "Free playing in A minor".
pub fn sketch_name(s: &Sketch) -> String {
    let key = s.key.as_deref().map(|k| format!(" in {k}")).unwrap_or_default();
    if s.tempo.is_none_or(|t| t.confidence < 0.3) || s.chords.is_empty() {
        return format!("Free playing{key}");
    }
    let c = &s.chords;
    // The shortest run of chords that the rest just repeats.
    for period in 1..=4.min(c.len() / 2) {
        if (0..c.len()).all(|i| c[i] == c[i % period]) {
            return format!("{} loop", c[..period].join(" "));
        }
    }
    let head = c.iter().take(4).cloned().collect::<Vec<_>>().join(" ");
    if c.len() > 4 { format!("{head}…") } else { head }
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
            let k = (first - rel) / beat;
            // Without accents the beat can't be told from the off-beat. A
            // sketch starts where the player starts, so when the first note
            // sits on an off-beat of the grid found, the beat moves to it.
            if (k - k.round()).abs() > 0.35 { rel + (k.floor() + 0.5) * beat } else { rel + k.round() * beat }
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

    #[test]
    fn a_long_steady_take_gets_an_exact_tempo() {
        // 21 s of eighths at 77.3 BPM, a little uneven, accents on the beat.
        let eighth = 30.0 / 77.3;
        let ons: Vec<(f64, u8)> = (0..56)
            .map(|i| {
                let wobble = ((i * 37) % 11) as f64 / 11.0 - 0.5;
                (1.234 + i as f64 * eighth + wobble * 0.012, if i % 2 == 0 { 96 } else { 70 })
            })
            .collect();
        let g = estimate_tempo(&ons).unwrap();
        assert!((g.bpm - 77.3).abs() < 0.05, "{}", g.bpm);
        // The downbeat lands on a played beat, to within a few ms.
        let off = ((g.downbeat - 1.234) / (2.0 * eighth)).fract().abs();
        assert!(off.min(1.0 - off) * 2.0 * eighth < 0.006, "downbeat {} off by {off}", g.downbeat);
    }

    fn transposed(ev: Vec<RawEvent>, by: i8) -> Vec<RawEvent> {
        ev.into_iter()
            .map(|e| RawEvent {
                msg: match e.msg {
                    RawMsg::NoteOn { pitch, velocity } => RawMsg::NoteOn { pitch: (pitch as i8 + by) as u8, velocity },
                    RawMsg::NoteOff { pitch } => RawMsg::NoteOff { pitch: (pitch as i8 + by) as u8 },
                    m => m,
                },
                ..e
            })
            .collect()
    }

    #[test]
    fn pauses_split_sketches_and_scraps_join_one() {
        let mut ev = played(100.0, 0.0, 4, 0.01); // 9.6 s
        // Two stray notes just after, then a pause and a second idea.
        ev.push(RawEvent { t: 10.2, channel: 0, msg: RawMsg::NoteOn { pitch: 60, velocity: 50 } });
        ev.push(RawEvent { t: 10.3, channel: 0, msg: RawMsg::NoteOff { pitch: 60 } });
        ev.extend(played(100.0, 16.0, 4, 0.01));
        ev.sort_by(|a, b| a.t.total_cmp(&b.t));
        let s = split_sketches(&ev);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[0].1 >= 10.3 && s[1].0 >= 16.0 - 0.02, "{s:?}");
    }

    #[test]
    fn a_long_stretch_splits_where_the_key_changes() {
        // 40 s in A minor, a breath, then 40 s a whole step up: no pause
        // long enough to split on its own.
        let mut ev = played(96.0, 0.0, 16, 0.01);
        let second = transposed(played(96.0, 41.0, 16, 0.01), 2);
        ev.extend(second);
        ev.sort_by(|a, b| a.t.total_cmp(&b.t));
        let s = split_sketches(&ev);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!((s[1].0 - 41.0).abs() < 0.05, "{s:?}");
        // The same playing without the change stays one sketch.
        let mut same = played(96.0, 0.0, 16, 0.01);
        same.extend(played(96.0, 41.0, 16, 0.01));
        same.sort_by(|a, b| a.t.total_cmp(&b.t));
        assert_eq!(split_sketches(&same).len(), 1);
    }

    #[test]
    fn sketches_are_named_by_their_music() {
        let s = summarize(&played(100.0, 0.0, 8, 0.01)).unwrap();
        assert_eq!(sketch_name(&s), "Am F C G loop", "{s:?}");
        let free = Sketch { tempo: None, key: Some("A minor".into()), ..s.clone() };
        assert_eq!(sketch_name(&free), "Free playing in A minor");
        let long = Sketch { chords: ["C", "G", "Am", "F", "Dm", "G"].map(String::from).to_vec(), ..s };
        assert_eq!(sketch_name(&long), "C G Am F…");
    }

    #[test]
    fn a_sketch_starts_on_its_first_note_when_the_beat_is_unclear() {
        // Even eighths, no accents: the grid found may put the beat on the
        // off-beats. Half a beat out on purpose.
        let ev = played(96.0, 10.0, 4, 0.0);
        let half = 30.0 / 96.0;
        let off = TempoGuess { bpm: 96.0, downbeat: 10.0 + half, confidence: 0.9 };
        let s = to_session(&ev, 10.0, 30.0, Id(1), "x".into(), Some(&off), MeterChange::default_four(), 0);
        assert!(s.downbeat_offset.abs() < 1e-6, "{}", s.downbeat_offset);
        // A grid that's right stays right.
        let on = TempoGuess { downbeat: 10.0, ..off };
        let s = to_session(&ev, 10.0, 30.0, Id(1), "x".into(), Some(&on), MeterChange::default_four(), 0);
        assert!(s.downbeat_offset.abs() < 1e-6);
    }
}
