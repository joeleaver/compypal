//! A virtual MIDI keyboard for testing recording without hardware. Opens an
//! ALSA port named "compypal-fake-keys", waits, then plays a slightly
//! sloppy C-Am-F-G arpeggio at 100 BPM.
//!
//! cargo run -p compypal-audio --example fake_keys -- [delay-seconds]

#[cfg(unix)]
fn main() {
    use midir::os::unix::VirtualOutput;
    use std::time::Duration;

    let delay: f64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(5.0);
    let out = midir::MidiOutput::new("compypal-fake").unwrap();
    let mut port = out.create_virtual("compypal-fake-keys").unwrap();
    println!("virtual port open; playing in {delay}s");
    std::thread::sleep(Duration::from_secs_f64(delay));
    let eighth = 0.3;
    let chords = [[60u8, 64, 67, 72], [57, 60, 64, 69], [53, 57, 60, 65], [55, 59, 62, 67]];
    let mut wobble = 0.0f64;
    for chord in chords {
        for step in [0usize, 1, 2, 3, 2, 1, 2, 1] {
            let key = chord[step];
            wobble = (wobble * 7.3 + 0.37).fract();
            let late = (wobble - 0.3) * 0.04;
            std::thread::sleep(Duration::from_secs_f64((eighth + late).max(0.0) * 0.5));
            port.send(&[0x90, key, 70 + (wobble * 30.0) as u8]).unwrap();
            std::thread::sleep(Duration::from_secs_f64(eighth * 0.4));
            port.send(&[0x80, key, 0]).unwrap();
            std::thread::sleep(Duration::from_secs_f64((eighth * 0.6 - late).max(0.0) * 0.5 + eighth * 0.1));
        }
    }
    println!("done");
    std::thread::sleep(Duration::from_secs(1));
}

#[cfg(not(unix))]
fn main() {
    eprintln!("virtual MIDI ports need ALSA or CoreMIDI");
}
