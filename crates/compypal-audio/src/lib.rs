//! Audio for previewing: a SoundFont (or built-in) synth, a transport that
//! plays a [`Schedule`], and auditioning of single phrases.
//!
//! [`Engine::start`] opens the default output device and returns at once.
//! It plays through [`synth::BasicSynth`] until a General MIDI SoundFont has
//! loaded in the background, then switches over.

pub mod input;
pub mod player;
pub mod schedule;
pub mod synth;

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use player::{Cmd, Player, Shared};
pub use schedule::{Schedule, ScheduleOptions, Timed};
use synth::{BasicSynth, SoundFontSynth, Synth};

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no audio output device")]
    NoDevice,
    #[error("audio device: {0}")]
    Device(String),
}

pub struct Engine {
    tx: Mutex<Sender<Cmd>>,
    shared: Arc<Shared>,
    sample_rate: u32,
    /// What the engine is playing through, and how loading went.
    status: Arc<Mutex<String>>,
}

impl Engine {
    pub fn start() -> Result<Self, AudioError> {
        let (tx, rx) = channel::<Cmd>();
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = channel::<Result<u32, AudioError>>();
        let shared_audio = shared.clone();

        // cpal streams are not Send everywhere, so one thread opens the
        // stream and keeps it alive for the life of the process.
        std::thread::Builder::new()
            .name("compypal-audio".into())
            .spawn(move || {
                let opened = (|| {
                    let host = cpal::default_host();
                    let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
                    let supported = device.default_output_config().map_err(|e| AudioError::Device(e.to_string()))?;
                    let format = supported.sample_format();
                    let config: cpal::StreamConfig = supported.into();
                    let rate = config.sample_rate;
                    let channels = config.channels as usize;
                    let player = Player::new(Box::new(BasicSynth::new(rate)), rate, rx, shared_audio);
                    let stream = match format {
                        cpal::SampleFormat::F32 => build::<f32>(&device, &config, channels, player),
                        cpal::SampleFormat::I16 => build::<i16>(&device, &config, channels, player),
                        cpal::SampleFormat::U16 => build::<u16>(&device, &config, channels, player),
                        other => return Err(AudioError::Device(format!("unsupported sample format {other}"))),
                    }?;
                    stream.play().map_err(|e| AudioError::Device(e.to_string()))?;
                    Ok((stream, rate))
                })();
                match opened {
                    Ok((stream, rate)) => {
                        let _ = ready_tx.send(Ok(rate));
                        // Hold the stream open until the process exits.
                        let _keep = stream;
                        loop {
                            std::thread::park();
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| AudioError::Device(e.to_string()))?;

        let sample_rate = ready_rx.recv().map_err(|_| AudioError::NoDevice)??;
        let status = Arc::new(Mutex::new("Built-in synth (loading SoundFont…)".to_string()));

        let sf_tx = tx.clone();
        let sf_status = status.clone();
        std::thread::spawn(move || {
            let msg = match synth::find_soundfont() {
                None => "Built-in synth: no SoundFont found (set COMPYPAL_SOUNDFONT)".to_string(),
                Some(path) => match SoundFontSynth::load(&path, sample_rate) {
                    Ok(sf) => {
                        let name = sf.name();
                        let _ = sf_tx.send(Cmd::SetSynth(Box::new(sf)));
                        name
                    }
                    Err(e) => format!("Built-in synth: {e}"),
                },
            };
            *sf_status.lock().unwrap() = msg;
        });

        Ok(Self { tx: Mutex::new(tx), shared, sample_rate, status })
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.lock().unwrap().send(cmd);
    }

    pub fn play(&self, schedule: Schedule, from: f64, looping: bool) {
        self.send(Cmd::Play { schedule: Arc::new(schedule), from, looping });
    }

    /// Replaces what's playing without stopping, keeping the position.
    pub fn update(&self, schedule: Schedule) {
        self.send(Cmd::Update(Arc::new(schedule)));
    }

    pub fn stop(&self) {
        self.send(Cmd::Stop);
    }

    pub fn set_looping(&self, on: bool) {
        self.send(Cmd::SetLooping(on));
    }

    pub fn audition(&self, schedule: Schedule) {
        self.send(Cmd::Audition(Arc::new(schedule)));
    }

    pub fn midi(&self, msg: [u8; 3]) {
        self.send(Cmd::Midi(msg));
    }

    pub fn panic(&self) {
        self.send(Cmd::Panic);
    }

    /// Transport position in seconds, or `None` when stopped.
    pub fn position(&self) -> Option<f64> {
        self.shared.position()
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut player: Player,
) -> Result<cpal::Stream, AudioError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |out: &mut [T], _| {
                let frames = out.len() / channels;
                if left.len() < frames {
                    left.resize(frames, 0.0);
                    right.resize(frames, 0.0);
                }
                player.process(&mut left[..frames], &mut right[..frames]);
                for (i, frame) in out.chunks_mut(channels).enumerate() {
                    let (l, r) = (left[i].clamp(-1.0, 1.0), right[i].clamp(-1.0, 1.0));
                    for (c, s) in frame.iter_mut().enumerate() {
                        *s = T::from_sample(if c % 2 == 0 { l } else { r });
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))
}
