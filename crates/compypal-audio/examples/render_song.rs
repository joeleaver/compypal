//! Renders a saved project to sound without the UI:
//! `render_song <project.json> <out.mp3|out.wav>`.

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: render_song <project.json> <out.mp3|out.wav>");
        return;
    };
    let text = std::fs::read_to_string(&input).expect("read the project");
    let p = compypal_core::Project::from_json(&text).expect("a compypal project");
    let t = std::time::Instant::now();
    let r = compypal_audio::render::render(&p);
    let bytes = if output.ends_with(".wav") { compypal_audio::render::wav(&r) } else { compypal_audio::render::mp3(&r, &p.name).expect("encode") };
    std::fs::write(&output, &bytes).expect("write");
    println!("{output}: {:.1} s through {}, {} kB, in {:.1} s", r.seconds(), r.synth, bytes.len() / 1024, t.elapsed().as_secs_f64());
}
