//! The song as sound, offline: the same schedule, player and synth that
//! Play uses, run as fast as the machine allows, so an export sounds the
//! way the song does in the app. Then MP3 (LAME) or WAV.

use std::sync::Arc;
use std::sync::mpsc::channel;

use compypal_core::Project;

use crate::player::{Cmd, Looping, Player, Shared};
use crate::schedule::{self, ScheduleOptions};
use crate::synth::{self, BasicSynth, SoundFontSynth, Synth};

/// Rendered stereo audio.
pub struct Rendered {
    pub rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// What it was played through.
    pub synth: String,
}

impl Rendered {
    pub fn seconds(&self) -> f64 {
        self.left.len() as f64 / self.rate as f64
    }
}

/// CD rate: what MP3 players expect.
pub const EXPORT_RATE: u32 = 44_100;
/// Kept after the end, for the last notes to ring out.
const TAIL: f64 = 3.0;
const BLOCK: usize = 512;

/// The synth an export plays through: the GM SoundFont if there is one
/// (as the app finds it), else the built-in one.
pub fn export_synth(rate: u32) -> Box<dyn Synth> {
    match synth::find_soundfont().and_then(|p| SoundFontSynth::load(&p, rate).ok()) {
        Some(sf) => Box::new(sf),
        None => Box::new(BasicSynth::new(rate)),
    }
}

/// Plays the whole song (muted and soloed tracks as in the mixer) through
/// `synth` at `rate`, with a few seconds of tail, trimmed where the sound
/// has died away.
pub fn render_with(project: &Project, synth: Box<dyn Synth>, rate: u32) -> Rendered {
    let name = synth.name();
    let schedule = schedule::build(project, ScheduleOptions::default());
    let (tx, rx) = channel();
    let shared = Arc::new(Shared::default());
    let mut player = Player::new(synth, rate, rx, shared.clone());
    let _ = tx.send(Cmd::Play { schedule: Arc::new(schedule), from: 0.0, looping: Looping::Off });
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut after = 0usize;
    let tail = (TAIL * rate as f64) as usize;
    while after < tail {
        player.process(&mut l, &mut r);
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
        if shared.position().is_none() {
            after += BLOCK;
        }
    }
    // Trim the tail where it has gone quiet (-80 dB), keeping a breath.
    let quiet = 1e-4f32;
    let last = left.iter().zip(&right).rposition(|(a, b)| a.abs() > quiet || b.abs() > quiet).unwrap_or(0);
    let keep = (last + rate as usize / 4).min(left.len());
    left.truncate(keep);
    right.truncate(keep);
    Rendered { rate, left, right, synth: name }
}

pub fn render(project: &Project) -> Rendered {
    render_with(project, export_synth(EXPORT_RATE), EXPORT_RATE)
}

/// One gain for the whole render, so its peak sits at -1 dBFS: as loud
/// as other music, never clipped. Louder by 12 dB at most.
fn safe_gain(r: &Rendered) -> f32 {
    let peak = r.left.iter().chain(&r.right).fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 1e-6 { 1.0 } else { (0.891 / peak).min(3.98) }
}

/// MP3 at 192 kbps, tagged with the song's name.
pub fn mp3(r: &Rendered, title: &str) -> Result<Vec<u8>, String> {
    use mp3lame_encoder::{Bitrate, Builder, DualPcm, FlushNoGap, Id3Tag, Quality};
    let err = |e: &dyn std::fmt::Display| format!("MP3: {e}");
    let mut b = Builder::new().ok_or("MP3: LAME didn't start")?;
    b.set_num_channels(2).map_err(|e| err(&e))?;
    b.set_sample_rate(r.rate).map_err(|e| err(&e))?;
    b.set_brate(Bitrate::Kbps192).map_err(|e| err(&e))?;
    b.set_quality(Quality::Best).map_err(|e| err(&e))?;
    let _ = b.set_id3_tag(Id3Tag { title: title.as_bytes(), artist: b"", album: b"", album_art: &[], year: b"", comment: b"Made with compypal" });
    let mut enc = b.build().map_err(|e| err(&e))?;
    let g = safe_gain(r);
    let (l, rr): (Vec<f32>, Vec<f32>) = (r.left.iter().map(|s| s * g).collect(), r.right.iter().map(|s| s * g).collect());
    let mut out = Vec::new();
    out.reserve(mp3lame_encoder::max_required_buffer_size(l.len()));
    enc.encode_to_vec(DualPcm { left: &l, right: &rr }, &mut out).map_err(|e| err(&e))?;
    enc.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| err(&e))?;
    Ok(out)
}

/// 16-bit stereo WAV.
pub fn wav(r: &Rendered) -> Vec<u8> {
    let g = safe_gain(r);
    let frames = r.left.len() as u32;
    let data = frames * 4;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&r.rate.to_le_bytes());
    out.extend_from_slice(&(r.rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for (a, b) in r.left.iter().zip(&r.right) {
        for s in [a, b] {
            out.extend_from_slice(&(((s * g).clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_renders_and_encodes() {
        let p = compypal_core::demo::project();
        // The built-in synth: no SoundFont needed, and quick.
        let r = render_with(&p, Box::new(BasicSynth::new(EXPORT_RATE)), EXPORT_RATE);
        let song = p.tempo.tick_to_seconds(p.end_tick() as f64);
        assert!(r.seconds() > song && r.seconds() < song + TAIL + 0.5, "{} for a {song} s song", r.seconds());
        let loud = r.left.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(loud > 0.01, "it makes sound");
        let mp3 = mp3(&r, "Demo").unwrap();
        // An ID3 tag, then MPEG frames; 192 kbps is 24 kB a second.
        assert_eq!(&mp3[..3], b"ID3");
        let expect = r.seconds() * 24_000.0;
        assert!((mp3.len() as f64 - expect).abs() < expect * 0.2, "{} bytes for {} s", mp3.len(), r.seconds());
        let wav = wav(&r);
        assert_eq!(wav.len(), 44 + r.left.len() * 4);
    }
}
