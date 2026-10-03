//! Plays the demo project through the default output: `cargo run -p compypal-audio --example play_demo`.

use std::time::{Duration, Instant};

use compypal_audio::{Engine, ScheduleOptions, schedule};

fn main() {
    let engine = Engine::start().expect("audio output");
    println!("rate {} Hz, {}", engine.sample_rate(), engine.status());
    let t = Instant::now();
    while engine.status().contains("loading") && t.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("{} after {:?}", engine.status(), t.elapsed());
    let p = compypal_core::demo::project();
    engine.play(schedule::build(&p, ScheduleOptions { metronome: true }), 0.0, false);
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(500));
        println!("position {:?}", engine.position());
    }
    engine.stop();
    std::thread::sleep(Duration::from_millis(300));
}
