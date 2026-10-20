//! Listening to audio: everything needed to turn a microphone recording of
//! an acoustic instrument into the same raw events a MIDI keyboard gives.
//!
//! - [`Resampler`] brings any input rate down to [`RATE`], the rate the
//!   journal keeps and transcription expects.
//! - [`Gate`] follows the level against the room's noise floor, so the
//!   journal can tell playing from silence the way it does with notes.
//! - [`onsets`] finds where notes and strums start (spectral flux, each
//!   onset then placed to the millisecond on the waveform).
//! - [`strums`] reads each stretch between onsets for the notes sounding
//!   (harmonic sums over a fine spectrum, tuned to the instrument) and the
//!   chord they make. Strummed guitar is mostly chords and rhythm, and this
//!   gets both, with no model.
//!
//! Nothing here touches files or devices.

use std::sync::Arc;

use compypal_core::theory::{self, Chord, Pc};
use compypal_core::{RawEvent, RawMsg};
use realfft::{RealFftPlanner, RealToComplex};

/// The journal's audio rate, and the rate Basic Pitch takes.
pub const RATE: u32 = 22_050;

// --- resampling ------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn lowpass(rate: f32, cutoff: f32) -> Self {
        let w = std::f32::consts::TAU * cutoff / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let b1 = (1.0 - cos) / a0;
        Self { b: [b1 / 2.0, b1, b1 / 2.0], a: [-2.0 * cos / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Streaming conversion of mono audio to [`RATE`]: a low-pass against
/// aliasing, then linear interpolation.
pub struct Resampler {
    step: f64,
    /// Position of the next output sample, in input samples after `prev`.
    pos: f64,
    prev: f32,
    filters: Vec<Biquad>,
}

impl Resampler {
    pub fn new(from: u32) -> Self {
        let filters = if from as f32 > RATE as f32 * 1.05 {
            vec![Biquad::lowpass(from as f32, RATE as f32 * 0.45); 2]
        } else {
            Vec::new()
        };
        Self { step: from as f64 / RATE as f64, pos: 0.0, prev: 0.0, filters }
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let filtered: Vec<f32> = input
            .iter()
            .map(|&x| self.filters.iter_mut().fold(x, |x, f| f.run(x)))
            .collect();
        // Index 0 is `prev`; the input follows.
        let at = |i: usize| if i == 0 { self.prev } else { filtered[i - 1] };
        let n = filtered.len();
        while self.pos + 1.0 < (n + 1) as f64 {
            let i = self.pos as usize;
            let f = (self.pos - i as f64) as f32;
            out.push(at(i) * (1.0 - f) + at(i + 1) * f);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = filtered[n - 1];
    }
}

// --- the gate --------------------------------------------------------------

/// Samples per level reading: about 23 ms.
pub const HOP: usize = 512;

/// Follows the level hop by hop against a noise floor that adapts to the
/// room: playing is anything well above the floor.
#[derive(Clone, Debug)]
pub struct Gate {
    /// Level of the last hop, dBFS.
    pub level: f32,
    /// The room when nobody plays, dBFS.
    pub floor: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self { level: -120.0, floor: -70.0 }
    }
}

/// How far above the floor counts as playing.
const ABOVE_FLOOR: f32 = 14.0;
/// Quieter than this is never playing, however quiet the room.
const MIN_LEVEL: f32 = -58.0;

impl Gate {
    /// Takes one hop of samples; true when it is playing.
    pub fn push(&mut self, hop: &[f32]) -> bool {
        let rms = (hop.iter().map(|s| s * s).sum::<f32>() / hop.len().max(1) as f32).sqrt();
        self.level = 20.0 * (rms + 1e-9).log10();
        let loud = self.level > (self.floor + ABOVE_FLOOR).max(MIN_LEVEL);
        if self.level < self.floor {
            // Quiet moments pull the floor down quickly...
            self.floor += (self.level - self.floor) * 0.3;
        } else if !loud {
            // ...and a noisier room raises it slowly; playing never does.
            self.floor += 0.04;
        }
        loud
    }
}

// --- spectra ---------------------------------------------------------------

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect()
}

struct Fft {
    plan: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    buf: Vec<f32>,
    spec: Vec<realfft::num_complex::Complex<f32>>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let plan = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let spec = plan.make_output_vec();
        Self { plan, window: hann(n), buf: vec![0.0; n], spec }
    }

    /// Magnitudes of `samples` (shorter is zero-padded), windowed over
    /// their own length.
    fn magnitudes(&mut self, samples: &[f32]) -> Vec<f32> {
        let n = self.buf.len();
        let len = samples.len().min(n);
        self.buf.fill(0.0);
        if len == n {
            for (i, s) in samples[..n].iter().enumerate() {
                self.buf[i] = s * self.window[i];
            }
        } else {
            let w = hann(len);
            for i in 0..len {
                self.buf[i] = samples[i] * w[i];
            }
        }
        let _ = self.plan.process(&mut self.buf, &mut self.spec);
        self.spec.iter().map(|c| c.norm()).collect()
    }
}

// --- onsets ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    /// Seconds from the first sample.
    pub t: f64,
    /// How sharp the attack was, in arbitrary units; compare within a
    /// recording.
    pub strength: f32,
}

const ONSET_FFT: usize = 1024;
const ONSET_HOP: usize = 256;

/// Where notes and strums start. Spectral flux on a log spectrum (with a
/// little smearing across bins, so vibrato isn't an attack), peaks above a
/// running mean; then each onset is moved to where the waveform actually
/// rises. A strum's strings, a few ms apart, are one onset.
pub fn onsets(samples: &[f32], rate: u32) -> Vec<Onset> {
    let mut fft = Fft::new(ONSET_FFT);
    let top = (8_000.0 / rate as f32 * ONSET_FFT as f32) as usize;
    let mut prev: Option<Vec<f32>> = None;
    let mut flux = Vec::new();
    let mut start = 0isize - ONSET_FFT as isize / 2;
    while start < samples.len() as isize {
        let mut frame = vec![0.0f32; ONSET_FFT];
        for (i, f) in frame.iter_mut().enumerate() {
            let at = start + i as isize;
            if at >= 0 && (at as usize) < samples.len() {
                *f = samples[at as usize];
            }
        }
        // Magnitudes as amplitudes (a full-scale sine is 1), compressed.
        let mags: Vec<f32> = fft.magnitudes(&frame)[..top].iter().map(|m| (1.0 + 100.0 * m * 4.0 / ONSET_FFT as f32).ln()).collect();
        let f = match &prev {
            // Frames hanging off the start see the edge, not an attack.
            Some(p) if start >= 0 => (0..top)
                .map(|k| {
                    let before = p[k.saturating_sub(1)].max(p[k]).max(p[(k + 1).min(top - 1)]);
                    (mags[k] - before).max(0.0)
                })
                .sum::<f32>(),
            _ => 0.0,
        };
        flux.push(f);
        prev = Some(mags);
        start += ONSET_HOP as isize;
    }
    let peak = flux.iter().cloned().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return Vec::new();
    }
    // The loudest attack nearby: soft playing is judged against itself,
    // and the wobble of ringing strings against the strum that rang them.
    let near = (2.0 * rate as f64 / ONSET_HOP as f64) as usize;
    let mut out: Vec<Onset> = Vec::new();
    for i in 1..flux.len() {
        let lo = i.saturating_sub(3);
        let hi = (i + 3).min(flux.len() - 1);
        if (lo..=hi).any(|j| flux[j] > flux[i] || (flux[j] == flux[i] && j < i)) {
            continue;
        }
        let a = i.saturating_sub(20);
        let b = (i + 5).min(flux.len() - 1);
        let mean = flux[a..=b].iter().sum::<f32>() / (b - a + 1) as f32;
        let loudest = flux[i.saturating_sub(near)..(i + near).min(flux.len())].iter().cloned().fold(0.0f32, f32::max);
        if flux[i] < 1.5 * mean || flux[i] < 0.13 * loudest || flux[i] < 0.02 * peak {
            continue;
        }
        let t = (i * ONSET_HOP) as f64 / rate as f64;
        let t = refine_onset(samples, rate, t);
        match out.last_mut() {
            // One strum, several strings.
            Some(last) if t - last.t < 0.06 => last.strength = last.strength.max(flux[i]),
            _ => out.push(Onset { t, strength: flux[i] }),
        }
    }
    out
}

/// Moves an onset found on spectral frames (~12 ms apart) to where the
/// signal rises: the first millisecond whose level climbs a tenth of the
/// way from the quiet before to the peak after (a strum's first strings,
/// not its last).
fn refine_onset(samples: &[f32], rate: u32, t: f64) -> f64 {
    let ms = (rate / 1000) as usize;
    let at = (t * rate as f64) as usize;
    let from = at.saturating_sub(45 * ms);
    let to = (at + 30 * ms).min(samples.len());
    if to <= from + 2 * ms {
        return t;
    }
    // On the signal's slope rather than the signal: an attack is bright,
    // and the strings still ringing from before mostly are not.
    let slope: Vec<f32> = samples[from.max(1)..to].iter().zip(&samples[from.max(1) - 1..to - 1]).map(|(a, b)| a - b).collect();
    let env: Vec<f32> = slope.chunks(ms).map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs()))).collect();
    let (peak_i, peak) = env.iter().enumerate().fold((0, 0.0f32), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
    let quiet = env[..=peak_i].iter().cloned().fold(f32::MAX, f32::min);
    let rise = quiet + (peak - quiet) / 10.0;
    // The last quiet point before the peak, then the first rise after it.
    let low = env[..=peak_i].iter().rposition(|&v| v <= quiet + (peak - quiet) * 0.1).unwrap_or(0);
    let i = (low..=peak_i).find(|&i| env[i] >= rise).unwrap_or(peak_i);
    (from.max(1) + i * ms) as f64 / rate as f64
}

// --- pitches and chords ----------------------------------------------------

const SPECTRUM_FFT: usize = 16_384;
/// The span after an onset that is read for pitches: past the pick noise,
/// before the notes have died away.
const READ_SKIP: f64 = 0.03;
const READ_LEN: f64 = 0.5;

fn midi_freq(m: f32, tuning: f32) -> f32 {
    440.0 * 2f32.powf((m + tuning - 69.0) / 12.0)
}

/// How far (in semitones, -0.5..0.5) the instrument sits from A440, from
/// the strongest spectral peaks: a guitar a little flat still reads right.
fn tuning(spectra: &[Vec<f32>], rate: u32) -> f32 {
    let bin_hz = rate as f32 / SPECTRUM_FFT as f32;
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for mags in spectra {
        let lo = (70.0 / bin_hz) as usize;
        let hi = ((1_500.0 / bin_hz) as usize).min(mags.len() - 2);
        let max = mags[lo..hi].iter().cloned().fold(0.0f32, f32::max);
        for k in lo..hi {
            if mags[k] > 0.2 * max && mags[k] > mags[k - 1] && mags[k] >= mags[k + 1] {
                // Parabolic interpolation of the peak.
                let (a, b, c) = (mags[k - 1], mags[k], mags[k + 1]);
                let d = 0.5 * (a - c) / (a - 2.0 * b + c);
                let f = (k as f32 + d) * bin_hz;
                let m = 69.0 + 12.0 * (f / 440.0).log2();
                let ang = std::f32::consts::TAU * (m - m.round());
                re += b * ang.cos();
                im += b * ang.sin();
            }
        }
    }
    if re == 0.0 && im == 0.0 { 0.0 } else { im.atan2(re) / std::f32::consts::TAU }
}

/// The strongest magnitude within a quarter tone of `f`.
fn peak_near(mags: &[f32], f: f32, bin_hz: f32) -> f32 {
    let lo = ((f * 0.9857) / bin_hz).floor().max(1.0) as usize;
    let hi = (((f * 1.0145) / bin_hz).ceil() as usize).min(mags.len() - 1);
    if lo > hi { 0.0 } else { mags[lo..=hi].iter().cloned().fold(0.0f32, f32::max) }
}

/// Notes sounding in one spectrum, strongest first: (MIDI pitch,
/// salience). A note needs its harmonics, and its own fundamental too, so
/// the octave below a note (whose harmonics it shares) isn't heard.
fn notes_in(mags: &[f32], rate: u32, tuning: f32) -> Vec<(u8, f32)> {
    let bin_hz = rate as f32 / SPECTRUM_FFT as f32;
    let max = mags.iter().cloned().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return Vec::new();
    }
    let range = 36u8..=96;
    let sal: Vec<f32> = range
        .clone()
        .map(|m| {
            let f0 = midi_freq(m as f32, tuning);
            (1..=6)
                .filter(|h| f0 * *h as f32 * 1.02 < rate as f32 / 2.0)
                .map(|h| 0.8f32.powi(h - 1) * peak_near(mags, f0 * h as f32, bin_hz))
                .sum()
        })
        .collect();
    let top = sal.iter().cloned().fold(0.0f32, f32::max);
    let mut out: Vec<(u8, f32)> = Vec::new();
    for (i, m) in range.enumerate() {
        let s = sal[i];
        let own = peak_near(mags, midi_freq(m as f32, tuning), bin_hz);
        let neighbours = sal.get(i.wrapping_sub(1)).copied().unwrap_or(0.0).max(sal.get(i + 1).copied().unwrap_or(0.0));
        if s >= 0.25 * top && own >= 0.12 * max && s >= neighbours {
            out.push((m, s));
        }
    }
    // A note that sits on a lower note's harmonic (the octave, the 12th,
    // two octaves) is that harmonic unless its own peak is about as strong
    // as the lower note's fundamental: harmonics fall off.
    let own = |m: u8| peak_near(mags, midi_freq(m as f32, tuning), bin_hz);
    let snapshot = out.clone();
    out.retain(|(m, _)| {
        !snapshot.iter().any(|(b, _)| {
            let d = *m as i32 - *b as i32;
            (d == 12 || d == 19 || d == 24 || d == 28) && own(*m) < 0.9 * own(*b)
        })
    });
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(6);
    out
}

/// A strum, a note or a few: what was heard at one onset.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub t: f64,
    /// Everything sounding, low to high: struck now or still ringing.
    pub sounding: Vec<u8>,
    /// What was struck at this onset.
    pub struck: Vec<u8>,
    pub velocity: u8,
    /// The chord the sounding notes make, when they make one.
    pub chord: Option<Chord>,
}

/// A note as heard: struck, and rung until it stopped or was struck again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeardNote {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    pub velocity: u8,
}

/// What [`strums`] found.
#[derive(Clone, Debug, Default)]
pub struct Strums {
    pub heard: Vec<Heard>,
    pub notes: Vec<HeardNote>,
    /// How much of it was strummed (three strings or more at once), 0..1.
    pub chordal: f32,
}

impl Strums {
    /// As raw events, in seconds from the first sample.
    pub fn events(&self) -> Vec<RawEvent> {
        notes_to_events(&self.notes)
    }
}

/// Note-on and note-off events for notes, in time order (an off before an
/// on at the same moment, so a re-struck note doesn't cut itself off).
pub fn notes_to_events(notes: &[HeardNote]) -> Vec<RawEvent> {
    let mut out = Vec::new();
    for n in notes {
        out.push(RawEvent { t: n.start, channel: 0, msg: RawMsg::NoteOn { pitch: n.pitch, velocity: n.velocity } });
        out.push(RawEvent { t: n.end, channel: 0, msg: RawMsg::NoteOff { pitch: n.pitch } });
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t).then(matches!(a.msg, RawMsg::NoteOn { .. }).cmp(&matches!(b.msg, RawMsg::NoteOn { .. }))));
    out
}

/// Struck rather than still ringing: its fundamental is clearly stronger
/// after the onset than just before it.
const STRUCK_RISE: f32 = 1.15;

/// Reads a recording as strums: at each onset, the notes sounding, which
/// of them were struck, and the chord they make. A note rings on until
/// it's no longer heard, it's struck again, or the sound dies away.
/// Velocity follows the attack, so accents (the downstrokes on the beat)
/// stay accents for the tempo finder.
pub fn strums(samples: &[f32], rate: u32) -> Strums {
    let ons = onsets(samples, rate);
    if ons.is_empty() {
        return Strums::default();
    }
    let at = |t: f64| ((t.max(0.0) * rate as f64) as usize).min(samples.len());
    let mut fft = Fft::new(SPECTRUM_FFT);
    let total = samples.len() as f64 / rate as f64;
    let spans: Vec<(f64, f64)> = ons
        .iter()
        .enumerate()
        .map(|(i, o)| (o.t, ons.get(i + 1).map_or(total, |n| n.t)))
        .collect();
    let spectra: Vec<Vec<f32>> = spans
        .iter()
        .map(|&(a, b)| {
            let from = at(a + READ_SKIP.min((b - a) / 4.0));
            let to = at(b.min(a + READ_SKIP + READ_LEN)).max(from);
            fft.magnitudes(&samples[from..to])
        })
        .collect();
    // Just before each onset: what was ringing.
    let before: Vec<Vec<f32>> = spans.iter().map(|&(a, _)| fft.magnitudes(&samples[at(a - 0.12)..at(a - 0.005)])).collect();
    let tune = tuning(&spectra, rate);
    let bin_hz = rate as f32 / SPECTRUM_FFT as f32;
    // Loudness per 10 ms, to see where the sound has died away.
    let win = (rate / 100) as usize;
    let env: Vec<f32> = samples.chunks(win).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
    let mut strengths: Vec<f32> = ons.iter().map(|o| o.strength).collect();
    strengths.sort_by(f32::total_cmp);
    let loud = strengths[(strengths.len() * 9 / 10).min(strengths.len() - 1)].max(1e-6);

    // First what each onset sounds like: what's sounding, what was struck.
    let read: Vec<(Vec<(u8, f32)>, Vec<u8>, Vec<u8>)> = (0..ons.len())
        .map(|i| {
            let (a, b) = spans[i];
            let found = notes_in(&spectra[i], rate, tune);
            // A window's magnitudes grow with its length: compare per second.
            let read_len = (b.min(a + READ_SKIP + READ_LEN) - (a + READ_SKIP.min((b - a) / 4.0))).max(1e-3);
            let before_len = (a - 0.005 - (a - 0.12).max(0.0)).max(1e-3);
            let struck_now = |m: u8| {
                let f = midi_freq(m as f32, tune);
                // The fundamental alone: a low string's harmonics are often
                // a new chord's own notes.
                let now = peak_near(&spectra[i], f, bin_hz) / read_len as f32;
                let was = peak_near(&before[i], f, bin_hz) / before_len as f32;
                now > STRUCK_RISE * was
            };
            let mut sounding: Vec<u8> = found.iter().map(|n| n.0).collect();
            sounding.sort();
            let struck: Vec<u8> = sounding.iter().copied().filter(|&m| struck_now(m)).collect();
            (found, sounding, struck)
        })
        .collect();
    // Strummed playing: plenty of onsets clearly strike three strings or
    // more (downstrokes; the upstrokes between barely raise some). Then a
    // light stroke over a ringing chord strikes it all.
    let strumming = read.iter().filter(|r| r.2.len() >= 3).count() * 5 >= read.len() * 2;

    let mut heard = Vec::new();
    let mut notes: Vec<HeardNote> = Vec::new();
    // Notes ringing: index into `notes`.
    let mut ringing: Vec<usize> = Vec::new();
    let mut strummed = 0usize;
    // The bass of the last clear strum: a light stroke after it doesn't
    // reach below it (what rings down there is left from before).
    let mut last_bass: Option<(u8, f32)> = None;
    for (i, o) in ons.iter().enumerate() {
        let (a, b) = spans[i];
        let (found, mut sounding, mut struck) = read[i].clone();
        let light = last_bass.is_some_and(|(_, strength)| o.strength < 0.6 * strength);
        if struck.len() >= 3 && !(strumming && light) {
            last_bass = struck.first().map(|p| (*p, o.strength));
        } else if strumming && sounding.len() >= 3 {
            // In strummed playing a stroke strikes every string that
            // sounds, from the last strum's bass up.
            let floor = last_bass.map_or(0, |b| b.0);
            sounding.retain(|p| *p >= floor);
            struck = sounding.clone();
        } else if struck.is_empty() {
            // Something was played here.
            struck = sounding.clone();
        }
        // A strum is named by the strings it struck: changing chords stops
        // the old one, so what still rings counts only if it fits the new
        // chord above its bass. Otherwise (a note over ringing strings) the
        // chord is everything sounding.
        let strum = struck.len() >= 3;
        let named: Vec<(u8, f32)> = found.iter().copied().filter(|(p, _)| !strum || struck.contains(p)).collect();
        let mut chord = None;
        if named.len() >= 3 {
            let mut w = [0.0f64; 12];
            for (p, s) in &named {
                w[(p % 12) as usize] += *s as f64;
            }
            let bass = named.iter().map(|n| n.0).min().map(|p| p % 12 as Pc);
            if let Some(fit) = theory::best_fit(&w, bass).filter(|f| f.score > 0.3) {
                chord = Some(fit.chord);
                if strum {
                    // What the chord doesn't explain is overtones and noise.
                    struck.retain(|p| fit.chord.contains(*p));
                    let low = struck.first().copied().unwrap_or(0);
                    sounding.retain(|p| struck.contains(p) || (fit.chord.contains(*p) && *p > low));
                }
            }
        }
        if struck.len() >= 3 {
            strummed += 1;
        }
        // What stopped ringing, or is struck again, ends here.
        ringing.retain(|&k| {
            let p = notes[k].pitch;
            let goes_on = sounding.contains(&p) && !struck.contains(&p);
            if !goes_on {
                notes[k].end = a;
            }
            goes_on
        });
        let velocity = (30.0 + 97.0 * ((o.strength / loud).min(1.3) / 1.3).powf(0.6)).clamp(1.0, 127.0) as u8;
        for &p in &struck {
            ringing.push(notes.len());
            notes.push(HeardNote { start: a, end: b, pitch: p, velocity });
        }
        // Whatever rings on lasts to the next onset, or until the sound has
        // faded by 30 dB.
        let e0 = ((a * 100.0) as usize).min(env.len().saturating_sub(1));
        let e1 = ((b * 100.0) as usize).clamp(e0 + 1, env.len().max(e0 + 1));
        let top = env[e0..e1.min(env.len())].iter().cloned().fold(0.0f32, f32::max);
        let faded = (e0..e1.min(env.len())).find(|&k| k > e0 + 5 && env[k] < top * 0.03).map(|k| k as f64 / 100.0);
        for &k in &ringing {
            notes[k].end = faded.unwrap_or(b).min(b - 0.005).max(notes[k].start + 0.05);
        }
        if faded.is_some() {
            ringing.clear();
        }
        heard.push(Heard { t: a, sounding, struck, velocity, chord });
    }
    let chordal = if heard.is_empty() { 0.0 } else { strummed as f32 / heard.len() as f32 };
    Strums { heard, notes, chordal }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A plucked string: harmonics falling off, decaying.
    pub fn pluck(out: &mut [f32], rate: u32, at: f64, pitch: u8, amp: f32) {
        let f0 = 440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0);
        let start = (at * rate as f64) as usize;
        for (i, s) in out.iter_mut().skip(start).enumerate() {
            let t = i as f32 / rate as f32;
            let env = amp * (-t / 0.7).exp() * (t / 0.002).min(1.0);
            if t > 0.01 && env < 1e-5 {
                break;
            }
            let mut v = 0.0;
            for h in 1..=8 {
                let f = f0 * h as f32;
                if f < rate as f32 / 2.0 {
                    v += (std::f32::consts::TAU * f * t).sin() / h as f32 * (-t * h as f32 / 1.5).exp();
                }
            }
            *s += env * v;
        }
    }

    /// Strummed chords: one per bar of four quarter-note strums, the
    /// strings 8 ms apart, beats 1 and 3 louder.
    pub fn strummed(chords: &[&[u8]], bpm: f64, rate: u32) -> (Vec<f32>, Vec<f64>) {
        let beat = 60.0 / bpm;
        let lead = 0.5;
        let mut out = vec![0.0f32; ((lead + chords.len() as f64 * 4.0 * beat + 2.0) * rate as f64) as usize];
        let mut times = Vec::new();
        for (c, chord) in chords.iter().enumerate() {
            for b in 0..4 {
                let t = lead + (c * 4 + b) as f64 * beat;
                times.push(t);
                let amp = if b % 2 == 0 { 0.12 } else { 0.06 };
                for (s, p) in chord.iter().enumerate() {
                    pluck(&mut out, rate, t + s as f64 * 0.008, *p, amp);
                }
            }
        }
        // A quiet room.
        let mut x = 0.123f32;
        for s in out.iter_mut() {
            x = (x * 9.731 + 0.317).fract();
            *s += (x - 0.5) * 0.0005;
        }
        (out, times)
    }

    pub const G: &[u8] = &[43, 47, 50, 55, 59, 67];
    pub const C: &[u8] = &[48, 52, 55, 60, 64];
    pub const D: &[u8] = &[50, 57, 62, 66];
    pub const EM: &[u8] = &[40, 47, 52, 55, 59, 64];

    #[test]
    fn resamples_to_the_journal_rate() {
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        // A 440 Hz tone in odd-sized blocks.
        let tone: Vec<f32> = (0..48_000).map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin()).collect();
        for block in tone.chunks(333) {
            r.push(block, &mut out);
        }
        assert!((out.len() as i64 - RATE as i64).abs() <= 2, "{}", out.len());
        // Still 440 Hz: count rising zero crossings over the second.
        let crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((crossings as i64 - 440).abs() <= 2, "{crossings}");
    }

    #[test]
    fn the_gate_hears_playing_over_the_room() {
        let mut g = Gate::default();
        let mut x = 0.3f32;
        let mut hop = |level: f32, tone: bool, n: usize| {
            (0..n)
                .map(|k| {
                    let buf: Vec<f32> = (0..HOP)
                        .map(|i| {
                            x = (x * 7.77 + 0.21).fract();
                            let noise = (x - 0.5) * 0.002;
                            noise + if tone { level * ((k * HOP + i) as f32 * 0.06).sin() } else { 0.0 }
                        })
                        .collect();
                    g.push(&buf)
                })
                .collect::<Vec<bool>>()
        };
        assert!(hop(0.0, false, 100).iter().all(|l| !l), "the room alone");
        assert!(hop(0.05, true, 40).iter().all(|l| *l), "playing");
        assert!(hop(0.0, false, 40).iter().skip(1).all(|l| !l), "and quiet again");
    }

    #[test]
    fn finds_clicks_to_the_millisecond() {
        let rate = RATE;
        let mut s = vec![0.0f32; rate as usize * 4];
        let times: Vec<f64> = (0..7).map(|i| 0.3 + i as f64 * 0.5).collect();
        for &t in &times {
            let at = (t * rate as f64) as usize;
            for i in 0..200 {
                s[at + i] = ((i as f32 * 1.3).sin()) * 0.5 * (-(i as f32) / 60.0).exp();
            }
        }
        let found = onsets(&s, rate);
        assert_eq!(found.len(), times.len(), "{found:?}");
        for (o, t) in found.iter().zip(&times) {
            assert!((o.t - t).abs() < 0.004, "{} vs {t}", o.t);
        }
    }

    #[test]
    fn reads_strummed_chords() {
        let (audio, times) = strummed(&[G, C, D, EM], 100.0, RATE);
        let s = strums(&audio, RATE);
        assert_eq!(s.heard.len(), times.len(), "{:?}", s.heard.iter().map(|h| h.t).collect::<Vec<_>>());
        for (h, t) in s.heard.iter().zip(&times) {
            // Within the strum: its strings take 40 ms.
            assert!(h.t - t > -0.005 && h.t - t < 0.04, "{} vs {t}", h.t);
        }
        let names: Vec<String> = s.heard.iter().map(|h| h.chord.map(|c| c.name(theory::Spelling::Sharps)).unwrap_or_default()).collect();
        assert_eq!(names.iter().step_by(4).cloned().collect::<Vec<_>>(), ["G", "C", "D", "Em"], "{names:?}");
        assert!(s.chordal > 0.7, "{}", s.chordal);
        // Accents survive: beat 1 is louder than beat 2.
        assert!(s.heard[0].velocity > s.heard[1].velocity + 10, "{} {}", s.heard[0].velocity, s.heard[1].velocity);
        // The bass is the chord's.
        assert_eq!(s.heard[4].sounding[0] % 12, 0, "C's root in the bass: {:?}", s.heard[4].sounding);
    }

    #[test]
    fn a_flat_guitar_still_reads_right() {
        // A whole guitar 30 cents flat: synthesised at a slightly higher
        // rate than it's read at.
        let rate = RATE;
        let mut audio = vec![0.0f32; rate as usize * 3];
        let f = 2f32.powf(-0.3 / 12.0);
        for (s, &p) in C.iter().enumerate() {
            let mut one = vec![0.0f32; audio.len()];
            pluck(&mut one, (rate as f32 / f) as u32, 0.3 + s as f64 * 0.008, p, 0.1);
            for (a, b) in audio.iter_mut().zip(&one) {
                *a += b;
            }
        }
        let s = strums(&audio, rate);
        assert_eq!(s.heard.len(), 1);
        assert_eq!(s.heard[0].chord.map(|c| c.name(theory::Spelling::Sharps)).as_deref(), Some("C"), "{:?}", s.heard);
    }

    #[test]
    fn single_notes_are_single_notes() {
        let rate = RATE;
        let mut audio = vec![0.0f32; rate as usize * 4];
        let line = [57u8, 60, 64, 62, 60, 59];
        for (i, &p) in line.iter().enumerate() {
            pluck(&mut audio, rate, 0.2 + i as f64 * 0.4, p, 0.1);
        }
        let s = strums(&audio, rate);
        let got: Vec<Vec<u8>> = s.heard.iter().map(|h| h.struck.clone()).collect();
        assert_eq!(got, line.iter().map(|p| vec![*p]).collect::<Vec<_>>());
        assert!(s.chordal < 0.1);
    }

    #[test]
    fn down_and_up_strums() {
        // Eighths: downstrokes on the beat, lighter upstrokes between, the
        // strings in reverse; a chord a bar.
        let beat = 60.0 / 96.0;
        let mut audio = vec![0.0f32; ((0.5 + 16.0 * beat + 3.0) * RATE as f64) as usize];
        let mut times = Vec::new();
        for (bar, chord) in [G, C, D, EM].iter().enumerate() {
            for e in 0..8 {
                let t = 0.5 + (bar * 4) as f64 * beat + e as f64 * beat / 2.0;
                times.push(t);
                let amp = match e {
                    0 | 4 => 0.12,
                    _ if e % 2 == 0 => 0.08,
                    _ => 0.04,
                };
                for (s, &p) in chord.iter().enumerate() {
                    let string = if e % 2 == 0 { s } else { chord.len() - 1 - s };
                    pluck(&mut audio, RATE, t + string as f64 * 0.007, p, amp);
                }
            }
        }
        let s = strums(&audio, RATE);
        assert!(s.heard.len() >= 26, "most strokes heard: {}", s.heard.len());
        for h in &s.heard {
            assert!(times.iter().any(|t| (h.t - t).abs() < 0.05), "nothing was played at {}", h.t);
        }
        // Each bar's first strum names its chord, with nothing left over
        // from the bar before.
        for (bar, want) in ["G", "C", "D", "Em"].iter().enumerate() {
            let t = times[bar * 8];
            let h = s.heard.iter().find(|h| (h.t - t).abs() < 0.05).unwrap();
            assert_eq!(h.chord.map(|c| c.name(theory::Spelling::Sharps)).as_deref(), Some(*want), "{h:?}");
        }
    }
}
