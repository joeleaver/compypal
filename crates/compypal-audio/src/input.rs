//! MIDI input from a controller: live monitoring through the engine, and
//! capture of raw events for recording sessions.
//!
//! midir calls back on its own thread. That callback timestamps each
//! message with `Instant::now()`, forwards notes to the synth on the
//! monitor channel, and appends to the take if one is running.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use compypal_core::{RawEvent, RawMsg};

use crate::Engine;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("MIDI input unavailable: {0}")]
    Init(String),
    #[error("no MIDI input named {0:?}")]
    NoPort(String),
    #[error("couldn't connect to {0}: {1}")]
    Connect(String, String),
}

/// Input port names. Loopback ports ("Midi Through") are left out.
pub fn ports() -> Vec<String> {
    let Ok(input) = midir::MidiInput::new("compypal-probe") else { return Vec::new() };
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .filter(|n| !n.contains("Through"))
        .collect()
}

/// The port to use at startup: the first whose name contains
/// `COMPYPAL_MIDI_IN`, else a keyboard's main port rather than its
/// DAW-control port.
pub fn default_port() -> Option<String> {
    let ports = ports();
    if let Ok(want) = std::env::var("COMPYPAL_MIDI_IN") {
        return ports.into_iter().find(|n| n.contains(&want));
    }
    ports
        .iter()
        .find(|n| !n.contains("DAW") && !n.contains("MCU"))
        .or(ports.first())
        .cloned()
}

/// State the midir thread shares with the app.
#[derive(Default)]
pub struct Capture {
    take: Mutex<Option<Take>>,
    /// Channel live input is played on, so you hear the selected track.
    monitor_channel: AtomicU8,
}

struct Take {
    start: Instant,
    events: Vec<RawEvent>,
}

impl Capture {
    /// Events so far in the running take, for live display.
    pub fn snapshot(&self) -> Option<Vec<RawEvent>> {
        self.take.lock().unwrap().as_ref().map(|t| t.events.clone())
    }

    /// Seconds since the take started, if one is running.
    pub fn elapsed(&self) -> Option<f64> {
        self.take.lock().unwrap().as_ref().map(|t| t.start.elapsed().as_secs_f64())
    }
}

pub struct MidiIn {
    conn: RefCell<Option<(String, midir::MidiInputConnection<()>)>>,
    capture: Arc<Capture>,
    engine: Option<&'static Engine>,
}

impl MidiIn {
    pub fn new(engine: Option<&'static Engine>) -> Self {
        Self { conn: RefCell::new(None), capture: Arc::new(Capture::default()), engine }
    }

    pub fn capture(&self) -> Arc<Capture> {
        self.capture.clone()
    }

    pub fn connected(&self) -> Option<String> {
        self.conn.borrow().as_ref().map(|(n, _)| n.clone())
    }

    pub fn connect(&self, name: &str) -> Result<(), InputError> {
        self.disconnect();
        let input = midir::MidiInput::new("compypal").map_err(|e| InputError::Init(e.to_string()))?;
        let port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).is_ok_and(|n| n == name))
            .ok_or_else(|| InputError::NoPort(name.to_string()))?;
        let capture = self.capture.clone();
        let engine = self.engine;
        let conn = input
            .connect(
                &port,
                "compypal-in",
                move |_stamp, bytes, _| {
                    let now = Instant::now();
                    let Some((channel, msg)) = RawMsg::from_midi(bytes) else { return };
                    if let Some(take) = capture.take.lock().unwrap().as_mut() {
                        let t = now.saturating_duration_since(take.start).as_secs_f64();
                        take.events.push(RawEvent { t, channel, msg });
                    }
                    if let Some(e) = engine {
                        let ch = capture.monitor_channel.load(Ordering::Relaxed) & 0x0f;
                        let mut out = [bytes[0], bytes.get(1).copied().unwrap_or(0), bytes.get(2).copied().unwrap_or(0)];
                        out[0] = (out[0] & 0xf0) | ch;
                        e.midi(out);
                    }
                },
                (),
            )
            .map_err(|e| InputError::Connect(name.to_string(), e.to_string()))?;
        *self.conn.borrow_mut() = Some((name.to_string(), conn));
        Ok(())
    }

    pub fn disconnect(&self) {
        if let Some((_, c)) = self.conn.borrow_mut().take() {
            c.close();
        }
    }

    pub fn set_monitor_channel(&self, channel: u8) {
        self.capture.monitor_channel.store(channel & 0x0f, Ordering::Relaxed);
    }

    /// Starts a take, with time zero at `start`.
    pub fn start_take(&self, start: Instant) {
        *self.capture.take.lock().unwrap() = Some(Take { start, events: Vec::new() });
    }

    /// Ends the take and returns what was played, in order.
    pub fn finish_take(&self) -> Vec<RawEvent> {
        let mut events = self.capture.take.lock().unwrap().take().map(|t| t.events).unwrap_or_default();
        events.sort_by(|a, b| a.t.total_cmp(&b.t));
        events
    }
}
