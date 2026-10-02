//! Chords, keys and Roman numerals.
//!
//! A [`Chord`] is a root, a quality from a fixed table, and an optional
//! bass note for slash chords. Text goes both ways: [`Chord::parse`] accepts
//! the common spellings people type (`Am7`, `F#m7b5`, `Bbmaj7`, `C/E`,
//! `G7sus4`, `Dø`), and [`Chord::name`] prints one canonical spelling.
//! [`parse_roman`] and [`Chord::roman`] do the same relative to a key.

use serde::{Deserialize, Serialize};

use crate::model::{KeySignature, Note};

/// Pitch class, 0 = C.
pub type Pc = u8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    /// The canonical suffix: `""` is a major triad.
    pub suffix: &'static str,
    /// Semitones above the root, in role order: root, third, fifth, seventh,
    /// then extensions. Extensions sit above the octave so voicings can
    /// place them there.
    pub intervals: &'static [u8],
    /// How unusual the chord is, as a tiebreak when several fit the notes.
    /// Triads are 0.
    pub complexity: f64,
}

pub const QUALITIES: &[Quality] = &[
    Quality { suffix: "", intervals: &[0, 4, 7], complexity: 0.0 },
    Quality { suffix: "m", intervals: &[0, 3, 7], complexity: 0.0 },
    Quality { suffix: "dim", intervals: &[0, 3, 6], complexity: 0.05 },
    Quality { suffix: "aug", intervals: &[0, 4, 8], complexity: 0.1 },
    Quality { suffix: "sus2", intervals: &[0, 2, 7], complexity: 0.08 },
    Quality { suffix: "sus4", intervals: &[0, 5, 7], complexity: 0.06 },
    Quality { suffix: "5", intervals: &[0, 7], complexity: 0.05 },
    Quality { suffix: "7", intervals: &[0, 4, 7, 10], complexity: 0.08 },
    Quality { suffix: "maj7", intervals: &[0, 4, 7, 11], complexity: 0.1 },
    Quality { suffix: "m7", intervals: &[0, 3, 7, 10], complexity: 0.1 },
    Quality { suffix: "m7b5", intervals: &[0, 3, 6, 10], complexity: 0.12 },
    Quality { suffix: "dim7", intervals: &[0, 3, 6, 9], complexity: 0.12 },
    Quality { suffix: "mMaj7", intervals: &[0, 3, 7, 11], complexity: 0.2 },
    Quality { suffix: "6", intervals: &[0, 4, 7, 9], complexity: 0.14 },
    Quality { suffix: "m6", intervals: &[0, 3, 7, 9], complexity: 0.16 },
    Quality { suffix: "7sus4", intervals: &[0, 5, 7, 10], complexity: 0.14 },
    Quality { suffix: "add9", intervals: &[0, 4, 7, 14], complexity: 0.14 },
    Quality { suffix: "madd9", intervals: &[0, 3, 7, 14], complexity: 0.16 },
    Quality { suffix: "6/9", intervals: &[0, 4, 7, 9, 14], complexity: 0.2 },
    Quality { suffix: "9", intervals: &[0, 4, 7, 10, 14], complexity: 0.18 },
    Quality { suffix: "maj9", intervals: &[0, 4, 7, 11, 14], complexity: 0.18 },
    Quality { suffix: "m9", intervals: &[0, 3, 7, 10, 14], complexity: 0.18 },
    Quality { suffix: "7b9", intervals: &[0, 4, 7, 10, 13], complexity: 0.22 },
    Quality { suffix: "7#9", intervals: &[0, 4, 7, 10, 15], complexity: 0.22 },
    Quality { suffix: "11", intervals: &[0, 4, 7, 10, 14, 17], complexity: 0.25 },
    Quality { suffix: "m11", intervals: &[0, 3, 7, 10, 14, 17], complexity: 0.25 },
    Quality { suffix: "13", intervals: &[0, 4, 7, 10, 14, 21], complexity: 0.25 },
];

/// Other ways people write the same suffixes.
const ALIASES: &[(&str, &str)] = &[
    ("maj", ""),
    ("M", ""),
    ("min", "m"),
    ("-", "m"),
    ("mi", "m"),
    ("°", "dim"),
    ("o", "dim"),
    ("+", "aug"),
    ("sus", "sus4"),
    ("M7", "maj7"),
    ("Maj7", "maj7"),
    ("ma7", "maj7"),
    ("Δ", "maj7"),
    ("Δ7", "maj7"),
    ("min7", "m7"),
    ("-7", "m7"),
    ("mi7", "m7"),
    ("ø", "m7b5"),
    ("ø7", "m7b5"),
    ("m7-5", "m7b5"),
    ("-7b5", "m7b5"),
    ("°7", "dim7"),
    ("o7", "dim7"),
    ("mM7", "mMaj7"),
    ("m(maj7)", "mMaj7"),
    ("min6", "m6"),
    ("add2", "add9"),
    ("2", "add9"),
    ("madd2", "madd9"),
    ("69", "6/9"),
    ("M9", "maj9"),
    ("min9", "m9"),
    ("sus7", "7sus4"),
    ("7sus", "7sus4"),
    ("aug7", "7"),
    ("dom7", "7"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chord {
    pub root: Pc,
    pub quality: &'static Quality,
    /// Bass note for slash chords, when it is not the root.
    pub bass: Option<Pc>,
}

impl Serialize for Chord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name(Spelling::Sharps))
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Chord::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    #[error("empty chord")]
    Empty,
    #[error("unknown note name in {0:?}")]
    BadRoot(String),
    #[error("unknown chord quality {0:?}")]
    BadQuality(String),
    #[error("not a Roman numeral: {0:?}")]
    BadRoman(String),
}

/// Whether to name black keys as sharps or flats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spelling {
    Sharps,
    Flats,
    /// The usual pop/jazz spelling in C: C#, Eb, F#, Ab, Bb.
    Mixed,
}

impl Spelling {
    pub fn for_key(key: KeySignature) -> Self {
        match key.fifths {
            f if f < 0 => Spelling::Flats,
            f if f >= 2 => Spelling::Sharps,
            _ => Spelling::Mixed,
        }
    }
}

pub fn pc_name(pc: Pc, spelling: Spelling) -> &'static str {
    const SHARPS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    const FLATS: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];
    const MIXED: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
    let table = match spelling {
        Spelling::Sharps => SHARPS,
        Spelling::Flats => FLATS,
        Spelling::Mixed => MIXED,
    };
    table[(pc % 12) as usize]
}

/// Reads a note name at the start of `s`: a letter, then any number of
/// `#`/`b`/`♯`/`♭`. Returns the pitch class and the rest of the string.
pub fn parse_pc(s: &str) -> Option<(Pc, &str)> {
    let mut chars = s.char_indices();
    let (_, letter) = chars.next()?;
    let base: i32 = match letter.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut pc = base;
    let mut rest = &s[letter.len_utf8()..];
    loop {
        let mut it = rest.chars();
        match it.next() {
            Some('#') | Some('♯') => pc += 1,
            // Lowercase "b" after the letter is always a flat, so a "b"
            // quality suffix is impossible; none exists.
            Some('b') | Some('♭') => pc -= 1,
            _ => break,
        }
        rest = it.as_str();
    }
    Some((pc.rem_euclid(12) as Pc, rest))
}

impl Chord {
    pub fn new(root: Pc, suffix: &str) -> Option<Self> {
        let quality = QUALITIES.iter().find(|q| q.suffix == suffix)?;
        Some(Self { root: root % 12, quality, bass: None })
    }

    pub fn parse(text: &str) -> Result<Self, ChordError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ChordError::Empty);
        }
        // A trailing "/X" is a bass note only when X is a note name: the
        // slash in "6/9" is part of the quality.
        let (body, bass) = match text.rsplit_once('/') {
            Some((b, after)) => match parse_pc(after.trim()) {
                Some((pc, "")) => (b, Some(pc)),
                _ => (text, None),
            },
            None => (text, None),
        };
        let (root, suffix) = parse_pc(body).ok_or_else(|| ChordError::BadRoot(text.to_string()))?;
        let suffix = suffix.trim();
        let quality = QUALITIES
            .iter()
            .find(|q| q.suffix == suffix)
            .or_else(|| {
                let canon = ALIASES.iter().find(|(a, _)| *a == suffix)?.1;
                QUALITIES.iter().find(|q| q.suffix == canon)
            })
            .ok_or_else(|| ChordError::BadQuality(suffix.to_string()))?;
        Ok(Self { root, quality, bass: bass.filter(|&b| b != root) })
    }

    pub fn name(&self, spelling: Spelling) -> String {
        let mut s = format!("{}{}", pc_name(self.root, spelling), self.quality.suffix);
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(pc_name(b, spelling));
        }
        s
    }

    /// Pitch classes in role order: root, third, fifth, and so on.
    pub fn pcs(&self) -> impl Iterator<Item = Pc> + '_ {
        self.quality.intervals.iter().map(|i| (self.root + i) % 12)
    }

    pub fn contains(&self, pc: Pc) -> bool {
        self.pcs().any(|p| p == pc % 12) || self.bass == Some(pc % 12)
    }

    /// Which role `pc` plays in the chord: 0 root, 1 third, 2 fifth, ...
    pub fn role_of(&self, pc: Pc) -> Option<usize> {
        self.pcs().position(|p| p == pc % 12)
    }

    pub fn is_minor(&self) -> bool {
        self.quality.intervals.get(1) == Some(&3)
    }

    /// The lowest bass note: the slash note if there is one, else the root.
    pub fn bass_pc(&self) -> Pc {
        self.bass.unwrap_or(self.root)
    }

    /// A close-position voicing with the root at or above `floor`.
    pub fn voicing(&self, floor: u8) -> Vec<u8> {
        let base = floor + (self.root + 12 - floor % 12) % 12;
        let mut v: Vec<u8> = self.quality.intervals.iter().map(|i| base.saturating_add(*i)).collect();
        if let Some(b) = self.bass {
            let below = base - (base % 12 + 12 - b) % 12;
            v.insert(0, if below == base { base - 12 } else { below });
        }
        v.retain(|p| *p <= 127);
        v
    }

    /// The chord as a Roman numeral in `key`: `vi`, `V7`, `bVII`, `ii°`.
    pub fn roman(&self, key: KeySignature) -> String {
        let scale = key.scale();
        let rel = (self.root + 12 - key.tonic()) % 12;
        // Name by scale degree when the root is in the key; otherwise as an
        // altered degree, flat of the one above.
        let (degree, acc) = match scale.iter().position(|&s| s == rel) {
            Some(d) => (d, ""),
            None => match scale.iter().position(|&s| s == (rel + 1) % 12) {
                Some(d) => (d, "b"),
                None => (scale.iter().position(|&s| s == (rel + 11) % 12).unwrap_or(0), "#"),
            },
        };
        const NUMERALS: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];
        let lower = matches!(self.quality.intervals.get(1), Some(3));
        let numeral =
            if lower { NUMERALS[degree].to_lowercase() } else { NUMERALS[degree].to_string() };
        let suffix = match self.quality.suffix {
            "m" => "",
            "dim" => "°",
            "aug" => "+",
            "m7b5" => "ø7",
            "dim7" => "°7",
            s if lower && s.starts_with('m') && !s.starts_with("maj") => &s[1..],
            s => s,
        };
        let mut out = format!("{acc}{numeral}{suffix}");
        if let Some(b) = self.bass {
            let rel = (b + 12 - self.root) % 12;
            // Inversions by the chord tone in the bass, as figured bass would.
            let fig = match self.quality.intervals.iter().position(|i| i % 12 == rel) {
                Some(1) => "/3",
                Some(2) => "/5",
                Some(3) => "/7",
                _ => "",
            };
            if fig.is_empty() {
                out.push('/');
                out.push_str(pc_name(b, Spelling::for_key(key)));
            } else {
                out.push_str(fig);
            }
        }
        out
    }
}

impl KeySignature {
    pub fn tonic(&self) -> Pc {
        let major = (self.fifths as i32 * 7).rem_euclid(12) as Pc;
        if self.minor { (major + 9) % 12 } else { major }
    }

    /// Semitones above the tonic, degrees 1 to 7 (natural minor for minor keys).
    pub fn scale(&self) -> [Pc; 7] {
        if self.minor { [0, 2, 3, 5, 7, 8, 10] } else { [0, 2, 4, 5, 7, 9, 11] }
    }

    pub fn contains(&self, pc: Pc) -> bool {
        let rel = (pc + 12 - self.tonic()) % 12;
        self.scale().contains(&rel)
    }

    pub fn from_tonic(tonic: Pc, minor: bool) -> Self {
        let major_tonic = if minor { (tonic + 3) % 12 } else { tonic % 12 };
        // Fifths for each major tonic, preferring the flat side for Db/Gb.
        const FIFTHS: [i8; 12] = [0, -5, 2, -3, 4, -1, -6, 1, -4, 3, -2, 5];
        Self { fifths: FIFTHS[major_tonic as usize], minor }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let (pc, rest) = parse_pc(t)?;
        let minor = match rest.trim() {
            "" | "maj" | "major" | "M" => false,
            "m" | "min" | "minor" => true,
            _ => return None,
        };
        Some(Self::from_tonic(pc, minor))
    }

    /// The seven diatonic chords, as triads or sevenths.
    pub fn diatonic(&self, sevenths: bool) -> Vec<Chord> {
        let scale = self.scale();
        (0..7)
            .map(|d| {
                let third = (scale[(d + 2) % 7] + 12 - scale[d]) % 12;
                let fifth = (scale[(d + 4) % 7] + 12 - scale[d]) % 12;
                let seventh = (scale[(d + 6) % 7] + 12 - scale[d]) % 12;
                let suffix = match (third, fifth, sevenths.then_some(seventh)) {
                    (4, 7, None) => "",
                    (3, 7, None) => "m",
                    (3, 6, None) => "dim",
                    (4, 8, None) => "aug",
                    (4, 7, Some(11)) => "maj7",
                    (4, 7, Some(10)) => "7",
                    (3, 7, Some(10)) => "m7",
                    (3, 6, Some(10)) => "m7b5",
                    (3, 6, Some(9)) => "dim7",
                    _ => "",
                };
                Chord::new((self.tonic() + scale[d]) % 12, suffix).unwrap()
            })
            .collect()
    }
}

/// Reads a Roman numeral in `key`: `I`, `vi`, `V7`, `bVII`, `ii7`, `vii°`,
/// `iiø7`, `IVmaj7`, `V/V`, `V7/ii`, `I/3`.
pub fn parse_roman(text: &str, key: KeySignature) -> Result<Chord, ChordError> {
    let bad = || ChordError::BadRoman(text.to_string());
    let text = text.trim();
    // A trailing "/x" is either an inversion figure (/3 /5 /7) or a
    // secondary function (V/V): the numeral after it becomes the tonic.
    let (body, after) = match text.split_once('/') {
        Some((b, a)) => (b, Some(a)),
        None => (text, None),
    };
    let mut key_here = key;
    let mut inversion = None;
    match after {
        Some(a @ ("3" | "5" | "7")) => inversion = a.parse::<usize>().ok(),
        Some(target) => {
            let t = parse_roman(target, key)?;
            key_here = KeySignature::from_tonic(t.root, t.is_minor());
        }
        None => {}
    }

    let (acc, rest) = match body.chars().next() {
        Some('b') | Some('♭') => (-1i8, &body[body.chars().next().unwrap().len_utf8()..]),
        Some('#') | Some('♯') => (1, &body[body.chars().next().unwrap().len_utf8()..]),
        _ => (0, body),
    };
    const NUMERALS: [&str; 7] = ["VII", "III", "VI", "IV", "II", "V", "I"]; // longest first
    let upper = rest.to_uppercase();
    let numeral = NUMERALS.iter().find(|n| upper.starts_with(*n)).ok_or_else(bad)?;
    let degree = ["I", "II", "III", "IV", "V", "VI", "VII"].iter().position(|n| n == numeral).unwrap();
    let written = &rest[..numeral.len()];
    let lower = written.chars().all(|c| c.is_lowercase());
    let suffix = &rest[numeral.len()..];

    let suffix: String = match (lower, suffix) {
        (_, "°" | "o") => "dim".into(),
        (_, "°7" | "o7") => "dim7".into(),
        (_, "ø" | "ø7") => "m7b5".into(),
        (_, "+") => "aug".into(),
        (true, "") => "m".into(),
        (true, s) if s.starts_with(|c: char| c.is_ascii_digit()) => format!("m{s}"),
        (_, s) => s.to_string(),
    };
    let root = (key_here.tonic() as i32 + key_here.scale()[degree] as i32 + acc as i32).rem_euclid(12) as Pc;
    let mut chord = Chord::new(root, &suffix)
        .or_else(|| Chord::parse(&format!("C{suffix}")).ok().map(|c| Chord { root, ..c }))
        .ok_or_else(bad)?;
    if let Some(n) = inversion {
        let role = n / 2; // 3 -> third (1), 5 -> fifth (2), 7 -> seventh (3)
        let bass = chord.pcs().nth(role);
        chord.bass = bass;
    }
    Ok(chord)
}

/// A chord as typed: a symbol, else a Roman numeral in `key`.
pub fn parse_chord_or_roman(text: &str, key: KeySignature) -> Result<Chord, ChordError> {
    Chord::parse(text).or_else(|e| parse_roman(text, key).map_err(|_| e))
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ChordFit {
    pub chord: Chord,
    /// Roughly 0 (nothing fits) to 1 (exact).
    pub score: f64,
}

/// Ranks the chords that could explain a group of notes. Each note counts
/// for its duration, so passing tones weigh less than held ones, and the
/// lowest note counts as the bass.
pub fn identify(notes: &[Note]) -> Vec<ChordFit> {
    let mut weights = [0.0f64; 12];
    for n in notes {
        weights[(n.pitch % 12) as usize] += n.duration.max(1) as f64;
    }
    let bass = notes.iter().map(|n| n.pitch).min().map(|p| p % 12);
    identify_weights(&weights, bass)
}

/// [`identify`] over a pitch-class histogram.
pub fn identify_weights(weights: &[f64; 12], bass: Option<Pc>) -> Vec<ChordFit> {
    let mut fits = Vec::with_capacity(12 * QUALITIES.len());
    each_fit(weights, bass, |f| fits.push(f));
    fits.sort_by(|a, b| b.score.total_cmp(&a.score));
    fits
}

/// The single best chord for a histogram, without ranking the rest.
pub fn best_fit(weights: &[f64; 12], bass: Option<Pc>) -> Option<ChordFit> {
    let mut best: Option<ChordFit> = None;
    each_fit(weights, bass, |f| {
        if best.is_none_or(|b| f.score > b.score) {
            best = Some(f);
        }
    });
    best
}

/// Scores roots and qualities against the histogram. A chord gains for
/// the weight it explains, loses for weight it doesn't, loses for each of
/// its tones that is absent (the fifth only half), and gains a little when
/// the bass is its root.
fn each_fit(weights: &[f64; 12], bass: Option<Pc>, mut emit: impl FnMut(ChordFit)) {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return;
    }
    let mut w = [0.0f64; 12];
    for (o, x) in w.iter_mut().zip(weights) {
        *o = x / total;
    }
    let present = w.iter().filter(|&&x| x > 0.0).count();
    // Only chords rooted on a sounding note: a rootless reading is never
    // the name you'd give what was played.
    for root in (0..12u8).filter(|&r| w[r as usize] > 0.0) {
        for q in QUALITIES {
            // A one-note group is just that note, not a chord.
            if present == 1 && !q.suffix.is_empty() {
                continue;
            }
            let mut covered = 0.0;
            let mut missing = 0.0;
            let mut bass_in_chord = false;
            for (role, iv) in q.intervals.iter().enumerate() {
                let pc = (root + iv) % 12;
                covered += w[pc as usize];
                if w[pc as usize] == 0.0 {
                    missing += if role == 2 { 0.5 } else { 1.0 };
                }
                bass_in_chord |= bass == Some(pc);
            }
            let mut score = covered - (1.0 - covered) - 0.15 * missing - q.complexity;
            let mut chord = Chord { root, quality: q, bass: None };
            match bass {
                Some(b) if b == root => score += 0.1,
                Some(b) if bass_in_chord => chord.bass = Some(b),
                _ => {}
            }
            emit(ChordFit { chord, score });
        }
    }
}

/// Chords to offer while someone types. `context` holds readings of the
/// notes being edited (shown first when nothing is typed). Text that parses
/// comes first as typed; then chords whose symbol or Roman numeral starts
/// with the text, diatonic and simple ones first.
pub fn suggest(text: &str, key: KeySignature, context: &[Chord], limit: usize) -> Vec<Chord> {
    let text = text.trim();
    let spell = Spelling::for_key(key);
    let diatonic: Vec<Chord> = key.diatonic(false).into_iter().chain(key.diatonic(true)).collect();
    let mut out: Vec<Chord> = Vec::new();
    let push = |c: Chord, out: &mut Vec<Chord>| {
        if out.len() < limit && !out.contains(&c) {
            out.push(c);
        }
    };
    if text.is_empty() {
        for &c in context.iter().chain(&diatonic) {
            push(c, &mut out);
        }
        return out;
    }
    if let Ok(c) = parse_chord_or_roman(text, key) {
        push(c, &mut out);
    }
    let rank = |c: &Chord| {
        let in_key = diatonic.contains(c);
        let in_context = context.contains(c);
        (!in_context, !in_key, (c.quality.complexity * 100.0) as i64, c.name(spell).len())
    };
    // "am7" means Am7: a lowercase root letter is just lazy typing.
    let symbol: String = match text.chars().next() {
        Some(c @ 'a'..='g') => c.to_ascii_uppercase().to_string() + &text[1..],
        _ => text.to_string(),
    };
    let mut matches: Vec<Chord> = (0..12u8)
        .flat_map(|root| QUALITIES.iter().map(move |q| Chord { root, quality: q, bass: None }))
        .filter(|c| c.name(spell).starts_with(&symbol) || c.roman(key).starts_with(text))
        .collect();
    matches.sort_by_key(rank);
    for c in matches {
        push(c, &mut out);
    }
    out
}

/// Krumhansl–Kessler key profiles, correlated against a duration-weighted
/// pitch-class histogram. Returns the best key and its correlation.
pub fn detect_key(notes: &[Note]) -> Option<(KeySignature, f64)> {
    const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    let mut h = [0.0f64; 12];
    for n in notes {
        h[(n.pitch % 12) as usize] += n.duration.max(1) as f64;
    }
    if h.iter().all(|&x| x == 0.0) {
        return None;
    }
    let corr = |profile: &[f64; 12], tonic: usize| {
        let p: Vec<f64> = (0..12).map(|i| profile[(i + 12 - tonic) % 12]).collect();
        let (mh, mp) = (h.iter().sum::<f64>() / 12.0, p.iter().sum::<f64>() / 12.0);
        let num: f64 = (0..12).map(|i| (h[i] - mh) * (p[i] - mp)).sum();
        let dh: f64 = h.iter().map(|x| (x - mh).powi(2)).sum::<f64>().sqrt();
        let dp: f64 = p.iter().map(|x| (x - mp).powi(2)).sum::<f64>().sqrt();
        if dh == 0.0 || dp == 0.0 { 0.0 } else { num / (dh * dp) }
    };
    (0..12)
        .flat_map(|t| [(t, false, corr(&MAJOR, t)), (t, true, corr(&MINOR, t))])
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(t, minor, r)| (KeySignature::from_tonic(t as Pc, minor), r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Chord {
        Chord::parse(s).unwrap()
    }

    #[test]
    fn parses_and_names() {
        for (input, canon) in [
            ("C", "C"),
            ("Am7", "Am7"),
            ("F#m7b5", "F#m7b5"),
            ("Bbmaj7", "Bbmaj7"),
            ("C/E", "C/E"),
            ("Dø", "Dm7b5"),
            ("G7sus", "G7sus4"),
            ("Ebmin", "Ebm"),
            ("C6/9", "C6/9"),
            ("C6/9/E", "C6/9/E"),
            ("Ab-7", "Abm7"),
        ] {
            assert_eq!(c(input).name(Spelling::Mixed), canon, "{input}");
        }
        assert!(matches!(Chord::parse("H7"), Err(ChordError::BadRoot(_))));
        assert!(matches!(Chord::parse("Cfoo"), Err(ChordError::BadQuality(_))));
    }

    #[test]
    fn identifies_common_chords() {
        let n = |ps: &[u8]| -> Vec<Note> {
            ps.iter().map(|&p| Note { pitch: p, velocity: 90, start: 0, duration: 480 }).collect()
        };
        let top = |ps: &[u8]| identify(&n(ps))[0].chord.name(Spelling::Mixed);
        assert_eq!(top(&[60, 64, 67]), "C");
        assert_eq!(top(&[57, 60, 64]), "Am");
        assert_eq!(top(&[64, 67, 72]), "C/E");
        assert_eq!(top(&[55, 59, 62, 65]), "G7");
        assert_eq!(top(&[57, 60, 64, 67]), "Am7");
        assert_eq!(top(&[48, 64, 67, 69]), "C6");
        assert_eq!(top(&[59, 62, 65]), "Bdim");
        assert_eq!(top(&[60, 64]), "C");
        assert_eq!(top(&[62]), "D");
    }

    #[test]
    fn romans_round_trip() {
        let c_major = KeySignature::default();
        for (r, name) in [
            ("I", "C"),
            ("vi", "Am"),
            ("IV", "F"),
            ("V7", "G7"),
            ("ii7", "Dm7"),
            ("viiø7", "Bm7b5"),
            ("bVII", "Bb"),
            ("V/V", "D"),
            ("V7/ii", "A7"),
            ("I/3", "C/E"),
            ("IVmaj7", "Fmaj7"),
        ] {
            let chord = parse_roman(r, c_major).unwrap();
            assert_eq!(chord.name(Spelling::Mixed), name, "{r}");
            if !r.contains("/V") && !r.contains("/ii") {
                assert_eq!(chord.roman(c_major), r, "{name}");
            }
        }
        let a_minor = KeySignature { fifths: 0, minor: true };
        assert_eq!(parse_roman("i", a_minor).unwrap().name(Spelling::Mixed), "Am");
        assert_eq!(parse_roman("V", a_minor).unwrap().name(Spelling::Mixed), "E");
        assert_eq!(parse_chord_or_roman("bVI", c_major).unwrap().name(Spelling::Flats), "Ab");
    }

    #[test]
    fn keys() {
        assert_eq!(KeySignature::parse("Eb").unwrap().fifths, -3);
        assert_eq!(KeySignature::parse("F#m").unwrap(), KeySignature { fifths: 3, minor: true });
        assert_eq!(KeySignature::parse("Am").unwrap().tonic(), 9);
        let names: Vec<_> =
            KeySignature::default().diatonic(true).iter().map(|c| c.name(Spelling::Mixed)).collect();
        assert_eq!(names, ["Cmaj7", "Dm7", "Em7", "Fmaj7", "G7", "Am7", "Bm7b5"]);
    }

    #[test]
    fn detects_key_of_a_scale() {
        let g_major = [67, 69, 71, 72, 74, 76, 78, 79, 74, 71, 67];
        let notes: Vec<_> = g_major
            .iter()
            .enumerate()
            .map(|(i, &p)| Note { pitch: p, velocity: 90, start: i as u64 * 480, duration: 480 })
            .collect();
        assert_eq!(detect_key(&notes).unwrap().0, KeySignature { fifths: 1, minor: false });
    }

    #[test]
    fn suggestions() {
        let key = KeySignature::default();
        let names = |t: &str| -> Vec<String> {
            suggest(t, key, &[], 6).iter().map(|c| c.name(Spelling::Mixed)).collect()
        };
        assert_eq!(names("")[..4], ["C", "Dm", "Em", "F"]);
        assert_eq!(names("A")[..3], ["A", "Am", "Am7"]);
        assert_eq!(names("vi")[0], "Am");
        assert_eq!(names("G7")[0], "G7");
        assert!(names("Dm")[..3].contains(&"Dm7".to_string()));
        assert_eq!(names("am7")[0], "Am7");
    }

    #[test]
    fn voicings() {
        assert_eq!(c("Am").voicing(55), vec![57, 60, 64]);
        assert_eq!(c("C/E").voicing(55), vec![52, 60, 64, 67]);
    }
}
