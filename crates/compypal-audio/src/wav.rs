//! Mono 16-bit WAV, written as it is captured. The header's sizes are only
//! filled in when a file is finished, so a reader trusts the file's length
//! instead: a recording cut short by a crash still reads.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const HEADER: u64 = 44;

pub struct WavWriter {
    out: BufWriter<File>,
    frames: u64,
    rate: u32,
}

impl WavWriter {
    pub fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(&header(rate, 0))?;
        Ok(Self { out, frames: 0, rate })
    }

    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            self.out.write_all(&v.to_le_bytes())?;
        }
        self.frames += samples.len() as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Cuts the file to its first `frames` samples and finishes it.
    pub fn finish(mut self, frames: Option<u64>) -> std::io::Result<()> {
        self.out.flush()?;
        let frames = frames.unwrap_or(self.frames).min(self.frames);
        let mut f = self.out.into_inner().map_err(|e| e.into_error())?;
        f.set_len(HEADER + frames * 2)?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.rate, frames))?;
        f.sync_all()
    }
}

fn header(rate: u32, frames: u64) -> [u8; HEADER as usize] {
    let data = (frames * 2).min(u32::MAX as u64 - 36) as u32;
    let mut h = [0u8; HEADER as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// Reads a file this module wrote: (sample rate, samples).
pub fn read(path: &Path) -> std::io::Result<(u32, Vec<f32>)> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < HEADER as usize || &bytes[0..4] != b"RIFF" || &bytes[36..40] != b"data" {
        return Err(std::io::Error::other("not a compypal WAV"));
    }
    let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let samples = bytes[HEADER as usize..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    Ok((rate, samples))
}

/// Finishes a file left unfinished (the app stopped mid-recording).
pub fn repair(path: &Path) -> std::io::Result<()> {
    let (rate, samples) = read(path)?;
    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.write_all(&header(rate, samples.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_cuts_and_reads_back() {
        let path = std::env::temp_dir().join(format!("compypal-wav-{}.wav", std::process::id()));
        let mut w = WavWriter::create(&path, 22_050).unwrap();
        let tone: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        w.write(&tone).unwrap();
        w.write(&[0.0; 500]).unwrap();
        w.finish(Some(1000)).unwrap();
        let (rate, back) = read(&path).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(back.len(), 1000);
        assert!((back[100] - tone[100]).abs() < 1e-3);
        // Unfinished: the header says nothing, the length tells.
        let mut w = WavWriter::create(&path, 22_050).unwrap();
        w.write(&tone).unwrap();
        w.out.flush().unwrap();
        drop(w);
        assert_eq!(read(&path).unwrap().1.len(), 1000);
        repair(&path).unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
