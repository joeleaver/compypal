//! Shows how a recording is heard: `transcribe_wav <file.wav>` (22.05 kHz
//! mono, as the journal keeps them) prints its pitch track in brief and
//! each reading's notes, so they can be compared with what was played.

use compypal_audio::analysis::{self, RATE};
use compypal_audio::audio_journal::transcribe;
use compypal_audio::basic_pitch::{BasicPitch, model_path};
use compypal_core::gm::pitch_name;
use compypal_core::{RawEvent, RawMsg};

fn notes(ev: &[RawEvent]) -> String {
    let mut out = Vec::new();
    for (i, e) in ev.iter().enumerate() {
        if let RawMsg::NoteOn { pitch, velocity } = e.msg {
            let end = ev[i..].iter().find(|o| matches!(o.msg, RawMsg::NoteOff { pitch: p } if p == pitch)).map_or(e.t, |o| o.t);
            out.push(format!("{:6.2}-{:5.2} {:4} v{velocity}", e.t, end, pitch_name(pitch)));
        }
    }
    out.join("\n")
}

fn main() {
    let path = std::env::args().nth(1).expect("transcribe_wav <file.wav>");
    let (rate, samples) = compypal_audio::wav::read(std::path::Path::new(&path)).expect("a WAV");
    assert_eq!(rate, RATE, "expects {RATE} Hz");
    let home = std::env::var("HOME").unwrap_or_default();
    let model = model_path(Some(&std::path::Path::new(&home).join(".local/share/compypal"))).and_then(|p| BasicPitch::load(&p).ok());
    let track = analysis::pitch_track(&samples, RATE);
    println!("pitch every 100 ms (time, pitch, clarity, dB):");
    for f in track.iter().step_by(10) {
        let p = f.pitch.map_or("   -  ".to_string(), |p| format!("{p:6.2}"));
        println!("  {:5.2} {p} {:.2} {:5.1}", f.t, f.clarity, f.level);
    }
    println!("monophony {:.2}", analysis::monophony(&track));
    let t = transcribe(&samples, 0.0, model.as_ref());
    println!("chosen: {:?}\n\nline:\n{}", t.reading, notes(t.line.as_deref().unwrap_or(&[])));
    println!("\nbasic pitch:\n{}", t.notes.as_deref().map(notes).unwrap_or_else(|| "(no model)".into()));
}
