//! The always-on journal: every MIDI message played while listening is
//! appended to a file per day, and the stream is cut into jams at long
//! silences and each jam into sketches, one idea each. Each finished
//! sketch's summary goes into an index, so finding "that thing I played
//! this afternoon" never means reading everything.
//!
//! Layout under the data directory:
//! `journal/<day>.jsonl` (one [`RawEvent`] per line, unix-time `t`), and
//! `journal/sketches.jsonl` (one [`Sketch`] per line). An older
//! `jams.jsonl` (a summary per whole jam) is split into sketches once.
//! Audio from a microphone is kept under `journal/audio/` and turns into
//! sketches once it's written down ([`crate::audio_journal`]).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compypal_core::jam::{self, GAP_SECONDS, Sketch};
use compypal_core::{AudioRef, RawEvent, RawMsg};

use crate::audio_journal::{self, Chunk, Transcript};

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// UTC day number, which names the day's file. Jams crossing midnight UTC
/// span two files; reads look in both.
fn day(t: f64) -> i64 {
    (t / 86_400.0).floor() as i64
}

fn day_file(dir: &Path, day: i64) -> PathBuf {
    dir.join(format!("{day}.jsonl"))
}

pub struct State {
    /// Events of the jam in progress, if any.
    current: Vec<RawEvent>,
    pub(crate) index: Vec<Sketch>,
    /// Sketches put out of sight, by id. Nothing is deleted.
    hidden: Vec<u64>,
    /// The recording in progress: file, start, when it was last loud.
    pub(crate) audio_live: Option<(u64, f64, f64)>,
    /// Recordings being written down: file, start, end.
    pub(crate) audio_pending: Vec<(u64, f64, f64)>,
    pub(crate) transcripts: HashMap<u64, Arc<Transcript>>,
}

pub struct Journal {
    dir: PathBuf,
    listening: AtomicBool,
    tx: Mutex<Sender<RawEvent>>,
    audio_tx: Mutex<Sender<Chunk>>,
    state: Arc<Mutex<State>>,
}

impl Journal {
    /// Opens (creating if needed) the journal in `dir` and starts its
    /// writer thread. Starts not listening.
    pub fn open(dir: PathBuf) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let index = load_index(&dir);
        let hidden = std::fs::read_to_string(dir.join("hidden.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let state = Arc::new(Mutex::new(State {
            current: Vec::new(),
            index,
            hidden,
            audio_live: None,
            audio_pending: Vec::new(),
            transcripts: HashMap::new(),
        }));
        let (tx, rx) = channel();
        let (dir2, state2) = (dir.clone(), state.clone());
        std::thread::Builder::new().name("compypal-journal".into()).spawn(move || writer(dir2, rx, state2))?;

        let audio = dir.join("audio");
        std::fs::create_dir_all(&audio)?;
        let (jobs_tx, jobs_rx) = channel();
        for job in audio_journal::to_write_down(&audio) {
            let _ = jobs_tx.send(job);
        }
        let (audio_tx, audio_rx) = channel();
        let (audio2, state2) = (audio.clone(), state.clone());
        std::thread::Builder::new()
            .name("compypal-journal-audio".into())
            .spawn(move || audio_journal::writer(audio2, audio_rx, state2, jobs_tx))?;
        let (audio2, state2, index_path) = (audio.clone(), state.clone(), dir.join("sketches.jsonl"));
        let model = crate::basic_pitch::model_path(dir.parent());
        std::thread::Builder::new()
            .name("compypal-transcribe".into())
            .spawn(move || audio_journal::transcriber(audio2, index_path, jobs_rx, state2, model))?;
        Ok(Arc::new(Self { dir, listening: AtomicBool::new(false), tx: Mutex::new(tx), audio_tx: Mutex::new(audio_tx), state }))
    }

    pub fn set_listening(&self, on: bool) {
        self.listening.store(on, Ordering::Relaxed);
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// Records one event, stamped now. Cheap enough for the MIDI callback.
    pub fn log(&self, channel: u8, msg: RawMsg) {
        if self.listening() {
            let _ = self.tx.lock().unwrap().send(RawEvent { t: now(), channel, msg });
        }
    }

    /// Records audio heard from the input, `t` the unix time of its first
    /// sample, at [`crate::analysis::RATE`].
    pub fn log_audio(&self, t: f64, samples: Vec<f32>) {
        if self.listening() {
            let _ = self.audio_tx.lock().unwrap().send(Chunk { t, samples });
        }
    }

    /// Sketches, newest first. Those of the jam still going come first: all
    /// but its last are over (a pause ended them); the last, if it has
    /// enough notes, is `live`. A recording still going, or still being
    /// written down, is listed as a sketch with no notes yet (`live` while
    /// it's going).
    pub fn sketches(&self) -> Vec<(Sketch, bool)> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<(Sketch, bool)> = Vec::new();
        let waiting = |file: u64, start: f64, end: f64| Sketch {
            id: file,
            start,
            end,
            notes: 0,
            lowest: 0,
            highest: 0,
            mean_velocity: 0,
            tempo: None,
            key: None,
            chords: Vec::new(),
            sustain_pedal: false,
            audio: Some(AudioRef { file, start }),
        };
        if let Some((file, start, loud)) = st.audio_live {
            out.push((waiting(file, start, loud), true));
        }
        for &(file, start, end) in st.audio_pending.iter().rev() {
            out.push((waiting(file, start, end), false));
        }
        let parts = jam::split_sketches(&st.current);
        for (i, (from, to)) in parts.iter().enumerate().rev() {
            let slice: Vec<RawEvent> = st.current.iter().filter(|e| e.t >= *from && e.t <= *to).copied().collect();
            if let Some(s) = jam::summarize(&slice) {
                out.push((s, i + 1 == parts.len()));
            }
        }
        out.extend(st.index.iter().rev().cloned().map(|j| (j, false)));
        // Listed newest first, whichever ear heard them.
        out.sort_by(|a, b| b.0.start.total_cmp(&a.0.start));
        out
    }

    /// A recording's transcript, if it has been written down.
    pub fn transcript(&self, file: u64) -> Option<Arc<Transcript>> {
        if let Some(t) = self.state.lock().unwrap().transcripts.get(&file) {
            return Some(t.clone());
        }
        let text = std::fs::read_to_string(audio_journal::transcript_path(&self.audio_dir(), file)).ok()?;
        let t: Arc<Transcript> = Arc::new(serde_json::from_str(&text).ok()?);
        self.state.lock().unwrap().transcripts.insert(file, t.clone());
        Some(t)
    }

    /// A sketch's events: the notes played, or the transcription of what
    /// was heard. `from`/`to` (unix seconds) narrow it.
    pub fn sketch_events(&self, s: &Sketch, from: f64, to: f64) -> Vec<RawEvent> {
        match s.audio {
            Some(a) => self
                .transcript(a.file)
                .map(|t| t.events().iter().filter(|e| e.t >= from && e.t <= to).copied().collect())
                .unwrap_or_default(),
            None => self.events(from, to),
        }
    }

    /// The sound of an audio sketch between `from` and `to` (unix
    /// seconds), at [`crate::analysis::RATE`].
    pub fn sketch_audio(&self, a: AudioRef, from: f64, to: f64) -> Option<Vec<f32>> {
        let (rate, samples) = crate::wav::read(&audio_journal::wav_path(&self.audio_dir(), a.file)).ok()?;
        let at = |t: f64| (((t - a.start).max(0.0) * rate as f64) as usize).min(samples.len());
        let (i, j) = (at(from), at(to));
        (j > i).then(|| samples[i..j].to_vec())
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.dir.join("audio")
    }

    pub fn sketch(&self, id: u64) -> Option<(Sketch, bool)> {
        self.sketches().into_iter().find(|(j, _)| j.id == id)
    }

    /// Every event logged between `from` and `to` (unix seconds).
    pub fn events(&self, from: f64, to: f64) -> Vec<RawEvent> {
        let mut out = Vec::new();
        for d in day(from)..=day(to) {
            let Ok(f) = File::open(day_file(&self.dir, d)) else { continue };
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<RawEvent>(&line)
                    && e.t >= from
                    && e.t <= to
                {
                    out.push(e);
                }
            }
        }
        // The jam in progress may not be on disk yet.
        let st = self.state.lock().unwrap();
        for e in st.current.iter().filter(|e| e.t >= from && e.t <= to) {
            if !out.iter().any(|o| o.t == e.t && o.msg == e.msg) {
                out.push(*e);
            }
        }
        out.sort_by(|a, b| a.t.total_cmp(&b.t));
        out
    }

    /// Puts a sketch out of sight, or back. The playing itself stays in the
    /// journal either way.
    pub fn set_hidden(&self, id: u64, hidden: bool) -> std::io::Result<()> {
        let mut st = self.state.lock().unwrap();
        st.hidden.retain(|h| *h != id);
        if hidden {
            st.hidden.push(id);
        }
        let text = serde_json::to_string(&st.hidden).map_err(std::io::Error::other)?;
        std::fs::write(self.dir.join("hidden.json"), text)
    }

    pub fn is_hidden(&self, id: u64) -> bool {
        self.state.lock().unwrap().hidden.contains(&id)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// The sketches in a finished jam.
fn sketches_of(events: &[RawEvent]) -> Vec<Sketch> {
    jam::split_sketches(events)
        .into_iter()
        .filter_map(|(from, to)| {
            let slice: Vec<RawEvent> = events.iter().filter(|e| e.t >= from && e.t <= to).copied().collect();
            jam::summarize(&slice)
        })
        .collect()
}

/// The sketch index; built once from an older index of whole jams.
fn load_index(dir: &Path) -> Vec<Sketch> {
    let path = dir.join("sketches.jsonl");
    if path.exists() {
        return read_lines(&path);
    }
    let jams = read_lines::<Sketch>(&dir.join("jams.jsonl"));
    let mut out = Vec::new();
    for j in jams {
        let mut events = Vec::new();
        for d in day(j.start)..=day(j.end) {
            let Ok(f) = File::open(day_file(dir, d)) else { continue };
            events.extend(
                BufReader::new(f)
                    .lines()
                    .map_while(Result::ok)
                    .filter_map(|l| serde_json::from_str::<RawEvent>(&l).ok())
                    .filter(|e| e.t >= j.start && e.t <= j.end),
            );
        }
        events.sort_by(|a, b| a.t.total_cmp(&b.t));
        let parts = sketches_of(&events);
        // A jam whose events are gone stays as it was.
        if parts.is_empty() { out.push(j) } else { out.extend(parts) }
    }
    for s in &out {
        if let Ok(line) = serde_json::to_string(s) {
            let _ = append(&path, &line);
        }
    }
    out
}

fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(f) = File::open(path) else { return Vec::new() };
    BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

pub(crate) fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// Writes events as they come, and closes the jam in progress once a long
/// enough silence has passed.
fn writer(dir: PathBuf, rx: Receiver<RawEvent>, state: Arc<Mutex<State>>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(e) => {
                if let Ok(line) = serde_json::to_string(&e) {
                    let _ = append(&day_file(&dir, day(e.t)), &line);
                }
                let mut st = state.lock().unwrap();
                st.current.push(e);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let mut st = state.lock().unwrap();
        let idle = st.current.last().is_some_and(|last| now() - last.t > GAP_SECONDS);
        if idle {
            let events = std::mem::take(&mut st.current);
            // Only notes make a sketch; a pedal or a knob alone doesn't.
            for summary in sketches_of(&events) {
                if let Ok(line) = serde_json::to_string(&summary) {
                    let _ = append(&dir.join("sketches.jsonl"), &line);
                }
                st.index.push(summary);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("compypal-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = Journal::open(dir.clone()).unwrap();
        j.log(0, RawMsg::NoteOn { pitch: 60, velocity: 90 });
        assert!(j.events(0.0, now() + 1.0).is_empty(), "not listening yet");
        j.set_listening(true);
        let t0 = now();
        for i in 0..8 {
            j.log(0, RawMsg::NoteOn { pitch: 60 + i, velocity: 90 });
            j.log(0, RawMsg::NoteOff { pitch: 60 + i });
        }
        std::thread::sleep(Duration::from_millis(300));
        let back = j.events(t0 - 1.0, now() + 1.0);
        assert_eq!(back.len(), 16);
        let sketches = j.sketches();
        assert_eq!(sketches.len(), 1);
        assert!(sketches[0].1, "still live: no gap has passed");
        assert_eq!(sketches[0].0.notes, 8);
        let id = sketches[0].0.id;
        j.set_hidden(id, true).unwrap();
        assert!(Journal::open(dir.clone()).unwrap().is_hidden(id), "remembered");
        j.set_hidden(id, false).unwrap();
        assert!(!j.is_hidden(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heard_audio_becomes_a_sketch() {
        use crate::analysis::RATE;
        use crate::analysis::tests::{C, D, EM, G, strummed};
        let dir = std::env::temp_dir().join(format!("compypal-journal-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = Journal::open(dir.clone()).unwrap();
        j.set_listening(true);
        // Strummed a minute ago, then quiet: as the input would hand it on.
        let (mut audio, _) = strummed(&[G, C, D, EM, G, C, D, EM], 100.0, RATE);
        audio.extend(vec![0.0; RATE as usize * 9]);
        let t0 = now() - 60.0;
        for (i, block) in audio.chunks(1024).enumerate() {
            j.log_audio(t0 + (i * 1024) as f64 / RATE as f64, block.to_vec());
        }
        let mut found = None;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            found = j.sketches().into_iter().find(|(s, _)| s.notes > 0);
            if found.is_some() {
                break;
            }
        }
        let (s, live) = found.expect("a sketch from the audio");
        assert!(!live);
        assert_eq!(jam::sketch_name(&s), "G C D Em loop", "{s:?}");
        let a = s.audio.unwrap();
        assert!((a.start - (t0 + 0.5 - 0.5)).abs() < 0.1, "{a:?} vs {t0}");
        assert_eq!(j.sketch_events(&s, s.start - 0.01, s.end + 0.01).len(), s.notes * 2);
        let sound = j.sketch_audio(a, s.start, s.start + 2.0).unwrap();
        assert_eq!(sound.len(), RATE as usize * 2);
        // Reopened, it's all still there.
        drop(j);
        let again = Journal::open(dir.clone()).unwrap();
        let (back, _) = again.sketches().into_iter().find(|(x, _)| x.id == s.id).unwrap();
        assert_eq!(again.sketch_events(&back, back.start - 0.01, back.end + 0.01).len(), s.notes * 2);
        drop(again);
        // Written down by an older version: done again at the next start,
        // its sketch replaced, not doubled.
        let json = audio_journal::transcript_path(&dir.join("audio"), a.file);
        let mut old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        old["version"] = 0.into();
        std::fs::write(&json, old.to_string()).unwrap();
        let redone = Journal::open(dir.clone()).unwrap();
        let mut version = 0;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            version = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&json).unwrap()).unwrap()["version"].as_u64().unwrap();
            if version == audio_journal::TRANSCRIPT_VERSION as u64 {
                break;
            }
        }
        assert_eq!(version, audio_journal::TRANSCRIPT_VERSION as u64);
        std::thread::sleep(Duration::from_millis(200));
        let ours = |j: &Journal| j.sketches().into_iter().filter(|(x, _)| x.audio.is_some_and(|b| b.file == a.file) && x.notes > 0).count();
        assert_eq!(ours(&redone), 1);
        drop(redone);
        assert_eq!(ours(&Journal::open(dir.clone()).unwrap()), 1, "and so on disk");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
