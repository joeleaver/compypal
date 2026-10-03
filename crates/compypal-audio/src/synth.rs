//! Instruments the player can drive. Everything speaks raw MIDI channel
//! messages, so the sequencer, live input and auditioning all look the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub trait Synth: Send {
    /// A channel voice message: status byte (with channel) and two data bytes.
    fn midi(&mut self, status: u8, data1: u8, data2: u8);
    /// Silences everything, immediately.
    fn panic(&mut self);
    /// Adds the next `left.len()` frames into the buffers, which arrive zeroed.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
    fn name(&self) -> String;
}

/// General MIDI playback from a SoundFont.
pub struct SoundFontSynth {
    synth: rustysynth::Synthesizer,
    name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SoundFontError {
    #[error("can't open {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} is not a usable SoundFont: {1}")]
    Parse(PathBuf, String),
}

impl SoundFontSynth {
    pub fn load(path: &Path, sample_rate: u32) -> Result<Self, SoundFontError> {
        let file = std::fs::File::open(path).map_err(|e| SoundFontError::Io(path.into(), e))?;
        let mut reader = std::io::BufReader::new(file);
        let sf = rustysynth::SoundFont::new(&mut reader)
            .map_err(|e| SoundFontError::Parse(path.into(), e.to_string()))?;
        let settings = rustysynth::SynthesizerSettings::new(sample_rate as i32);
        let synth = rustysynth::Synthesizer::new(&Arc::new(sf), &settings)
            .map_err(|e| SoundFontError::Parse(path.into(), e.to_string()))?;
        let name = path.file_name().map_or_else(|| "SoundFont".into(), |n| n.to_string_lossy().into_owned());
        Ok(Self { synth, name })
    }
}

impl Synth for SoundFontSynth {
    fn midi(&mut self, status: u8, data1: u8, data2: u8) {
        self.synth.process_midi_message(
            (status & 0x0f) as i32,
            (status & 0xf0) as i32,
            data1 as i32,
            data2 as i32,
        );
    }

    fn panic(&mut self) {
        self.synth.note_off_all(true);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.synth.render(left, right);
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

/// Where to look for a General MIDI SoundFont: `COMPYPAL_SOUNDFONT` first,
/// then the places Linux distributions and Homebrew install them.
pub fn find_soundfont() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COMPYPAL_SOUNDFONT") {
        return Some(PathBuf::from(p));
    }
    [
        "/usr/share/sounds/sf2/FluidR3_GM.sf2",
        "/usr/share/soundfonts/FluidR3_GM.sf2",
        "/usr/share/sounds/sf2/default-GM.sf2",
        "/usr/share/soundfonts/default.sf2",
        "/usr/share/sounds/sf2/TimGM6mb.sf2",
        "/opt/homebrew/share/soundfonts/default.sf2",
        "/usr/local/share/soundfonts/default.sf2",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// A small built-in synth so there is always something to hear: a few
/// waveforms chosen by GM program family, and synthesized drums on channel
/// 10. Not pretty; recognisable.
pub struct BasicSynth {
    rate: f32,
    programs: [u8; 16],
    volume: [f32; 16],
    voices: Vec<Voice>,
    noise: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Wave {
    /// Piano-ish: triangle with a quick decay to a low sustain.
    Keys,
    /// Round bass: sine plus a little second harmonic.
    Bass,
    /// Saw through a one-pole low-pass, slow attack.
    Pad,
    /// Square-ish lead.
    Lead,
    Kick,
    Snare,
    Hat,
    Click,
}

struct Voice {
    channel: u8,
    key: u8,
    wave: Wave,
    freq: f32,
    phase: f32,
    amp: f32,
    /// Seconds since note-on.
    age: f32,
    /// Seconds since note-off, if released.
    released: Option<f32>,
    level_at_release: f32,
    lp: f32,
}

const MAX_VOICES: usize = 48;

impl BasicSynth {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            rate: sample_rate as f32,
            programs: [0; 16],
            volume: [100.0 / 127.0; 16],
            voices: Vec::with_capacity(MAX_VOICES),
            noise: 0x1234_5678,
        }
    }

    fn wave_for(&self, channel: u8, key: u8) -> Wave {
        if channel == 9 {
            return match key {
                35 | 36 => Wave::Kick,
                37..=40 => Wave::Snare,
                42 | 44 | 46 | 49..=59 => Wave::Hat,
                76 | 77 => Wave::Click,
                _ => Wave::Snare,
            };
        }
        match self.programs[channel as usize] {
            0..=23 => Wave::Keys,
            32..=39 => Wave::Bass,
            40..=55 | 88..=103 => Wave::Pad,
            _ => Wave::Lead,
        }
    }

    fn envelope(v: &Voice) -> f32 {
        let attack = match v.wave {
            Wave::Pad => 0.08,
            _ => 0.003,
        };
        let held = match v.wave {
            Wave::Keys => {
                let a = (v.age / attack).min(1.0);
                a * (0.35 + 0.65 * (-v.age * 3.0).exp())
            }
            Wave::Kick | Wave::Click => (-v.age * 25.0).exp(),
            Wave::Snare => (-v.age * 18.0).exp(),
            Wave::Hat => (-v.age * 40.0).exp(),
            _ => (v.age / attack).min(1.0) * 0.8,
        };
        match v.released {
            None => held,
            Some(r) => {
                let release = if v.wave == Wave::Pad { 0.3 } else { 0.08 };
                v.level_at_release * (1.0 - r / release).max(0.0)
            }
        }
    }

    fn next_noise(&mut self) -> f32 {
        // xorshift32
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

impl Synth for BasicSynth {
    fn midi(&mut self, status: u8, data1: u8, data2: u8) {
        let ch = status & 0x0f;
        match status & 0xf0 {
            0x90 if data2 > 0 => {
                if self.voices.len() >= MAX_VOICES {
                    // Steal the oldest.
                    let oldest = (0..self.voices.len())
                        .max_by(|&a, &b| self.voices[a].age.total_cmp(&self.voices[b].age))
                        .unwrap();
                    self.voices.swap_remove(oldest);
                }
                let wave = self.wave_for(ch, data1);
                self.voices.push(Voice {
                    channel: ch,
                    key: data1,
                    wave,
                    freq: 440.0 * 2f32.powf((data1 as f32 - 69.0) / 12.0),
                    phase: 0.0,
                    amp: (data2 as f32 / 127.0).powf(1.5) * 0.25,
                    age: 0.0,
                    released: None,
                    level_at_release: 0.0,
                    lp: 0.0,
                });
            }
            0x80 | 0x90 => {
                for v in self.voices.iter_mut().filter(|v| v.channel == ch && v.key == data1 && v.released.is_none()) {
                    v.level_at_release = Self::envelope(v);
                    v.released = Some(0.0);
                }
            }
            0xc0 => self.programs[ch as usize] = data1 & 0x7f,
            0xb0 if data1 == 7 => self.volume[ch as usize] = data2 as f32 / 127.0,
            0xb0 if data1 == 120 || data1 == 123 => self.voices.retain(|v| v.channel != ch),
            _ => {}
        }
    }

    fn panic(&mut self) {
        self.voices.clear();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let dt = 1.0 / self.rate;
        for i in 0..left.len() {
            let mut out = 0.0;
            for vi in 0..self.voices.len() {
                let noise = if matches!(self.voices[vi].wave, Wave::Snare | Wave::Hat) { self.next_noise() } else { 0.0 };
                let v = &mut self.voices[vi];
                let env = Self::envelope(v);
                let p = v.phase;
                let s = match v.wave {
                    Wave::Keys => 1.0 - 4.0 * (p - 0.5).abs(),
                    Wave::Bass => (p * std::f32::consts::TAU).sin() + 0.3 * (p * 2.0 * std::f32::consts::TAU).sin(),
                    Wave::Pad => {
                        let saw = 2.0 * p - 1.0;
                        v.lp += (saw - v.lp) * 0.08;
                        v.lp
                    }
                    Wave::Lead => {
                        let sq = if p < 0.5 { 1.0 } else { -1.0 };
                        v.lp += (sq - v.lp) * 0.2;
                        v.lp * 0.6
                    }
                    Wave::Kick => (p * std::f32::consts::TAU).sin() * 1.6,
                    Wave::Snare => noise * 0.8 + (p * std::f32::consts::TAU).sin() * 0.4,
                    Wave::Hat => {
                        v.lp += (noise - v.lp) * 0.5;
                        (noise - v.lp) * 0.6
                    }
                    Wave::Click => (p * std::f32::consts::TAU).sin(),
                };
                let freq = match v.wave {
                    // Pitch drops fast: the thump of a kick.
                    Wave::Kick => 50.0 + 120.0 * (-v.age * 30.0).exp(),
                    Wave::Snare => 180.0,
                    Wave::Click => if v.key == 76 { 1600.0 } else { 1100.0 },
                    _ => v.freq,
                };
                v.phase = (v.phase + freq * dt).fract();
                out += s * env * v.amp * self.volume[v.channel as usize];
                v.age += dt;
                if let Some(r) = v.released.as_mut() {
                    *r += dt;
                }
            }
            left[i] += out;
            right[i] += out;
            // Drop finished voices now and then, not every sample.
            if i % 64 == 0 {
                self.voices.retain(|v| match v.released {
                    Some(r) => r < 0.31,
                    None => !matches!(v.wave, Wave::Kick | Wave::Snare | Wave::Hat | Wave::Click) || v.age < 0.6,
                });
            }
        }
    }

    fn name(&self) -> String {
        "Built-in synth".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_synth_makes_sound_and_stops() {
        let mut s = BasicSynth::new(48_000);
        s.midi(0x90, 60, 100);
        let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
        s.render(&mut l, &mut r);
        assert!(l.iter().any(|x| x.abs() > 0.05));
        s.midi(0x80, 60, 0);
        let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
        s.render(&mut l, &mut r);
        assert!(l[24_000..].iter().all(|x| *x == 0.0), "released note dies out");
        assert!(s.voices.is_empty());
    }
}
