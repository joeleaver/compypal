//! Puts a strummed guitar into a journal, as if heard through a mic: for
//! trying audio sketches with no guitar. `fake_strums <data dir>` (the
//! folder holding `journal/`, e.g. `$XDG_DATA_HOME/compypal`) writes four
//! bars each of G C D Em at 96 BPM, twice, a minute ago, and waits until
//! it has been written down. Run it with the app closed.

use compypal_audio::analysis::RATE;
use compypal_audio::journal::{Journal, now};

fn pluck(out: &mut [f32], at: f64, pitch: u8, amp: f32) {
    let f0 = 440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0);
    let start = (at * RATE as f64) as usize;
    for (i, s) in out.iter_mut().skip(start).enumerate() {
        let t = i as f32 / RATE as f32;
        let env = amp * (-t / 0.9).exp() * (t / 0.002).min(1.0);
        if t > 0.01 && env < 1e-5 {
            break;
        }
        let v: f32 = (1..=8)
            .map(|h| (std::f32::consts::TAU * f0 * h as f32 * t).sin() / h as f32 * (-t * h as f32 / 1.5).exp())
            .sum();
        *s += env * v;
    }
}

fn main() {
    let Some(data) = std::env::args().nth(1) else {
        eprintln!("usage: fake_strums <data dir>");
        return;
    };
    let chords: [&[u8]; 4] = [&[43, 47, 50, 55, 59, 67], &[48, 52, 55, 60, 64], &[50, 57, 62, 66], &[40, 47, 52, 55, 59, 64]];
    let beat = 60.0 / 96.0;
    let mut audio = vec![0.0f32; ((0.5 + 32.0 * beat + 12.0) * RATE as f64) as usize];
    // Down on every beat, up on the "and", the downbeats louder.
    for bar in 0..8 {
        for e in 0..8 {
            let t = 0.5 + (bar * 4) as f64 * beat + e as f64 * beat / 2.0;
            let amp = match e { 0 | 4 => 0.12, _ if e % 2 == 0 => 0.08, _ => 0.04 };
            let chord = chords[bar % 4];
            for (s, &p) in chord.iter().enumerate() {
                let string = if e % 2 == 0 { s } else { chord.len() - 1 - s };
                pluck(&mut audio, t + string as f64 * 0.007, p, amp);
            }
        }
    }
    let journal = Journal::open(std::path::Path::new(&data).join("journal")).expect("journal");
    journal.set_listening(true);
    let t0 = now() - 60.0;
    for (i, block) in audio.chunks(1024).enumerate() {
        journal.log_audio(t0 + (i * 1024) as f64 / RATE as f64, block.to_vec());
    }
    for _ in 0..600 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some((s, _)) = journal.sketches().into_iter().find(|(s, _)| s.audio.is_some() && s.notes > 0 && s.start >= t0) {
            println!("{}: {} notes, {:?}", compypal_core::jam::sketch_name(&s), s.notes, s.tempo.map(|t| t.bpm.round()));
            return;
        }
    }
    eprintln!("nothing came of it");
}
