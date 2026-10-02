//! ABC notation export.
//!
//! ABC is text, so it's the format an agent can read and write most easily,
//! and it renders to a score with any ABC tool. The export is lossy by
//! design: times snap to a fixed unit, and when chord notes have different
//! lengths, the whole chord is cut where the next note starts.

use std::fmt::Write;

use compypal_core::{KeySignature, Note, PPQ, Project, Tick, Track};

#[derive(Clone, Copy, Debug)]
pub struct AbcOptions {
    /// The `L:` unit in ticks. Everything snaps to it. Defaults to a
    /// sixteenth note.
    pub unit: Tick,
    pub bars_per_line: usize,
    /// Leave out muted tracks.
    pub skip_muted: bool,
}

impl Default for AbcOptions {
    fn default() -> Self {
        Self { unit: PPQ as Tick / 4, bars_per_line: 4, skip_muted: false }
    }
}

pub fn export(project: &Project, opts: &AbcOptions) -> String {
    let unit = opts.unit.max(1);
    let whole = PPQ as Tick * 4;
    let mut out = String::new();
    let m0 = project.meter_at(0);
    let _ = writeln!(out, "X:1");
    let _ = writeln!(out, "T:{}", project.name);
    let _ = writeln!(out, "M:{}/{}", m0.numerator, m0.denominator);
    let _ = writeln!(out, "L:{}", fraction(unit, whole));
    let _ = writeln!(out, "Q:1/4={}", project.tempo.bpm_at(0).round());
    let _ = writeln!(out, "K:{}", project.key.name());

    let bars = bar_lines(project);
    let tracks: Vec<&Track> =
        project.tracks.iter().filter(|t| !(opts.skip_muted && t.muted)).collect();
    for (i, t) in tracks.iter().enumerate() {
        let notes = t.absolute_notes();
        let clef = if !t.is_drums() && mean_pitch(&notes) < 55.0 { " clef=bass" } else { "" };
        let _ = writeln!(out, "V:{} name=\"{}\"{clef}", i + 1, t.name.replace('"', "'"));
        if t.is_drums() {
            let _ = writeln!(out, "%%MIDI channel 10");
        } else {
            let _ = writeln!(out, "%%MIDI program {}", t.program);
        }
        let events = timeline(&notes, unit, bars.last().copied().unwrap_or(0));
        write_voice(&mut out, &events, &bars, unit, project, opts.bars_per_line);
    }
    out
}

fn mean_pitch(notes: &[Note]) -> f64 {
    if notes.is_empty() {
        return 60.0;
    }
    notes.iter().map(|n| n.pitch as f64).sum::<f64>() / notes.len() as f64
}

/// Tick positions of every barline up to and including the end, so the
/// last bar is complete.
fn bar_lines(project: &Project) -> Vec<Tick> {
    let end = project.end_tick().max(1);
    let mut lines = vec![0];
    let mut t = 0;
    while t < end {
        t += project.meter_at(t).ticks_per_bar();
        lines.push(t);
    }
    lines
}

/// A run of time in units: a chord (one or more pitches) or a rest.
#[derive(Clone, Debug, PartialEq)]
struct Event {
    start: Tick,
    len: Tick,
    pitches: Vec<u8>,
}

/// Flattens notes into non-overlapping chords and rests, in units.
fn timeline(notes: &[Note], unit: Tick, end: Tick) -> Vec<Event> {
    let snap = |t: Tick| (t + unit / 2) / unit;
    let mut snapped: Vec<(Tick, Tick, u8)> = notes
        .iter()
        .map(|n| {
            let s = snap(n.start);
            (s, snap(n.end()).max(s + 1), n.pitch)
        })
        .collect();
    snapped.sort();

    let mut events = Vec::new();
    let mut cursor = 0;
    let mut i = 0;
    while i < snapped.len() {
        let start = snapped[i].0;
        let mut j = i;
        while j < snapped.len() && snapped[j].0 == start {
            j += 1;
        }
        let chord = &snapped[i..j];
        let next = snapped.get(j).map_or(Tick::MAX, |n| n.0);
        let stop = chord.iter().map(|n| n.1).max().unwrap().min(next);
        if start > cursor {
            events.push(Event { start: cursor, len: start - cursor, pitches: vec![] });
        }
        let mut pitches: Vec<u8> = chord.iter().map(|n| n.2).collect();
        pitches.dedup();
        events.push(Event { start, len: stop - start, pitches });
        cursor = stop;
        i = j;
    }
    let end = end / unit;
    if end > cursor {
        events.push(Event { start: cursor, len: end - cursor, pitches: vec![] });
    }
    events
}

fn write_voice(
    out: &mut String,
    events: &[Event],
    bars: &[Tick],
    unit: Tick,
    project: &Project,
    per_line: usize,
) {
    let mut meter = project.meter_at(0);
    let mut line = String::new();
    let mut ev = events.iter().cloned();
    let mut carry: Option<Event> = None;
    for (b, w) in bars.windows(2).enumerate() {
        let (bar_start, bar_end) = (w[0] / unit, w[1] / unit);
        let m = project.meter_at(w[0]);
        if m != meter && m.tick == w[0] {
            let _ = write!(line, "[M:{}/{}]", m.numerator, m.denominator);
            meter = m;
        }
        let mut acc = Accidentals::new(project.key);
        while let Some(e) = carry.take().or_else(|| ev.next()) {
            if e.start >= bar_end {
                carry = Some(e);
                break;
            }
            let start = e.start.max(bar_start);
            let stop = (e.start + e.len).min(bar_end);
            let crosses = e.start + e.len > bar_end;
            write_event(&mut line, &e.pitches, stop - start, crosses, unit, &mut acc);
            if crosses {
                carry = Some(Event { start: bar_end, len: e.start + e.len - bar_end, pitches: e.pitches });
                break;
            }
        }
        line.push_str(if b + 1 == bars.len() - 1 { " |]" } else { " |" });
        if (b + 1) % per_line == 0 || b + 1 == bars.len() - 1 {
            out.push_str(line.trim());
            out.push('\n');
            line.clear();
        }
    }
}

/// Writes one bar-bounded event, splitting lengths that have no single
/// note value into tied pieces.
fn write_event(out: &mut String, pitches: &[u8], len: Tick, tie_out: bool, unit: Tick, acc: &mut Accidentals) {
    let pieces = split_len(len, unit);
    for (i, piece) in pieces.iter().enumerate() {
        out.push(' ');
        if pitches.is_empty() {
            out.push('z');
        } else {
            if pitches.len() > 1 {
                out.push('[');
            }
            for &p in pitches {
                out.push_str(&acc.spell(p));
            }
            if pitches.len() > 1 {
                out.push(']');
            }
        }
        if *piece != 1 {
            let _ = write!(out, "{piece}");
        }
        let last = i + 1 == pieces.len();
        if !pitches.is_empty() && (!last || tie_out) {
            out.push('-');
        }
    }
}

/// Splits a length in units into note values that read cleanly: whole,
/// half, quarter and so on, plus their dotted forms.
fn split_len(len: Tick, unit: Tick) -> Vec<Tick> {
    let whole = PPQ as Tick * 4;
    let mut values: Vec<Tick> = Vec::new();
    let mut v = whole;
    while v >= unit {
        if v.is_multiple_of(unit) {
            values.push(v / unit);
            let dotted = v * 3 / 2;
            if dotted.is_multiple_of(unit) && dotted <= whole {
                values.push(dotted / unit);
            }
        }
        v /= 2;
    }
    values.sort_unstable_by(|a, b| b.cmp(a));
    values.dedup();
    let mut left = len;
    let mut out = Vec::new();
    while left > 0 {
        let v = values.iter().copied().find(|&v| v <= left).unwrap_or(left);
        out.push(v);
        left -= v;
    }
    out
}

/// Spells pitches against the key signature and the accidentals already
/// written in the current bar, which ABC carries to the barline.
struct Accidentals {
    sharps: bool,
    key: [i8; 7],
    bar: std::collections::HashMap<(u8, i32), i8>,
}

impl Accidentals {
    fn new(key: KeySignature) -> Self {
        const SHARP_ORDER: [usize; 7] = [3, 0, 4, 1, 5, 2, 6]; // F C G D A E B
        let mut k = [0i8; 7];
        let n = key.fifths.unsigned_abs().min(7) as usize;
        for i in 0..n {
            if key.fifths > 0 {
                k[SHARP_ORDER[i]] = 1;
            } else {
                k[SHARP_ORDER[6 - i]] = -1;
            }
        }
        Self { sharps: key.fifths >= 0, key: k, bar: Default::default() }
    }

    fn spell(&mut self, pitch: u8) -> String {
        // (letter index C=0..B=6, accidental) per pitch class.
        const SHARP: [(u8, i8); 12] =
            [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (3, 0), (3, 1), (4, 0), (4, 1), (5, 0), (5, 1), (6, 0)];
        const FLAT: [(u8, i8); 12] =
            [(0, 0), (1, -1), (1, 0), (2, -1), (2, 0), (3, 0), (4, -1), (4, 0), (5, -1), (5, 0), (6, -1), (6, 0)];
        let (letter, a) = if self.sharps { SHARP } else { FLAT }[(pitch % 12) as usize];
        let octave = pitch as i32 / 12 - 1;
        let current = *self.bar.get(&(letter, octave)).unwrap_or(&self.key[letter as usize]);
        let mut s = String::new();
        if current != a {
            s.push(match a {
                1 => '^',
                -1 => '_',
                _ => '=',
            });
            self.bar.insert((letter, octave), a);
        }
        let name = b"CDEFGAB"[letter as usize] as char;
        if octave >= 5 {
            s.push(name.to_ascii_lowercase());
            s.extend(std::iter::repeat_n('\'', (octave - 5) as usize));
        } else {
            s.push(name);
            s.extend(std::iter::repeat_n(',', (4 - octave).max(0) as usize));
        }
        s
    }
}

fn fraction(n: Tick, d: Tick) -> String {
    fn gcd(a: Tick, b: Tick) -> Tick {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(n, d);
    format!("{}/{}", n / g, d / g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compypal_core::{Clip, Id};

    fn one_track(notes: Vec<Note>, key: KeySignature) -> Project {
        let mut p = Project::new("Test");
        p.key = key;
        let t = p.add_track("Piano", 0);
        let len = notes.iter().map(|n| n.end()).max().unwrap_or(0);
        p.track_mut(t).unwrap().clips.push(Clip {
            id: Id(99),
            name: String::new(),
            start: 0,
            length: len,
            notes,
            source_session: None,
        });
        p
    }

    fn q(pitch: u8, beat: Tick, beats: Tick) -> Note {
        Note { pitch, velocity: 90, start: beat * PPQ as Tick, duration: beats * PPQ as Tick }
    }

    fn body(abc: &str) -> String {
        abc.lines().filter(|l| !l.contains(':') && !l.starts_with("%%")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn scale_in_c() {
        let notes = (0..4).map(|i| q([60, 62, 64, 72][i], i as Tick, 1)).collect();
        let abc = export(&one_track(notes, KeySignature::default()), &AbcOptions::default());
        assert_eq!(body(&abc), "C4 D4 E4 c4 |]");
    }

    #[test]
    fn accidentals_follow_key_and_bar() {
        // G major: F# needs no mark; F natural does, and it then carries.
        let key = KeySignature { fifths: 1, minor: false };
        let notes = vec![q(66, 0, 1), q(65, 1, 1), q(65, 2, 1), q(66, 3, 1), q(65, 4, 4)];
        let abc = export(&one_track(notes, key), &AbcOptions::default());
        assert!(abc.contains("K:G"));
        assert_eq!(body(&abc), "F4 =F4 F4 ^F4 | =F16 |]");
    }

    #[test]
    fn ties_across_barline_and_rests() {
        let notes = vec![q(48, 1, 2), q(43, 3, 2)];
        let abc = export(&one_track(notes, KeySignature::default()), &AbcOptions::default());
        assert!(abc.contains("clef=bass"));
        assert_eq!(body(&abc), "z4 C,8 G,,4- | G,,4 z12 |]");
    }

    #[test]
    fn chords_and_odd_lengths() {
        // Five sixteenths splits into a quarter tied to a sixteenth.
        let notes = vec![
            Note { pitch: 60, velocity: 90, start: 0, duration: 5 * 240 },
            Note { pitch: 64, velocity: 90, start: 0, duration: 5 * 240 },
        ];
        let abc = export(&one_track(notes, KeySignature::default()), &AbcOptions::default());
        assert_eq!(body(&abc), "[CE]4- [CE] z8 z3 |]");
    }
}
