//! Polyphonic transcription with Spotify's Basic Pitch (Apache-2.0): a
//! small network that takes 2 s windows of 22.05 kHz audio and gives, per
//! frame of 256 samples and per piano key, how likely a note is sounding
//! there and how likely one starts. [`notes`] turns those into notes the
//! way the reference implementation does (onset peaks, followed while the
//! note stays loud, then the strongest leftovers).
//!
//! The model is not built in: it is loaded from `COMPYPAL_BASIC_PITCH` or
//! from `models/basic-pitch.onnx` in compypal's data directory. Without it,
//! audio sketches are read by [`crate::analysis::strums`] alone.

use std::path::{Path, PathBuf};

use tract_onnx::prelude::*;

use crate::analysis::{HeardNote, RATE};

const HOP: usize = 256;
/// Samples per window the model takes: 2 s less one hop.
const WINDOW: usize = RATE as usize * 2 - HOP;
/// Frames of overlap between windows; half is dropped from each side.
const OVERLAP_FRAMES: usize = 30;
const OVERLAP: usize = OVERLAP_FRAMES * HOP;
const WINDOW_HOP: usize = WINDOW - OVERLAP;
/// Frames per second as the reference counts them (integer division).
const FPS: usize = RATE as usize / HOP;
const FRAMES_PER_WINDOW: usize = FPS * 2;
pub const KEYS: usize = 88;
const LOWEST: u8 = 21;

/// Where the model file is looked for.
pub fn model_path(data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COMPYPAL_BASIC_PITCH") {
        return Some(PathBuf::from(p)).filter(|p| p.exists());
    }
    data_dir.map(|d| d.join("models").join("basic-pitch.onnx")).filter(|p| p.exists())
}

type Plan = std::sync::Arc<TypedSimplePlan>;

pub struct BasicPitch {
    plan: Plan,
    /// Which outputs are the note and onset posteriors.
    note: usize,
    onset: usize,
}

/// Per frame and key, 0..1.
#[derive(Clone, Debug, Default)]
pub struct Posteriors {
    pub note: Vec<[f32; KEYS]>,
    pub onset: Vec<[f32; KEYS]>,
}

impl BasicPitch {
    pub fn load(path: &Path) -> Result<Self, String> {
        let err = |e: TractError| format!("Basic Pitch model {}: {e}", path.display());
        let model = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(err)?
            .with_input_fact(0, f32::fact([1, WINDOW, 1]).into())
            .map_err(err)?;
        // Outputs: contour (264 bins), note and onset (88 keys each). The
        // converted model lists them as `StatefulPartitionedCall:N`, N in
        // TensorFlow's (alphabetical) order: 0 contour, 1 note, 2 onset;
        // not necessarily in that order. A model naming them says so.
        let names: Vec<String> = model.output_outlets().map_err(err)?.iter().map(|o| model.node(o.node).name.clone()).collect();
        let find = |want: &str, n: &str| {
            names.iter().position(|x| x.to_lowercase().contains(want)).or_else(|| names.iter().position(|x| x.ends_with(n)))
        };
        let (note, onset) = match (find("note", ":1"), find("onset", ":2")) {
            (Some(n), Some(o)) => (n, o),
            _ => return Err(format!("Basic Pitch model {}: can't tell its outputs apart ({names:?})", path.display())),
        };
        let plan = model.into_optimized().map_err(err)?.into_runnable().map_err(err)?;
        let bp = Self { plan, note, onset };
        let probe = bp.run_window(&vec![0.0; WINDOW])?;
        if [note, onset].iter().any(|&i| probe.get(i).and_then(|t| t.shape().last().copied()) != Some(KEYS)) {
            return Err("Basic Pitch model: unexpected output shapes".into());
        }
        Ok(bp)
    }

    fn run_window(&self, window: &[f32]) -> Result<TVec<TValue>, String> {
        let input = tract_ndarray::Array3::from_shape_vec((1, WINDOW, 1), window.to_vec()).map_err(|e| e.to_string())?;
        self.plan.run(tvec!(Tensor::from(input).into())).map_err(|e| e.to_string())
    }

    /// The model's posteriors for a whole recording at [`RATE`]: windows
    /// overlapping by 30 frames, the overlap trimmed, stitched together.
    pub fn posteriors(&self, audio: &[f32]) -> Result<Posteriors, String> {
        let mut padded = vec![0.0f32; OVERLAP / 2];
        padded.extend_from_slice(audio);
        let mut out = Posteriors::default();
        let mut start = 0;
        while start < padded.len() {
            let mut window = vec![0.0f32; WINDOW];
            let end = (start + WINDOW).min(padded.len());
            window[..end - start].copy_from_slice(&padded[start..end]);
            let result = self.run_window(&window)?;
            for (which, into) in [(self.note, &mut out.note), (self.onset, &mut out.onset)] {
                let view = result[which].to_plain_array_view::<f32>().map_err(|e| e.to_string())?;
                let frames = view.shape()[1];
                let keep = OVERLAP_FRAMES / 2..frames.saturating_sub(OVERLAP_FRAMES / 2);
                for f in keep {
                    let mut row = [0.0f32; KEYS];
                    for (k, r) in row.iter_mut().enumerate() {
                        *r = view[[0, f, k]];
                    }
                    into.push(row);
                }
            }
            start += WINDOW_HOP;
        }
        let frames = audio.len() * FPS / RATE as usize;
        out.note.truncate(frames);
        out.onset.truncate(frames);
        Ok(out)
    }

    /// Notes in a recording at [`RATE`], seconds from its first sample.
    pub fn transcribe(&self, audio: &[f32]) -> Result<Vec<HeardNote>, String> {
        Ok(notes(&self.posteriors(audio)?, &Settings::default()))
    }
}

/// The reference's defaults.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub onset_threshold: f32,
    pub frame_threshold: f32,
    /// Frames; 11 is about 128 ms.
    pub min_note_frames: usize,
    /// Frames below the threshold a note may sag before it's over.
    pub energy_tolerance: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { onset_threshold: 0.5, frame_threshold: 0.3, min_note_frames: 11, energy_tolerance: 11 }
    }
}

/// When frame `f` is, in seconds: each window's frames run a little long,
/// which the reference corrects per window.
fn frame_time(f: usize) -> f64 {
    let window_offset = HOP as f64 / RATE as f64 * (FRAMES_PER_WINDOW as f64 - WINDOW as f64 / HOP as f64) + 0.0018;
    f as f64 * HOP as f64 / RATE as f64 - window_offset * (f / FRAMES_PER_WINDOW) as f64
}

/// Notes from posteriors, as Basic Pitch's `output_to_notes_polyphonic`
/// makes them (with inferred onsets and the "melodia trick").
pub fn notes(post: &Posteriors, s: &Settings) -> Vec<HeardNote> {
    let frames = &post.note;
    let n = frames.len();
    if n < 3 {
        return Vec::new();
    }
    // Onsets also where a note's frames jump up, scaled like the onsets.
    let mut onsets = post.onset.clone();
    let mut diff = vec![[0.0f32; KEYS]; n];
    for t in 2..n {
        for k in 0..KEYS {
            let d1 = frames[t][k] - frames[t - 1][k];
            let d2 = frames[t][k] - frames[t - 2][k];
            diff[t][k] = d1.min(d2).max(0.0);
        }
    }
    let max_onset = onsets.iter().flatten().cloned().fold(0.0f32, f32::max);
    let max_diff = diff.iter().flatten().cloned().fold(0.0f32, f32::max);
    if max_diff > 0.0 {
        for t in 0..n {
            for k in 0..KEYS {
                onsets[t][k] = onsets[t][k].max(diff[t][k] * max_onset / max_diff);
            }
        }
    }
    // Onset peaks in time, latest first.
    let mut peaks: Vec<(usize, usize)> = Vec::new();
    for t in 1..n - 1 {
        for k in 0..KEYS {
            let v = onsets[t][k];
            if v > onsets[t - 1][k] && v > onsets[t + 1][k] && v >= s.onset_threshold {
                peaks.push((t, k));
            }
        }
    }
    peaks.sort_by(|a, b| b.cmp(a));
    let mut energy = frames.clone();
    let mut out: Vec<(usize, usize, usize, f32)> = Vec::new();
    let clear = |energy: &mut Vec<[f32; KEYS]>, t: usize, k: usize| {
        energy[t][k] = 0.0;
        if k + 1 < KEYS {
            energy[t][k + 1] = 0.0;
        }
        if k > 0 {
            energy[t][k - 1] = 0.0;
        }
    };
    let mean = |a: usize, b: usize, k: usize| (a..b).map(|t| frames[t][k]).sum::<f32>() / (b - a).max(1) as f32;
    for (start, k) in peaks {
        if start >= n - 1 {
            continue;
        }
        let (mut i, mut low) = (start + 1, 0);
        while i < n - 1 && low < s.energy_tolerance {
            if energy[i][k] < s.frame_threshold { low += 1 } else { low = 0 }
            i += 1;
        }
        i -= low;
        if i - start <= s.min_note_frames {
            continue;
        }
        for t in start..i {
            clear(&mut energy, t, k);
        }
        out.push((start, i, k, mean(start, i, k)));
    }
    // Then notes with no onset of their own: the strongest energy left,
    // followed both ways.
    loop {
        let mut best = (0usize, 0usize, 0.0f32);
        for (t, row) in energy.iter().enumerate() {
            for (k, &v) in row.iter().enumerate() {
                if v > best.2 {
                    best = (t, k, v);
                }
            }
        }
        let (mid, k, v) = best;
        if v <= s.frame_threshold {
            break;
        }
        energy[mid][k] = 0.0;
        let (mut i, mut low) = (mid + 1, 0);
        while i < n - 1 && low < s.energy_tolerance {
            if energy[i][k] < s.frame_threshold { low += 1 } else { low = 0 }
            clear(&mut energy, i, k);
            i += 1;
        }
        let end = i - 1 - low;
        let (mut i, mut low) = (mid as isize - 1, 0);
        while i > 0 && low < s.energy_tolerance {
            if energy[i as usize][k] < s.frame_threshold { low += 1 } else { low = 0 }
            clear(&mut energy, i as usize, k);
            i -= 1;
        }
        let start = (i + 1 + low as isize).max(0) as usize;
        if end <= start || end - start <= s.min_note_frames {
            continue;
        }
        out.push((start, end, k, mean(start, end, k)));
    }
    let mut notes: Vec<HeardNote> = out
        .into_iter()
        .map(|(a, b, k, amp)| HeardNote {
            start: frame_time(a).max(0.0),
            end: frame_time(b),
            pitch: LOWEST + k as u8,
            velocity: (amp * 127.0).round().clamp(1.0, 127.0) as u8,
        })
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posteriors_become_notes() {
        // A held C4 with a clear onset, and an E4 that fades in with none.
        let n = 200;
        let mut p = Posteriors { note: vec![[0.0; KEYS]; n], onset: vec![[0.0; KEYS]; n] };
        let (c, e) = (60 - LOWEST as usize, 64 - LOWEST as usize);
        p.onset[20][c] = 0.9;
        for f in 20..80 {
            p.note[f][c] = 0.8;
        }
        for f in 100..160 {
            p.note[f][e] = 0.2 + 0.5 * ((f - 100) as f32 / 60.0).min(1.0) + 0.05;
        }
        let got = notes(&p, &Settings::default());
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].pitch, got[1].pitch), (60, 64));
        assert!((got[0].start - 20.0 * 256.0 / 22050.0).abs() < 0.001);
        assert!((got[0].end - 80.0 * 256.0 / 22050.0).abs() < 0.02, "{got:?}");
        assert_eq!(got[0].velocity, (0.8f32 * 127.0).round() as u8);
        // Too short to be a note.
        let mut q = Posteriors { note: vec![[0.0; KEYS]; n], onset: vec![[0.0; KEYS]; n] };
        q.onset[20][c] = 0.9;
        for f in 20..26 {
            q.note[f][c] = 0.8;
        }
        assert!(notes(&q, &Settings::default()).is_empty());
    }

    /// Runs the real model when it's there (`COMPYPAL_BASIC_PITCH`).
    #[test]
    fn transcribes_plucked_notes_with_the_model() {
        let Some(path) = model_path(None) else {
            eprintln!("no Basic Pitch model; set COMPYPAL_BASIC_PITCH to test it");
            return;
        };
        let bp = BasicPitch::load(&path).unwrap();
        let mut audio = vec![0.0f32; RATE as usize * 4];
        let line = [57u8, 60, 64, 69];
        for (i, &p) in line.iter().enumerate() {
            crate::analysis::tests::pluck(&mut audio, RATE, 0.3 + i as f64 * 0.8, p, 0.2);
        }
        let got = bp.transcribe(&audio).unwrap();
        for (i, &p) in line.iter().enumerate() {
            let n = got.iter().find(|n| n.pitch == p).unwrap_or_else(|| panic!("{p} missing: {got:?}"));
            assert!((n.start - (0.3 + i as f64 * 0.8)).abs() < 0.05, "{n:?}");
        }
        // Anything else is quiet: harmonics heard faintly as notes.
        assert!(got.iter().all(|n| line.contains(&n.pitch) || n.velocity < 60), "{got:?}");
    }
}
