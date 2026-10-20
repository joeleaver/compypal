//! Checks an audio input without the UI: lists the inputs, opens one
//! (`mic_level [name or id]`, default the system input) and prints its
//! level for five seconds.

use compypal_audio::mic::{AudioIn, devices};

fn main() {
    let list = devices();
    for d in &list {
        println!("{}  ({})", d.name, d.id);
    }
    let want = std::env::args().nth(1).unwrap_or_else(|| "System input".into()).to_lowercase();
    let Some(d) = list.iter().find(|d| d.name.to_lowercase().contains(&want) || d.id.to_lowercase().contains(&want)) else {
        eprintln!("no input matching {want:?}");
        return;
    };
    // `mic_level <name> engine`: with the synth's output running too, as
    // in the app.
    let _engine = (std::env::args().nth(2).as_deref() == Some("engine")).then(|| compypal_audio::Engine::start().unwrap());
    let input: &'static AudioIn = Box::leak(Box::new(AudioIn::new(None)));
    input.keep_open();
    // `mic_level <name> again`: open, close and open again, as the app
    // does when an input is picked twice.
    if std::env::args().nth(2).as_deref() == Some("again") {
        input.connect(&d.id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        input.disconnect();
    }
    if let Err(e) = input.connect(&d.id) {
        eprintln!("{}: {e}", d.name);
        return;
    }
    println!("hearing {}", d.name);
    let seconds: usize = std::env::var("MIC_LEVEL_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    for _ in 0..seconds * 5 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let db = 20.0 * (input.level() + 1e-9).log10();
        println!("{db:6.1} dBFS {}", "#".repeat(((db + 80.0).max(0.0) / 2.0) as usize));
    }
}
