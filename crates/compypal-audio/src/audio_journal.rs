//! The journal's ears: audio from [`crate::mic`] is cut into jams at long
//! silences (the gate's, not the notes'), each jam kept as a WAV file,
//! then transcribed in the background into the same raw events a MIDI
//! keyboard gives, and split into sketches like any other playing.
//!
//! Under the journal directory: `audio/<file>.wav` (22.05 kHz mono, kept
//! as recorded) and `audio/<file>.json` (its [`Transcript`]); `<file>` is
//! the unix second the recording starts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use compypal_core::jam::{self, GAP_SECONDS, Sketch};
use compypal_core::{AudioRef, RawEvent};
use serde::{Deserialize, Serialize};

use crate::analysis::{self, Gate, HOP, RATE};
use crate::basic_pitch::BasicPitch;
use crate::journal::{State, append};
use crate::wav::{self, WavWriter};

/// Audio from the input, `t` the unix time of its first sample.
pub struct Chunk {
    pub t: f64,
    pub samples: Vec<f32>,
}

/// Which transcription stands for a recording.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reading {
    /// Strums and their chords ([`analysis::strums`]): strummed playing.
    Strums,
    /// Every note Basic Pitch hears: picking, several voices at once.
    Notes,
    /// One line ([`analysis::line`]): a voice, a whistle, single notes.
    Line,
}

/// Which version of the transcription a transcript was made by. Raise it
/// when reading changes enough to be worth redoing old recordings: they
/// are written down again at the next start (the audio stays as it was).
pub const TRANSCRIPT_VERSION: u32 = 4;

/// A recording, written down.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transcript {
    /// [`TRANSCRIPT_VERSION`] when it was made; 0 before versions.
    #[serde(default)]
    pub version: u32,
    /// Unix time of the first sample.
    pub start: f64,
    pub seconds: f64,
    pub reading: Reading,
    /// How much of it was strummed chords, 0..1.
    pub chordal: f32,
    /// As strums, in unix time.
    pub strums: Vec<RawEvent>,
    /// As notes, when the Basic Pitch model was there to hear them.
    #[serde(default)]
    pub notes: Option<Vec<RawEvent>>,
    /// As one line, by its pitch.
    #[serde(default)]
    pub line: Option<Vec<RawEvent>>,
    /// How much of it was one clear pitch at a time, 0..1.
    #[serde(default)]
    pub monophony: f32,
}

impl Transcript {
    pub fn events(&self) -> &[RawEvent] {
        match (self.reading, &self.notes, &self.line) {
            (Reading::Notes, Some(n), _) => n,
            (Reading::Line, _, Some(l)) => l,
            _ => &self.strums,
        }
    }
}

/// Writes down a recording at [`RATE`], every way it can, and picks the
/// reading that suits it: strummed playing as strums (chords on their
/// rhythm, cleaner than every string's note); one voice or one line by
/// its pitch (a polyphonic model hears a voice's harmonics as notes, and
/// splits its vibrato); anything else as Basic Pitch's notes when the
/// model is there, else as strums.
pub fn transcribe(samples: &[f32], start: f64, model: Option<&BasicPitch>) -> Transcript {
    let strums = analysis::strums(samples, RATE);
    let shift = |ev: Vec<RawEvent>| ev.into_iter().map(|e| RawEvent { t: e.t + start, ..e }).collect::<Vec<_>>();
    let notes = model.and_then(|m| match m.transcribe(samples) {
        Ok(n) => Some(shift(analysis::notes_to_events(&n))),
        Err(e) => {
            eprintln!("transcription: {e}");
            None
        }
    });
    let line = analysis::line(samples, RATE);
    let reading = if strums.chordal >= 0.5 {
        Reading::Strums
    } else if line.monophony >= 0.6 {
        Reading::Line
    } else if notes.is_some() {
        Reading::Notes
    } else {
        Reading::Strums
    };
    Transcript {
        version: TRANSCRIPT_VERSION,
        start,
        seconds: samples.len() as f64 / RATE as f64,
        reading,
        chordal: strums.chordal,
        strums: shift(strums.events()),
        notes,
        line: Some(shift(line.events())),
        monophony: line.monophony,
    }
}

/// The sketches in a transcribed recording, each pointing back at it.
pub fn sketches_of(file: u64, t: &Transcript) -> Vec<Sketch> {
    let events = t.events();
    jam::split_sketches(events)
        .into_iter()
        .filter_map(|(from, to)| {
            let slice: Vec<RawEvent> = events.iter().filter(|e| e.t >= from && e.t <= to).copied().collect();
            jam::summarize(&slice)
        })
        .map(|s| Sketch { audio: Some(AudioRef { file, start: t.start }), ..s })
        .collect()
}

pub fn wav_path(dir: &Path, file: u64) -> PathBuf {
    dir.join(format!("{file}.wav"))
}

pub fn transcript_path(dir: &Path, file: u64) -> PathBuf {
    dir.join(format!("{file}.json"))
}

/// A recording waiting to be written down.
pub struct Job {
    pub file: u64,
    pub start: f64,
    /// Written down before, by an older version: its sketches are
    /// replaced, and the recording is kept whatever comes of it.
    pub redo: bool,
}

/// A jam being recorded.
struct Open {
    file: u64,
    start: f64,
    writer: WavWriter,
    /// Frames written when it was last loud.
    last_loud: u64,
}

/// How much is kept from before the gate opened, so the first attack is
/// whole.
const PREROLL: f64 = 0.5;
/// Kept after the last loud moment: the last chord's ring.
const TAIL: f64 = 1.5;

/// Takes audio as it comes and keeps the playing in it, one file per jam.
pub fn writer(dir: PathBuf, rx: Receiver<Chunk>, state: Arc<Mutex<State>>, jobs: Sender<Job>) {
    let mut gate = Gate::default();
    let mut hop: Vec<f32> = Vec::with_capacity(HOP);
    let mut hop_t = 0.0;
    let mut preroll: VecDeque<f32> = VecDeque::new();
    let mut jam: Option<Open> = None;
    // Where the stream should be next; a gap means listening stopped.
    let mut expect = f64::NEG_INFINITY;
    let mut last_heard = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => {
                last_heard = Instant::now();
                if (chunk.t - expect).abs() > 1.0 {
                    hop.clear();
                    preroll.clear();
                    if (chunk.t - expect) > GAP_SECONDS {
                        close(&dir, &mut jam, &state, &jobs);
                    }
                }
                expect = chunk.t + chunk.samples.len() as f64 / RATE as f64;
                for (i, s) in chunk.samples.iter().enumerate() {
                    if hop.is_empty() {
                        hop_t = chunk.t + i as f64 / RATE as f64;
                    }
                    hop.push(*s);
                    if hop.len() == HOP {
                        let loud = gate.push(&hop);
                        take_hop(&dir, &hop, hop_t, loud, &mut jam, &mut preroll);
                        hop.clear();
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                close(&dir, &mut jam, &state, &jobs);
                return;
            }
        }
        // Over when the stream has been quiet long enough, or has stopped
        // (listening turned off, the device gone).
        let quiet_since = jam.as_ref().map(|j| j.start + j.last_loud as f64 / RATE as f64);
        if quiet_since.is_some_and(|q| expect - q > GAP_SECONDS) || (jam.is_some() && last_heard.elapsed().as_secs_f64() > GAP_SECONDS) {
            close(&dir, &mut jam, &state, &jobs);
        }
        state.lock().unwrap().audio_live = jam.as_ref().map(|j| (j.file, j.start, j.start + j.last_loud as f64 / RATE as f64));
    }
}

fn take_hop(dir: &Path, hop: &[f32], t: f64, loud: bool, jam: &mut Option<Open>, preroll: &mut VecDeque<f32>) {
    match jam {
        Some(j) => {
            if j.writer.write(hop).is_ok() && loud {
                j.last_loud = j.writer.frames();
            }
        }
        None => {
            preroll.extend(hop);
            let keep = (PREROLL * RATE as f64) as usize;
            while preroll.len() > keep {
                preroll.pop_front();
            }
            if !loud {
                return;
            }
            let start = t + hop.len() as f64 / RATE as f64 - preroll.len() as f64 / RATE as f64;
            let mut file = start as u64;
            while wav_path(dir, file).exists() {
                file += 1;
            }
            match WavWriter::create(&wav_path(dir, file), RATE) {
                Ok(mut writer) => {
                    let lead: Vec<f32> = preroll.drain(..).collect();
                    let _ = writer.write(&lead);
                    let last_loud = writer.frames();
                    *jam = Some(Open { file, start, writer, last_loud });
                }
                Err(e) => eprintln!("audio journal: {e}"),
            }
        }
    }
}

fn close(dir: &Path, jam: &mut Option<Open>, state: &Mutex<State>, jobs: &Sender<Job>) {
    let Some(j) = jam.take() else { return };
    let keep = j.last_loud + (TAIL * RATE as f64) as u64;
    let end = j.start + keep.min(j.writer.frames()) as f64 / RATE as f64;
    if let Err(e) = j.writer.finish(Some(keep)) {
        eprintln!("audio journal: {e}");
        return;
    }
    let mut st = state.lock().unwrap();
    st.audio_live = None;
    st.audio_pending.push((j.file, j.start, end));
    let _ = jobs.send(Job { file: j.file, start: j.start, redo: false });
    let _ = dir;
}

/// Writes recordings down, one at a time, and files their sketches. The
/// model is loaded on the first recording; a recording that holds no
/// sketch (a door, a cough) isn't kept.
pub fn transcriber(dir: PathBuf, index: PathBuf, rx: Receiver<Job>, state: Arc<Mutex<State>>, model_path: Option<PathBuf>) {
    let mut model: Option<Option<BasicPitch>> = None;
    for job in rx {
        let model = model.get_or_insert_with(|| {
            let path = model_path.as_ref()?;
            match BasicPitch::load(path) {
                Ok(m) => Some(m),
                Err(e) => {
                    eprintln!("{e}");
                    None
                }
            }
        });
        let wav = wav_path(&dir, job.file);
        let read = wav::read(&wav);
        let (sketches, transcript) = match &read {
            Ok((_, samples)) => {
                let t = transcribe(samples, job.start, model.as_ref());
                (sketches_of(job.file, &t), Some(t))
            }
            Err(e) => {
                eprintln!("audio journal: {}: {e}", wav.display());
                (Vec::new(), None)
            }
        };
        if job.redo && transcript.is_some() {
            // Out with the old reading's sketches.
            let mut st = state.lock().unwrap();
            st.index.retain(|s| s.audio.is_none_or(|a| a.file != job.file));
            let kept: Vec<String> = st.index.iter().filter_map(|s| serde_json::to_string(s).ok()).collect();
            let tmp = index.with_extension("jsonl.tmp");
            if std::fs::write(&tmp, kept.iter().map(|l| format!("{l}\n")).collect::<String>()).is_ok() {
                let _ = std::fs::rename(&tmp, &index);
            }
        }
        match transcript.filter(|t| !sketches.is_empty() || job.redo && !t.events().is_empty()) {
            Some(t) => {
                if let Ok(text) = serde_json::to_string(&t) {
                    let _ = std::fs::write(transcript_path(&dir, job.file), text);
                }
                for s in &sketches {
                    if let Ok(line) = serde_json::to_string(s) {
                        let _ = append(&index, &line);
                    }
                }
                let mut st = state.lock().unwrap();
                st.transcripts.insert(job.file, Arc::new(t));
                st.index.extend(sketches);
                st.index.sort_by(|a, b| a.start.total_cmp(&b.start));
            }
            None => {
                // Nothing in it (a door, a cough). A recording that once
                // held a sketch is kept, whatever a new reading makes of it.
                if read.is_ok() && !job.redo {
                    let _ = std::fs::remove_file(&wav);
                }
            }
        }
        state.lock().unwrap().audio_pending.retain(|p| p.0 != job.file);
    }
}

/// Recordings to write down at startup: those the app stopped in the
/// middle of (a crash, a quit while playing), finished first; and those
/// written down by an older version of the transcription.
pub fn to_write_down(dir: &Path) -> Vec<Job> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(file) = path.extension().filter(|x| *x == "wav").and(path.file_stem()).and_then(|s| s.to_str()?.parse::<u64>().ok()) else {
            continue;
        };
        match std::fs::read_to_string(transcript_path(dir, file)) {
            Err(_) => {
                if wav::repair(&path).is_ok() {
                    out.push(Job { file, start: file as f64, redo: false });
                }
            }
            Ok(text) => {
                if let Ok(t) = serde_json::from_str::<Transcript>(&text)
                    && t.version < TRANSCRIPT_VERSION
                {
                    out.push(Job { file, start: t.start, redo: true });
                }
            }
        }
    }
    out.sort_by_key(|j| j.file);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::tests::{C, D, EM, G, strummed};

    #[test]
    fn a_sung_melody_becomes_a_sketch() {
        // A tune sung twice, in time at 100 BPM (a beat a note).
        let tune = [60u8, 64, 67, 72, 71, 67, 64, 62, 60, 64, 67, 72, 71, 67, 65, 64];
        let audio = crate::analysis::tests::sung(&tune, 0.6, RATE);
        let t = transcribe(&audio, 2_000_000.0, None);
        assert_eq!(t.reading, Reading::Line, "monophony {} chordal {}", t.monophony, t.chordal);
        let sketches = sketches_of(2_000_000, &t);
        assert_eq!(sketches.len(), 1, "{sketches:?}");
        assert_eq!(sketches[0].notes, tune.len(), "{:?}", t.line);
        assert!(sketches[0].tempo.is_some_and(|g| (g.bpm - 100.0).abs() < 2.0 || (g.bpm - 50.0).abs() < 1.0), "{:?}", sketches[0].tempo);
        // With the model there, one voice is still read as one line.
        if let Some(path) = crate::basic_pitch::model_path(None) {
            let bp = BasicPitch::load(&path).unwrap();
            let t = transcribe(&audio, 2_000_000.0, Some(&bp));
            assert_eq!(t.reading, Reading::Line);
            assert!(t.notes.is_some());
        }
    }

    #[test]
    fn strummed_guitar_becomes_a_sketch() {
        let (audio, _) = strummed(&[G, C, D, EM, G, C, D, EM], 100.0, RATE);
        let t = transcribe(&audio, 1_000_000.0, None);
        assert_eq!(t.reading, Reading::Strums);
        let sketches = sketches_of(1_000_000, &t);
        assert_eq!(sketches.len(), 1, "{sketches:?}");
        let s = &sketches[0];
        assert_eq!(s.audio, Some(AudioRef { file: 1_000_000, start: 1_000_000.0 }));
        let tempo = s.tempo.unwrap();
        assert!((tempo.bpm - 100.0).abs() < 2.0, "{tempo:?}");
        assert!(tempo.confidence > 0.5, "{tempo:?}");
        // G major or its relative minor (it ends each time round on Em).
        assert!(matches!(s.key.as_deref(), Some("G" | "Em")), "{s:?}");
        assert_eq!(jam::sketch_name(s), "G C D Em loop", "{s:?}");
    }
}
