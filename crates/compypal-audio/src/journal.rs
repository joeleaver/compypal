//! The always-on journal: every MIDI message played while listening is
//! appended to a file per day, and the stream is cut into jams at
//! silences. Each finished jam's summary goes into an index, so finding
//! "that thing I played this afternoon" never means reading everything.
//!
//! Layout under the data directory:
//! `journal/YYYYMMDD.jsonl` (one [`RawEvent`] per line, unix-time `t`), and
//! `journal/jams.jsonl` (one [`JamSummary`] per line).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compypal_core::jam::{self, GAP_SECONDS, JamSummary};
use compypal_core::{RawEvent, RawMsg};

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

struct State {
    /// Events of the jam in progress, if any.
    current: Vec<RawEvent>,
    index: Vec<JamSummary>,
}

pub struct Journal {
    dir: PathBuf,
    listening: AtomicBool,
    tx: Mutex<Sender<RawEvent>>,
    state: Arc<Mutex<State>>,
}

impl Journal {
    /// Opens (creating if needed) the journal in `dir` and starts its
    /// writer thread. Starts not listening.
    pub fn open(dir: PathBuf) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let index = read_lines::<JamSummary>(&dir.join("jams.jsonl"));
        let state = Arc::new(Mutex::new(State { current: Vec::new(), index }));
        let (tx, rx) = channel();
        let (dir2, state2) = (dir.clone(), state.clone());
        std::thread::Builder::new().name("compypal-journal".into()).spawn(move || writer(dir2, rx, state2))?;
        Ok(Arc::new(Self { dir, listening: AtomicBool::new(false), tx: Mutex::new(tx), state }))
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

    /// Finished jams, newest first, then the jam in progress (if it has
    /// enough notes) at the front, with `live` set.
    pub fn jams(&self) -> Vec<(JamSummary, bool)> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<(JamSummary, bool)> = st.index.iter().rev().cloned().map(|j| (j, false)).collect();
        if let Some(live) = jam::summarize(&st.current) {
            out.insert(0, (live, true));
        }
        out
    }

    pub fn jam(&self, id: u64) -> Option<(JamSummary, bool)> {
        self.jams().into_iter().find(|(j, _)| j.id == id)
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

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(f) = File::open(path) else { return Vec::new() };
    BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

fn append(path: &Path, line: &str) -> std::io::Result<()> {
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
            // Only notes make a jam; a pedal or a knob alone doesn't.
            if let Some(summary) = jam::summarize(&events) {
                if let Ok(line) = serde_json::to_string(&summary) {
                    let _ = append(&dir.join("jams.jsonl"), &line);
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
        let jams = j.jams();
        assert_eq!(jams.len(), 1);
        assert!(jams[0].1, "still live: no gap has passed");
        assert_eq!(jams[0].0.notes, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
