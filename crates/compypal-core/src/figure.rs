//! Figures: the units you'd name when talking about music.
//!
//! "A C arpeggio, then a run up into G, then stabs on Am." A figure is one
//! harmonic idea with one shape: a block chord (or repeated stabs), an
//! arpeggio, a scalar run, a melodic fragment, a single note, or a mix such
//! as a chord with a line over it. Figures are derived from notes, never
//! stored: change the notes and the figures follow.
//!
//! Segmentation runs in two steps. Notes whose onsets fall within
//! `chord_window` of each other merge into one event (a chord as actually
//! played, with human spread). A dynamic program then splits the events
//! into figures, trading how well each span fits a single chord against a
//! fixed cost per figure. Silences longer than `split_gap` always split.
//!
//! Editing works on roles: each note is a chord tone (root, third, fifth,
//! seventh, ...) or a passing tone. Changing the chord moves every note to
//! the same role in the new chord, near where it was, keeping the contour.

use serde::Serialize;

use crate::model::{Id, KeySignature, MeterChange, Note, PPQ, Project, Tick};
use crate::theory::{self, Chord, ChordFit, Spelling};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct FigureSettings {
    /// Onsets this close together are one chord.
    pub chord_window: Tick,
    /// A silence this long always ends a figure.
    pub split_gap: Tick,
    /// No figure spans more than this.
    pub max_span: Tick,
    /// What starting a new figure costs, in tick-weighted misfit. Higher
    /// gives fewer, longer figures.
    pub change_cost: f64,
}

impl Default for FigureSettings {
    fn default() -> Self {
        let q = PPQ as Tick;
        Self { chord_window: q / 8, split_gap: q, max_span: q * 16, change_cost: q as f64 / 2.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FigureKind {
    /// Notes struck together, once or repeated.
    Block,
    /// Chord tones one at a time.
    Arpeggio,
    /// Mostly stepwise motion.
    Run,
    /// A single line that is neither of the above.
    Melody,
    Note,
    /// Chords and single notes together, like a chord with a line over it.
    Mixed,
}

impl FigureKind {
    pub fn label(&self) -> &'static str {
        match self {
            FigureKind::Block => "block",
            FigureKind::Arpeggio => "arpeggio",
            FigureKind::Run => "run",
            FigureKind::Melody => "melody",
            FigureKind::Note => "note",
            FigureKind::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Figure {
    pub start: Tick,
    /// Where the last note stops sounding.
    pub end: Tick,
    pub kind: FigureKind,
    pub chord: Chord,
    /// How well `chord` explains the notes, about 0 to 1.
    pub confidence: f64,
    /// Other readings of the same notes, best first.
    pub alternatives: Vec<Chord>,
    /// The figure's notes in absolute ticks, sorted.
    pub notes: Vec<Note>,
}

/// Groups notes whose onsets fall within `window` of the group's first.
fn events(notes: &[Note], window: Tick) -> Vec<Vec<Note>> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.pitch));
    let mut out: Vec<Vec<Note>> = Vec::new();
    for n in sorted {
        match out.last_mut() {
            Some(ev) if n.start - ev[0].start <= window => ev.push(n),
            _ => out.push(vec![n]),
        }
    }
    out
}

/// How much each event counts toward the harmony. A single note reached and
/// left by step is a passing tone and counts a quarter; everything else
/// counts fully.
fn event_weights(evs: &[Vec<Note>]) -> Vec<f64> {
    let line = |i: usize| evs.get(i).filter(|e| e.len() == 1).map(|e| e[0].pitch as i16);
    let step = |a: Option<i16>, b: Option<i16>| matches!((a, b), (Some(a), Some(b)) if matches!((a - b).abs(), 1 | 2));
    (0..evs.len())
        .map(|i| {
            let here = line(i);
            let prev = i.checked_sub(1).and_then(line);
            if here.is_some() && step(prev, here) && step(here, line(i + 1)) { 0.25 } else { 1.0 }
        })
        .collect()
}

fn fit(weights: &[f64; 12], bass: Option<u8>) -> ChordFit {
    theory::best_fit(weights, bass).expect("non-empty histogram always has a fit")
}

/// How much more it costs to start a figure at `t` than on a barline:
/// figures want to begin where the music is strong.
fn metric_cost(t: Tick, meter: &MeterChange, window: Tick) -> f64 {
    let rel = t.saturating_sub(meter.tick);
    let near = |unit: Tick| {
        let off = rel % unit;
        off.min(unit - off) <= window
    };
    if near(meter.ticks_per_bar()) {
        1.0
    } else if near(meter.ticks_per_beat()) {
        1.5
    } else if near(meter.ticks_per_beat() / 2) {
        2.0
    } else {
        3.0
    }
}

pub fn analyze(notes: &[Note], meter: &MeterChange, s: &FigureSettings) -> Vec<Figure> {
    let evs = events(notes, s.chord_window);
    let n = evs.len();
    if n == 0 {
        return Vec::new();
    }
    let factor = event_weights(&evs);
    // Silence before each event: its start minus the latest end so far.
    let mut gap_before = vec![0u64; n];
    let mut sounding_until = 0;
    for (i, ev) in evs.iter().enumerate() {
        gap_before[i] = ev[0].start.saturating_sub(sounding_until);
        sounding_until = sounding_until.max(ev.iter().map(|n| n.end()).max().unwrap());
    }

    // best[j]: cheapest split of events[..j]; from[j]: where its last
    // figure starts.
    let mut best = vec![f64::INFINITY; n + 1];
    let mut from = vec![0usize; n + 1];
    best[0] = 0.0;
    for j in 1..=n {
        let mut weights = [0.0f64; 12];
        let mut lowest = 127u8;
        let mut total = 0.0;
        for i in (0..j).rev() {
            if i + 1 < j && gap_before[i + 1] > s.split_gap {
                break;
            }
            if evs[j - 1][0].start - evs[i][0].start > s.max_span {
                break;
            }
            for note in &evs[i] {
                let w = note.duration.max(1) as f64 * factor[i];
                weights[(note.pitch % 12) as usize] += w;
                total += w;
                lowest = lowest.min(note.pitch);
            }
            let f = fit(&weights, Some(lowest % 12));
            let start_cost = s.change_cost * metric_cost(evs[i][0].start, meter, s.chord_window);
            let cost = best[i] + (1.0 - f.score) * total + start_cost;
            if cost < best[j] {
                best[j] = cost;
                from[j] = i;
            }
        }
    }

    let mut bounds = Vec::new();
    let mut j = n;
    while j > 0 {
        bounds.push((from[j], j));
        j = from[j];
    }
    bounds.reverse();
    bounds.into_iter().map(|(i, j)| build(&evs[i..j], &factor[i..j])).collect()
}

fn build(evs: &[Vec<Note>], factor: &[f64]) -> Figure {
    let notes: Vec<Note> = evs.iter().flatten().copied().collect();
    let mut weights = [0.0f64; 12];
    for (ev, f) in evs.iter().zip(factor) {
        for n in ev {
            weights[(n.pitch % 12) as usize] += n.duration.max(1) as f64 * f;
        }
    }
    let bass = notes.iter().map(|n| n.pitch).min().map(|p| p % 12);
    let ranked = theory::identify_weights(&weights, bass);
    let chord = ranked[0].chord;
    let mut alternatives: Vec<Chord> = Vec::new();
    for f in ranked.iter().skip(1) {
        if alternatives.len() == 4 || f.score < ranked[0].score - 0.15 {
            break;
        }
        if f.chord != chord && !alternatives.contains(&f.chord) {
            alternatives.push(f.chord);
        }
    }
    Figure {
        start: notes.iter().map(|n| n.start).min().unwrap(),
        end: notes.iter().map(|n| n.end()).max().unwrap(),
        kind: classify(evs, &chord),
        chord,
        confidence: ranked[0].score.clamp(0.0, 1.0),
        alternatives,
        notes,
    }
}

fn classify(evs: &[Vec<Note>], chord: &Chord) -> FigureKind {
    let blocks = evs.iter().filter(|e| e.len() > 1).count();
    if blocks == evs.len() {
        return FigureKind::Block;
    }
    if blocks > 0 {
        return FigureKind::Mixed;
    }
    if evs.len() == 1 {
        return FigureKind::Note;
    }
    let pitches: Vec<i16> = evs.iter().map(|e| e[0].pitch as i16).collect();
    let steps = pitches.windows(2).filter(|w| matches!((w[1] - w[0]).abs(), 1 | 2)).count();
    let all_chord_tones = pitches.iter().all(|&p| chord.contains(p as u8));
    if pitches.len() >= 3 && steps * 10 >= (pitches.len() - 1) * 7 {
        FigureKind::Run
    } else if pitches.len() >= 3 && all_chord_tones {
        FigureKind::Arpeggio
    } else {
        FigureKind::Melody
    }
}

/// Moves `notes` from `from` to `to`, keeping each note's role and staying
/// near its old pitch. Passing tones follow the root and are nudged back
/// into `key` if the move pushed them out.
pub fn revoice(notes: &[Note], from: &Chord, to: &Chord, key: KeySignature) -> Vec<Note> {
    let to_pcs: Vec<u8> = to.pcs().collect();
    let shift = (to.root as i16 - from.root as i16).rem_euclid(12);
    // Each note's role in the new chord; None for passing tones.
    let mut roles: Vec<Option<usize>> =
        notes.iter().map(|n| from.role_of(n.pitch % 12).map(|r| r % to_pcs.len())).collect();
    let recast = fill_missing_roles(notes, &mut roles, to_pcs.len());

    let mut out: Vec<Note> = notes
        .iter()
        .zip(&roles)
        .enumerate()
        .map(|(i, (n, role))| {
            let pitch = match role {
                // A note that gave up its role for a new one lands next to
                // where its old role would have put it: the octave root
                // becomes the seventh just below it.
                Some(r) if recast[i] => {
                    let old_role = from.role_of(n.pitch % 12).unwrap_or(0) % to_pcs.len();
                    nearest(nearest(n.pitch, to_pcs[old_role]), to_pcs[*r])
                }
                Some(r) => nearest(n.pitch, to_pcs[*r]),
                None => {
                    let pc = n.pitch % 12;
                    let moved = ((pc as i16 + shift) % 12) as u8;
                    let moved = if key.contains(pc) && !key.contains(moved) {
                        if key.contains((moved + 11) % 12) { (moved + 11) % 12 } else { (moved + 1) % 12 }
                    } else {
                        moved
                    };
                    nearest(n.pitch, moved)
                }
            };
            Note { pitch, ..*n }
        })
        .collect();

    keep_contour(notes, &mut out);

    if let Some(b) = to.bass {
        // Each event's lowest note becomes the slash bass, under the rest.
        for ev in events(&out.clone(), 0) {
            let low = ev.iter().min_by_key(|n| n.pitch).unwrap();
            let next = ev.iter().map(|n| n.pitch).filter(|&p| p != low.pitch).min();
            let mut p = nearest(low.pitch, b);
            if let Some(next) = next {
                while p >= next && p >= 12 {
                    p -= 12;
                }
            }
            if let Some(o) = out.iter_mut().find(|o| o.start == low.start && o.pitch == low.pitch) {
                o.pitch = p;
            }
        }
    }

    out.sort_by_key(|n| (n.start, n.pitch));
    out.dedup_by_key(|n| (n.start, n.pitch));
    out
}

/// Gives the new chord's tones that no note plays (its seventh, say, when
/// the old chord was a triad) to notes that can spare their role: the
/// highest doubled root first, then a doubled fifth, then the only fifth,
/// which seventh chords commonly drop. Every note at a chosen pitch moves
/// together, so a repeating figure changes the same way each time round.
/// Returns which notes were recast.
fn fill_missing_roles(notes: &[Note], roles: &mut [Option<usize>], size: usize) -> Vec<bool> {
    let mut recast = vec![false; notes.len()];
    for want in (0..size).filter(|&r| r != 2) {
        if roles.contains(&Some(want)) {
            continue;
        }
        let pitches_with = |roles: &[Option<usize>], role: usize| {
            let mut ps: Vec<u8> =
                notes.iter().zip(roles.iter()).filter(|(_, r)| **r == Some(role)).map(|(n, _)| n.pitch).collect();
            ps.sort_unstable();
            ps.dedup();
            ps
        };
        let roots = pitches_with(roles, 0);
        let fifths = pitches_with(roles, 2);
        let donor = if roots.len() > 1 {
            roots.last().copied()
        } else if fifths.len() > 1 || want >= 3 {
            fifths.last().copied()
        } else {
            None
        };
        let Some(pitch) = donor else { continue };
        for (i, n) in notes.iter().enumerate() {
            if n.pitch == pitch && !recast[i] {
                roles[i] = Some(want);
                recast[i] = true;
            }
        }
    }
    recast
}

/// The pitch with class `pc` closest to `pitch`, ties going down.
fn nearest(pitch: u8, pc: u8) -> u8 {
    let up = (pc as i16 - (pitch % 12) as i16).rem_euclid(12);
    let p = pitch as i16 + if up <= 6 { up } else { up - 12 };
    p.clamp(0, 127) as u8
}

/// Where the old line went up, the new one goes up too: a note that moved
/// against the contour jumps an octave if that keeps it within a sixth of
/// where it was.
fn keep_contour(old: &[Note], new: &mut [Note]) {
    // Only single-note lines have a contour; chords keep their voicing.
    let mut order: Vec<usize> = (0..old.len()).collect();
    order.sort_by_key(|&i| (old[i].start, old[i].pitch));
    let line: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| old.iter().filter(|o| o.start == old[i].start).count() == 1)
        .collect();
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let was = old[b].pitch as i16 - old[a].pitch as i16;
        let is = new[b].pitch as i16 - new[a].pitch as i16;
        if was == 0 || is == 0 || was.signum() == is.signum() {
            continue;
        }
        let fixed = new[b].pitch as i16 + 12 * was.signum();
        if (fixed - old[b].pitch as i16).abs() <= 9 && (0..=127).contains(&fixed) {
            new[b].pitch = fixed as u8;
        }
    }
}

/// How far after a figure's start the next one begins: up to the next
/// figure if there is one, else the figure rounded up to whole beats.
pub fn span(figures: &[Figure], index: usize, meter: &MeterChange) -> Tick {
    let f = &figures[index];
    match figures.get(index + 1) {
        Some(next) => next.start - f.start,
        None => {
            let beat = meter.ticks_per_beat();
            (f.end - f.start).div_ceil(beat).max(1) * beat
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FigureError {
    #[error("no track {0}")]
    NoTrack(Id),
    #[error("figure {index} out of range: the track has {count}")]
    NoFigure { index: usize, count: usize },
}

fn track_figures(p: &Project, track: Id, s: &FigureSettings) -> Result<Vec<Figure>, FigureError> {
    let t = p.track(track).ok_or(FigureError::NoTrack(track))?;
    Ok(analyze(&t.absolute_notes(), &p.meter_at(0), s))
}

fn figure_at(figs: &[Figure], index: usize) -> Result<&Figure, FigureError> {
    figs.get(index).ok_or(FigureError::NoFigure { index, count: figs.len() })
}

/// Re-voices figure `index` on `track` to `chord`.
pub fn set_chord(
    p: &mut Project,
    track: Id,
    index: usize,
    chord: &Chord,
    s: &FigureSettings,
) -> Result<(), FigureError> {
    let figs = track_figures(p, track, s)?;
    let f = figure_at(&figs, index)?;
    let new = revoice(&f.notes, &f.chord, chord, p.key);
    p.replace_notes(track, &f.notes, &new);
    Ok(())
}

/// Plays figure `index` again right after itself, re-voiced to `chord`,
/// replacing whatever starts in that slot. This is "and then": type the
/// next chord and the shape carries on.
pub fn continue_with(
    p: &mut Project,
    track: Id,
    index: usize,
    chord: &Chord,
    s: &FigureSettings,
) -> Result<(), FigureError> {
    let figs = track_figures(p, track, s)?;
    figure_at(&figs, index)?;
    let (at, len, new) = continued(&figs, index, chord, p);
    let displaced: Vec<Note> = p
        .track(track)
        .unwrap()
        .absolute_notes()
        .into_iter()
        .filter(|n| n.start >= at && n.start < at + len)
        .collect();
    p.replace_notes(track, &displaced, &new);
    Ok(())
}

/// What [`continue_with`] would write, without writing it: where the new
/// figure starts, how long its slot is, and its notes.
pub fn continued(figs: &[Figure], index: usize, chord: &Chord, p: &Project) -> (Tick, Tick, Vec<Note>) {
    let f = &figs[index];
    let len = span(figs, index, &p.meter_at(f.start));
    let moved: Vec<Note> = f.notes.iter().map(|n| Note { start: n.start + len, ..*n }).collect();
    (f.start + len, len, revoice(&moved, &f.chord, chord, p.key))
}

/// Appends a block chord in close position at `at`, for starting from an
/// empty track.
pub fn add_block(p: &mut Project, track: Id, at: Tick, length: Tick, chord: &Chord) -> Result<(), FigureError> {
    let t = p.track(track).ok_or(FigureError::NoTrack(track))?;
    let floor = if t.clips.iter().flat_map(|c| &c.notes).any(|n| n.pitch < 52) { 36 } else { 55 };
    let notes: Vec<Note> = chord
        .voicing(floor)
        .into_iter()
        .map(|pitch| Note { pitch, velocity: 90, start: at, duration: length })
        .collect();
    p.replace_notes(track, &[], &notes);
    Ok(())
}

/// One line per figure, for an agent to read:
/// `3  2.1.0  Am (vi)  arpeggio  8 notes  0.97  [C6, Am7]`.
pub fn describe(figures: &[Figure], key: KeySignature, meter: &MeterChange) -> String {
    let spell = Spelling::for_key(key);
    figures
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let alts: Vec<String> = f.alternatives.iter().map(|c| c.name(spell)).collect();
            format!(
                "{i}  {}  {} ({})  {}  {} notes  {:.2}{}",
                crate::cleanup::bar_beat_tick(f.start, meter),
                f.chord.name(spell),
                f.chord.roman(key),
                f.kind.label(),
                f.notes.len(),
                f.confidence,
                if alts.is_empty() { String::new() } else { format!("  [{}]", alts.join(", ")) },
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Project {
    /// Removes `old` notes from `track` (matched exactly, in absolute
    /// ticks) and adds `new` ones, each into the clip it starts in. Notes
    /// past every clip go into the clip ending last, which grows to hold
    /// them; notes before every clip get a new clip.
    pub fn replace_notes(&mut self, track: Id, old: &[Note], new: &[Note]) {
        let first_start = self.track(track).and_then(|t| t.clips.iter().map(|c| c.start).min());
        let needs_clip = new.iter().any(|n| first_start.is_none_or(|s| n.start < s));
        let clip_id = needs_clip.then(|| self.alloc_id());
        let Some(t) = self.track_mut(track) else { return };

        for c in &mut t.clips {
            let start = c.start;
            c.notes.retain(|n| !old.contains(&Note { start: n.start + start, ..*n }));
        }
        if let Some(id) = clip_id {
            let start = new.iter().map(|n| n.start).min().unwrap();
            t.clips.push(crate::model::Clip {
                id,
                name: "Figures".into(),
                start,
                length: 0,
                notes: Vec::new(),
                source_session: None,
            });
        }
        for n in new {
            let i = t
                .clips
                .iter()
                .position(|c| n.start >= c.start && n.start < c.start + c.length)
                .or_else(|| {
                    t.clips
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.start <= n.start)
                        .max_by_key(|(_, c)| c.start + c.length)
                        .map(|(i, _)| i)
                })
                .expect("a clip starts at or before every new note");
            let c = &mut t.clips[i];
            c.notes.push(Note { start: n.start - c.start, ..*n });
            c.length = c.length.max(n.end() - c.start);
        }
        for c in &mut t.clips {
            c.notes.sort_by_key(|n| (n.start, n.pitch));
        }
        t.clips.sort_by_key(|c| c.start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    fn names(figs: &[Figure]) -> Vec<String> {
        figs.iter().map(|f| f.chord.name(Spelling::Mixed)).collect()
    }

    fn n(pitch: u8, start: Tick, duration: Tick) -> Note {
        Note { pitch, velocity: 90, start, duration }
    }

    const FOUR_FOUR: MeterChange = MeterChange { tick: 0, numerator: 4, denominator: 4 };

    #[test]
    fn demo_arpeggios_are_one_figure_per_chord() {
        let p = demo::project();
        let figs = analyze(&p.tracks[0].absolute_notes(), &p.meter_at(0), &FigureSettings::default());
        assert_eq!(names(&figs), ["C", "Am", "F", "G"]);
        assert!(figs.iter().all(|f| f.kind == FigureKind::Arpeggio), "{figs:#?}");
        assert_eq!(
            figs.iter().map(|f| f.chord.roman(p.key)).collect::<Vec<_>>(),
            ["I", "vi", "IV", "V"]
        );
    }

    #[test]
    fn block_chords_with_human_spread() {
        let q = PPQ as Tick;
        let notes = vec![
            // C, rolled over 40 ticks, held two beats, played twice.
            n(60, 0, 2 * q), n(64, 20, 2 * q), n(67, 40, 2 * q),
            n(60, 2 * q, 2 * q), n(64, 2 * q + 10, 2 * q), n(67, 2 * q, 2 * q),
            // Then Fmaj7 for a bar.
            n(53, 4 * q, 4 * q), n(57, 4 * q, 4 * q), n(60, 4 * q + 30, 4 * q), n(64, 4 * q, 4 * q),
        ];
        let figs = analyze(&notes, &FOUR_FOUR, &FigureSettings::default());
        assert_eq!(names(&figs), ["C", "Fmaj7"]);
        assert_eq!(figs[0].kind, FigureKind::Block);
    }

    #[test]
    fn a_run_is_a_run() {
        let e = PPQ as Tick / 4;
        let notes: Vec<Note> =
            [60, 62, 64, 65, 67, 69, 71, 72].iter().enumerate().map(|(i, &p)| n(p, i as Tick * e, e)).collect();
        let figs = analyze(&notes, &FOUR_FOUR, &FigureSettings::default());
        assert_eq!(figs.len(), 1, "{figs:#?}");
        assert_eq!(figs[0].kind, FigureKind::Run);
    }

    #[test]
    fn silence_splits() {
        let q = PPQ as Tick;
        let notes = vec![n(60, 0, q), n(64, q, q), n(67, 2 * q, q), n(60, 8 * q, q), n(64, 9 * q, q), n(67, 10 * q, q)];
        assert_eq!(analyze(&notes, &FOUR_FOUR, &FigureSettings::default()).len(), 2);
    }

    #[test]
    fn revoice_keeps_arpeggio_shape() {
        let e = PPQ as Tick / 2;
        let c_arp: Vec<Note> =
            [60, 64, 67, 72, 67, 64, 67, 64].iter().enumerate().map(|(i, &p)| n(p, i as Tick * e, e)).collect();
        let am = revoice(&c_arp, &Chord::parse("C").unwrap(), &Chord::parse("Am").unwrap(), KeySignature::default());
        let pitches: Vec<u8> = am.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, [57, 60, 64, 69, 64, 60, 64, 60]);
    }

    #[test]
    fn revoice_block_and_slash() {
        let block = vec![n(60, 0, 960), n(64, 0, 960), n(67, 0, 960)];
        let key = KeySignature::default();
        let g: Vec<u8> = revoice(&block, &Chord::parse("C").unwrap(), &Chord::parse("G").unwrap(), key)
            .iter()
            .map(|n| n.pitch)
            .collect();
        assert_eq!(g, [55, 59, 62]);
        let g_b: Vec<u8> = revoice(&block, &Chord::parse("C").unwrap(), &Chord::parse("G/B").unwrap(), key)
            .iter()
            .map(|n| n.pitch)
            .collect();
        assert_eq!(g_b[0] % 12, 11, "{g_b:?}");
        assert!(g_b[0] < g_b[1]);
    }

    #[test]
    fn sevenths_take_a_doubled_note() {
        let key = KeySignature::default();
        let c = Chord::parse("C").unwrap();
        let g7 = Chord::parse("G7").unwrap();
        let e = PPQ as Tick / 2;
        let arp: Vec<Note> =
            [60, 64, 67, 72, 67, 64, 67, 64].iter().enumerate().map(|(i, &p)| n(p, i as Tick * e, e)).collect();
        let pitches: Vec<u8> = revoice(&arp, &c, &g7, key).iter().map(|n| n.pitch).collect();
        // The top C, an octave root, becomes F just under where G would be.
        assert_eq!(pitches, [55, 59, 62, 65, 62, 59, 62, 59]);

        let block = vec![n(60, 0, 960), n(64, 0, 960), n(67, 0, 960)];
        let shell: Vec<u8> = revoice(&block, &c, &g7, key).iter().map(|n| n.pitch).collect();
        assert_eq!(shell, [55, 59, 65], "no doubling: the fifth makes way for the seventh");

        // Going back down to a triad keeps everything a chord tone.
        let back: Vec<u8> = revoice(&revoice(&arp, &c, &g7, key), &g7, &c, key).iter().map(|n| n.pitch % 12).collect();
        assert!(back.iter().all(|pc| [0, 4, 7].contains(pc)), "{back:?}");
    }

    #[test]
    fn passing_tones_follow_the_root() {
        // C D E over C, moved to G: the chord tones become G and B, and the
        // passing D moves with the root to A.
        let notes = vec![n(60, 0, 240), n(62, 240, 240), n(64, 480, 240)];
        let to_g = revoice(&notes, &Chord::parse("C").unwrap(), &Chord::parse("G").unwrap(), KeySignature::default());
        assert_eq!(to_g.iter().map(|n| n.pitch).collect::<Vec<_>>(), [55, 57, 59]);
    }

    #[test]
    fn continue_with_extends_the_progression() {
        let mut p = demo::project();
        let keys = p.tracks[0].id;
        let s = FigureSettings::default();
        continue_with(&mut p, keys, 3, &Chord::parse("C").unwrap(), &s).unwrap();
        let figs = track_figures(&p, keys, &s).unwrap();
        assert_eq!(names(&figs), ["C", "Am", "F", "G", "C"]);
        // The demo is quantized at 90%, so starts sit a tick or two off grid.
        assert!(figs[4].start.abs_diff(16 * PPQ as Tick) < 10, "{}", figs[4].start);
        assert_eq!(figs[4].notes.len(), 8);

        set_chord(&mut p, keys, 1, &Chord::parse("Em").unwrap(), &s).unwrap();
        let figs = track_figures(&p, keys, &s).unwrap();
        assert_eq!(names(&figs), ["C", "Em", "F", "G", "C"]);
    }

    #[test]
    fn long_tracks_analyze_quickly() {
        // 64 bars of sixteenths: about what a dense real track looks like.
        let s = PPQ as Tick / 4;
        let roots = [60u8, 57, 53, 55];
        let notes: Vec<Note> = (0..64 * 16)
            .map(|i| {
                let r = roots[(i / 16) % 4];
                n(r + [0, 4, 7, 12][i % 4], i as Tick * s, s)
            })
            .collect();
        let t = std::time::Instant::now();
        let figs = analyze(&notes, &FOUR_FOUR, &FigureSettings::default());
        let took = t.elapsed();
        assert_eq!(figs.len(), 64);
        // Generous, for unoptimized test builds.
        assert!(took.as_millis() < 2000, "{took:?}");
    }

    #[test]
    fn describes_for_agents() {
        let p = demo::project();
        let figs = analyze(&p.tracks[0].absolute_notes(), &p.meter_at(0), &FigureSettings::default());
        let text = describe(&figs, p.key, &p.meter_at(0));
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("0  1.1.") && first.contains("C (I)  arpeggio  8 notes"), "{text}");
    }
}
