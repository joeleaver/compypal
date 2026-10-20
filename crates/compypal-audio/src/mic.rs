//! Audio input: a microphone or an interface, for instruments without MIDI
//! (an acoustic guitar). The stream runs while a device is chosen, so the
//! level meter moves; what it hears goes to the journal, which keeps it
//! while listening, at [`RATE`] mono.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::analysis::{RATE, Resampler};
use crate::journal::Journal;

/// An input device as listed: `id` is cpal's (persistable) device id.
#[derive(Clone, Debug, PartialEq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
}

/// Devices to offer. On ALSA every card shows up under a dozen plugin
/// names; one entry per card (its converting `plughw` device) plus the
/// system default (which, under PipeWire or Pulse, is whatever the sound
/// settings pick) is the useful list.
pub fn devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let Ok(all) = host.input_devices() else { return Vec::new() };
    let alsa = host.id().name().eq_ignore_ascii_case("alsa");
    let mut out: Vec<InputDevice> = Vec::new();
    for d in all {
        let Ok(id) = d.id().map(|i| i.to_string()) else { continue };
        let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
        if alsa {
            let raw = id.split_once(':').map_or(id.as_str(), |(_, r)| r);
            if raw == "default" {
                out.insert(0, InputDevice { id, name: "System input".into() });
                continue;
            }
            // `plughw:CARD=Name,DEV=0`; numbered duplicates (CARD=1) and
            // the other plugins are left out.
            let Some(card) = raw.strip_prefix("plughw:CARD=") else { continue };
            if card.chars().next().is_some_and(|c| c.is_ascii_digit()) || !card.ends_with(",DEV=0") {
                continue;
            }
            let name = name.split(',').next().unwrap_or(&name).trim().to_string();
            out.push(InputDevice { id, name });
        } else {
            out.push(InputDevice { id, name });
        }
    }
    out
}

/// The device to open at startup: `COMPYPAL_AUDIO_IN` (matched against
/// names and ids, or "default"), else the one remembered.
pub fn default_device(remembered: Option<&str>) -> Option<String> {
    let list = devices();
    if let Ok(want) = std::env::var("COMPYPAL_AUDIO_IN") {
        let want = want.to_lowercase();
        if want == "default" {
            return list.into_iter().find(|d| d.name == "System input").map(|d| d.id);
        }
        return list.into_iter().find(|d| d.id.to_lowercase().contains(&want) || d.name.to_lowercase().contains(&want)).map(|d| d.id);
    }
    let r = remembered?;
    list.into_iter().find(|d| d.id == r).map(|d| d.id)
}

pub struct AudioIn {
    journal: Option<Arc<Journal>>,
    /// The stream's thread, told to stop through this.
    running: Mutex<Option<(String, Sender<()>)>>,
    /// Peak level of the latest block, as f32 bits (linear, 0..1).
    level: Arc<AtomicU32>,
}

impl AudioIn {
    pub fn new(journal: Option<Arc<Journal>>) -> Self {
        Self { journal, running: Mutex::new(None), level: Arc::new(AtomicU32::new(0)) }
    }

    pub fn connected(&self) -> Option<String> {
        self.running.lock().unwrap().as_ref().map(|(id, _)| id.clone())
    }

    /// Peak level of the latest block, 0..1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn disconnect(&self) {
        if let Some((_, stop)) = self.running.lock().unwrap().take() {
            let _ = stop.send(());
        }
        self.level.store(0, Ordering::Relaxed);
    }

    /// Opens device `id` (from [`devices`]) and starts hearing it.
    pub fn connect(&self, id: &str) -> Result<(), String> {
        self.disconnect();
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = channel::<()>();
        let (id2, level, journal) = (id.to_string(), self.level.clone(), self.journal.clone());
        // cpal streams aren't Send everywhere: one thread opens the stream
        // and holds it until told to stop.
        std::thread::Builder::new()
            .name("compypal-mic".into())
            .spawn(move || {
                let opened = open(&id2, level, journal);
                match opened {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|_| "audio input thread died".to_string())??;
        *self.running.lock().unwrap() = Some((id.to_string(), stop_tx));
        Ok(())
    }
}

fn open(id: &str, level: Arc<AtomicU32>, journal: Option<Arc<Journal>>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let parsed: cpal::DeviceId = id.parse().map_err(|e: cpal::Error| e.to_string())?;
    let device = host.device_by_id(&parsed).ok_or_else(|| format!("no audio input {id}"))?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, level, journal),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, level, journal),
        cpal::SampleFormat::I32 => build::<i32>(&device, &config, level, journal),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, level, journal),
        other => return Err(format!("unsupported input format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, level: Arc<AtomicU32>, journal: Option<Arc<Journal>>) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let rate = config.sample_rate;
    let mut resampler = Resampler::new(rate);
    let mut mono: Vec<f32> = Vec::new();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let now = crate::journal::now();
                // Mixed to mono: a guitar on one input of two is heard at
                // half level, which the gate doesn't mind.
                mono.clear();
                let mut peak = 0.0f32;
                for frame in data.chunks(channels) {
                    let s = frame.iter().map(|x| <f32 as cpal::FromSample<T>>::from_sample_(*x)).sum::<f32>() / channels as f32;
                    peak = peak.max(s.abs());
                    mono.push(s);
                }
                level.store(peak.to_bits(), Ordering::Relaxed);
                if let Some(j) = journal.as_ref().filter(|j| j.listening()) {
                    let mut out = Vec::with_capacity(mono.len() * RATE as usize / rate as usize + 2);
                    resampler.push(&mono, &mut out);
                    let first = now - mono.len() as f64 / rate as f64;
                    j.log_audio(first, out);
                }
            },
            |e| eprintln!("audio input error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}
