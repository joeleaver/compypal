//! compypal: a MIDI composer and arranger built to work alongside an agent.

mod autosave;
mod store;

use std::rc::Rc;

use compypal_core::cleanup::grid_ticks;
use compypal_core::{Id, Project, gm};
use rinch::prelude::*;
use store::{ROW, Slot, Store};

/// Width of the key labels down the left of the roll.
const KEYS: f64 = 64.0;

const CSS: &str = include_str!("style.css");

#[derive(Clone, Debug, PartialEq)]
struct TrackRow {
    id: Id,
    name: String,
    instrument: String,
    notes: usize,
    channel: u8,
}

#[derive(Clone, Debug, PartialEq)]
struct SessionRow {
    id: Id,
    name: String,
    detail: String,
}

fn track_rows(p: &Project) -> Vec<TrackRow> {
    p.tracks
        .iter()
        .map(|t| TrackRow {
            id: t.id,
            name: t.name.clone(),
            instrument: if t.is_drums() { "Drums".into() } else { gm::program_name(t.program).into() },
            notes: t.clips.iter().map(|c| c.notes.len()).sum(),
            channel: t.channel + 1,
        })
        .collect()
}

fn session_rows(p: &Project) -> Vec<SessionRow> {
    p.sessions
        .iter()
        .map(|s| {
            let tempo = s.click_bpm.map_or("free time".to_string(), |b| format!("{b:.0} BPM"));
            SessionRow {
                id: s.id,
                name: s.name.clone(),
                detail: format!("{} notes · {:.1}s · {tempo}", s.notes().len(), s.duration()),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
struct PortRow {
    index: usize,
    name: String,
}

fn port_rows() -> Vec<PortRow> {
    compypal_audio::input::ports().into_iter().enumerate().map(|(index, name)| PortRow { index, name }).collect()
}

/// "KeyLab mkII 49:KeyLab mkII 49 MIDI 32:0" reads better as
/// "KeyLab mkII 49 MIDI": ALSA names are "client:port id:id".
fn short_port_name(name: &str) -> String {
    let port = name.split_once(':').map_or(name, |(_, p)| p);
    match port.rsplit_once(' ') {
        Some((p, ids)) if ids.contains(':') => p.to_string(),
        _ => port.to_string(),
    }
}

#[component]
fn Transport() -> NodeHandle {
    let store = use_store::<Store>();
    rsx! {
        div { class: "transport",
            span { class: "project-name", {|| store.project.with(|p| p.name.clone())} }
            span { class: "readout",
                {|| store.project.with(|p| {
                    let m = p.meter_at(0);
                    format!("{:.0} BPM  ·  {}/{}  ·  {}", p.tempo.bpm_at(0), m.numerator, m.denominator, p.key.name())
                })}
            }
            div { class: "divider" }
            Button { size: "xs",
                variant: {|| if store.is_playing() { "filled" } else { "light" }},
                disabled: store.engine.is_none(),
                onclick: move || store.toggle_play(),
                {|| if store.is_playing() { "■ Stop" } else { "▶ Play" }}
            }
            Button { size: "xs",
                color: "red",
                variant: {|| if store.is_recording() { "filled" } else { "light" }},
                disabled: store.engine.is_none() || store.midi.is_none(),
                onclick: move || if store.is_recording() { store.stop_recording() } else { store.record() },
                {|| if store.is_recording() { "■ Stop rec" } else { "● Rec" }}
            }
            Button { size: "xs",
                variant: {|| if store.looping.get() { "filled" } else { "default" }},
                onclick: move || store.toggle_looping(),
                "Loop"
            }
            Button { size: "xs",
                variant: {|| if store.metronome.get() { "filled" } else { "default" }},
                onclick: move || store.metronome.update(|m| *m = !*m),
                "Click"
            }
            span { class: "readout position",
                {|| {
                    let tick = match store.song_seconds() {
                        Some(secs) => store.project.with(|p| p.tempo.seconds_to_tick(secs).max(0.0) as u64),
                        None => store.cursor.get(),
                    };
                    store.project.with(|p| compypal_core::cleanup::bar_beat_tick(tick, &p.meter_at(0)))
                }}
            }
            div { class: "spacer" }
            Button { size: "xs", variant: "default",
                disabled: {|| store.history.with(|h| h.undo_label().is_none())},
                onclick: move || store.undo(),
                "Undo"
            }
            Button { size: "xs", variant: "default",
                disabled: {|| store.history.with(|h| h.redo_label().is_none())},
                onclick: move || store.redo(),
                "Redo"
            }
            div { class: "divider" }
            Button { size: "xs", variant: "light",
                onclick: move || store.quantize_selected(grid_ticks("1/16").unwrap()),
                "Quantize 1/16"
            }
            Button { size: "xs",
                variant: {|| if store.roman.get() { "filled" } else { "default" }},
                onclick: move || store.roman.update(|v| *v = !*v),
                "Roman"
            }
            Button { size: "xs",
                variant: {|| if store.show_raw.get() { "filled" } else { "default" }},
                onclick: move || store.show_raw.update(|v| *v = !*v),
                "Show raw take"
            }
            Button { size: "xs", variant: "default", onclick: move || store.zoom.update(|z| *z = (*z / 1.5).max(8.0)), "−" }
            Button { size: "xs", variant: "default", onclick: move || store.zoom.update(|z| *z = (*z * 1.5).min(400.0)), "+" }
            div { class: "divider" }
            Button { size: "xs", variant: "default", onclick: move || store.new_project(), "New" }
            Button { size: "xs", variant: "light", onclick: move || store.export_midi(), "Export MIDI" }
            Button { size: "xs", variant: "light", onclick: move || store.export_abc(), "Export ABC" }
        }
    }
}

#[component]
fn Sidebar() -> NodeHandle {
    let store = use_store::<Store>();
    rsx! {
        div { class: "sidebar",
            div { class: "sidebar-heading heading-row",
                span { "Tracks" }
                Button { size: "xs", variant: "subtle", onclick: move || store.add_track(), "+ Track" }
            }
            for t in store.project.with(track_rows) {
                div {
                    key: t.id.0,
                    class: {
                        let id = t.id;
                        move || if store.selected_track.get() == Some(id) { "track-row selected" } else { "track-row" }
                    },
                    onclick: {
                        let id = t.id;
                        move || store.select_track(id)
                    },
                    div { class: "track-name", {t.name.clone()} }
                    div { class: "track-detail",
                        {format!("{} · ch {} · {} notes", t.instrument, t.channel, t.notes)}
                    }
                }
            }
            div { class: "sidebar-heading", "Sessions" }
            for s in store.project.with(session_rows) {
                div { key: s.id.0, class: "session-row",
                    div { class: "session-head",
                        div { class: "track-name", {s.name.clone()} }
                        Button { size: "xs", variant: "subtle",
                            onclick: {
                                let id = s.id;
                                move || store.tidy_session(id)
                            },
                            "Tidy"
                        }
                    }
                    div { class: "track-detail", {s.detail.clone()} }
                }
            }
            if store.project.with(|p| p.sessions.is_empty()) {
                div { class: "empty", "No recordings yet" }
            }
            div { class: "sidebar-heading", "MIDI input" }
            for port in port_rows() {
                div {
                    key: port.index,
                    class: {
                        let i = port.index;
                        move || if store.midi_port_index() == Some(i) { "port-row selected" } else { "port-row" }
                    },
                    onclick: {
                        let i = port.index;
                        move || store.toggle_port(i)
                    },
                    {short_port_name(&port.name)}
                }
            }
        }
    }
}

#[component]
fn PianoRoll() -> NodeHandle {
    let store = use_store::<Store>();
    let roll = Memo::new(move || store.roll());
    rsx! {
        div { class: "roll-scroll",
            div {
                class: "roll",
                style: {|| { let r = roll.get(); format!("width: {}px; height: {}px;", r.width + 64.0, r.height + 20.0) }},
                FigureLane {}
                div {
                    class: "ruler",
                    onclick: move || {
                        let c = get_click_context();
                        let x = (c.mouse_x - c.element_x) as f64 - KEYS;
                        let px = store.zoom.get() / compypal_core::PPQ as f64;
                        // Snap to the beat: you'd count in from a beat, not a tick.
                        let beat = compypal_core::PPQ as f64;
                        let tick = ((x / px) / beat).round().max(0.0) * beat;
                        store.seek(tick as u64);
                    },
                    for b in roll.get().bars {
                        div { key: b.key.clone(), class: "bar-number",
                            style: {format!("left: {}px;", b.left + 64.0)},
                            {b.number.to_string()}
                        }
                    }
                }
                div { class: "grid", style: {|| format!("height: {}px;", roll.get().height)},
                    for row in roll.get().rows {
                        div { key: row.key.clone(),
                            class: {if row.black { "row black" } else { "row" }},
                            style: {format!("top: {}px; height: {ROW}px;", row.top)},
                            span {
                                class: "key-label",
                                onclick: {
                                    let pitch = row.pitch;
                                    move || {
                                        if let Some(t) = store.selected_track.get() {
                                            let q = compypal_core::PPQ as u64;
                                            store.audition(t, &[compypal_core::Note { pitch, velocity: 100, start: 0, duration: q }]);
                                        }
                                    }
                                },
                                {row.label.clone()}
                            }
                        }
                    }
                    for b in roll.get().bars {
                        div { key: b.key.clone(), class: "bar-line",
                            style: {format!("left: {}px;", b.left + 64.0)},
                        }
                    }
                    for n in roll.get().notes {
                        div { key: n.key.clone(), class: "note",
                            style: {format!("left: {}px; top: {}px; width: {}px; height: {}px; opacity: {:.2};",
                                n.left + 64.0, n.top + 1.0, n.width, ROW - 2.0, 0.45 + n.velocity as f64 / 127.0 * 0.55)},
                        }
                    }
                    div {
                        class: "cursor-line",
                        style: {|| format!("left: {}px;", store.cursor.get() as f64 * store.zoom.get() / compypal_core::PPQ as f64 + KEYS)},
                    }
                    div {
                        class: "playhead",
                        style: {|| match store.song_seconds() {
                            Some(secs) => {
                                let tick = store.project.with(|p| p.tempo.seconds_to_tick(secs));
                                format!("left: {}px;", tick * store.zoom.get() / compypal_core::PPQ as f64 + KEYS)
                            }
                            None => "display: none;".into(),
                        }},
                    }
                    for n in roll.get().live {
                        div { key: n.key.clone(), class: "note live",
                            style: {format!("left: {}px; top: {}px; width: {}px; height: {}px;",
                                n.left + KEYS, n.top + 1.0, n.width, ROW - 2.0)},
                        }
                    }
                    for n in roll.get().raw {
                        div { key: n.key.clone(), class: "note raw",
                            style: {format!("left: {}px; top: {}px; width: {}px; height: {}px;",
                                n.left + 64.0, n.top + 1.0, n.width, ROW - 2.0)},
                        }
                    }
                }
            }
        }
    }
}

/// One box per figure, labelled with its chord, above the roll. Click one to
/// retype its chord; click "+" to continue the music with a new chord.
#[component]
fn FigureLane() -> NodeHandle {
    let store = use_store::<Store>();
    let lane = Memo::new(move || store.lane());
    rsx! {
        div { class: "lane",
            div { class: "lane-label", "Figures" }
            for item in lane.get().items {
                div {
                    key: item.key.clone(),
                    class: {
                        let i = item.index;
                        let unsure = item.unsure;
                        move || {
                            let mut c = String::from("figure");
                            if store.editing.get() == Some(Slot::Figure(i)) { c.push_str(" editing"); }
                            if unsure { c.push_str(" unsure"); }
                            c
                        }
                    },
                    style: {format!("left: {}px; width: {}px;", item.left + KEYS, item.width - 2.0)},
                    onclick: {
                        let i = item.index;
                        move || store.open_editor(Slot::Figure(i))
                    },
                    span { class: "figure-chord", {item.label.clone()} }
                    span { class: "figure-kind", {item.kind} }
                }
            }
            div {
                class: {|| if store.editing.get() == Some(Slot::Append) { "figure add editing" } else { "figure add" }},
                style: {|| format!("left: {}px;", lane.get().append_left + KEYS)},
                onclick: move || store.open_editor(Slot::Append),
                "+"
            }
            if store.editing.get().is_some() {
                ChordEditor {}
            }
        }
    }
}

/// Typeahead for a chord symbol or Roman numeral. Enter applies, Tab
/// applies and moves on, arrows pick a suggestion, Escape cancels.
#[component]
fn ChordEditor() -> NodeHandle {
    let store = use_store::<Store>();
    let input = rsx! {
        input {
            class: "chord-input",
            placeholder: "Am7, V/V, F/A…",
            value: {|| store.draft.get()},
            oninput: move |v: String| {
                store.draft.set(v);
                store.highlight.set(0);
            },
        }
    };
    let root = rsx! {
        div {
            class: "chord-editor",
            style: {|| format!("left: {}px;", store.lane().editor_left.unwrap_or(0.0) + KEYS)},
            {input.clone()}
            div { class: "suggestions",
                for (i, chord) in store.suggestions().into_iter().enumerate() {
                    div {
                        key: format!("{i}:{}", chord.name(compypal_core::theory::Spelling::Sharps)),
                        class: {move || if store.highlight.get() == i { "suggestion active" } else { "suggestion" }},
                        onclick: move || {
                            store.highlight.set(i);
                            store.commit(false);
                        },
                        span { class: "suggestion-chord", {store.chord_label(&chord)} }
                        span { class: "suggestion-alt", {store.other_label(&chord)} }
                    }
                }
            }
            div { class: "editor-hint", "Enter apply · Tab next · Esc cancel" }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| match k.key.as_str() {
            "Enter" => {
                store.commit(false);
                true
            }
            "Tab" => {
                store.commit(true);
                true
            }
            "Escape" => {
                store.close_editor();
                true
            }
            "ArrowDown" => {
                store.move_highlight(1);
                true
            }
            "ArrowUp" => {
                store.move_highlight(-1);
                true
            }
            _ => false,
        }),
        Rc::new(move || store.close_editor()),
    );
    // Typing should go straight into the field: the editor opens because
    // someone wants to type a chord.
    input.focus();
    root
}

#[component]
fn app() -> NodeHandle {
    let engine: Option<&'static compypal_audio::Engine> = match compypal_audio::Engine::start() {
        Ok(e) => Some(Box::leak(Box::new(e))),
        Err(e) => {
            eprintln!("audio disabled: {e}");
            None
        }
    };
    let midi: &'static compypal_audio::input::MidiIn =
        Box::leak(Box::new(compypal_audio::input::MidiIn::new(engine)));
    match compypal_audio::input::default_port() {
        Some(port) => match midi.connect(&port) {
            Ok(()) => eprintln!("MIDI input: {port}"),
            Err(e) => eprintln!("MIDI input: {e}"),
        },
        None => eprintln!("MIDI input: none found"),
    }
    let project = autosave::load().unwrap_or_else(compypal_core::demo::project);
    let store = create_store(Store::new(project, engine, Some(midi)));
    store.select_track(store.selected_track.get().unwrap_or_default());

    // Every change is saved, so a take is never lost to a crash or a quit.
    rinch::core::Effect::new(move || {
        store.project.with(|p| {
            if let Err(e) = autosave::save(p) {
                eprintln!("autosave failed: {e}");
            }
        });
    });

    // Edits while playing are heard on the next pass, without stopping.
    rinch::core::Effect::new(move || {
        let schedule = store.schedule();
        let playing = untracked(|| store.playhead.get()).is_some();
        // A take has its own schedule (count-in, open end); leave it be.
        if let (Some(e), true, false) = (store.engine, playing, untracked(|| store.is_recording())) {
            e.update(schedule);
        }
    });

    // The audio thread can't touch signals; poll it from a plain thread and
    // send() the results across.
    if let Some(engine) = engine {
        let (playhead, audio_status, live_take) = (store.playhead, store.audio_status, store.live_take);
        let capture = midi.capture();
        std::thread::spawn(move || {
            let mut last = (None, String::new());
            let mut last_take = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(33));
                let now = (engine.position(), engine.status());
                if now.0 != last.0 {
                    playhead.send(now.0);
                }
                if now.1 != last.1 {
                    audio_status.send(now.1.clone());
                }
                last = now;
                if let Some(events) = capture.snapshot()
                    && events.len() != last_take
                {
                    last_take = events.len();
                    live_take.send(events);
                } else if capture.elapsed().is_none() {
                    last_take = 0;
                }
            }
        });
    }

    // Space plays and stops, unless someone is typing.
    rinch::core::set_keyboard_interceptor(move |k| {
        if k.is_space() && k.is_down() && store.editing.get().is_none() {
            // Space ends a take too: the same key that starts things stops them.
            store.toggle_play();
            return true;
        }
        false
    });

    rsx! {
        div { class: "app",
            style { {CSS} }
            Transport {}
            div { class: "main",
                Sidebar {}
                PianoRoll {}
            }
            div { class: "statusbar",
                span { {|| store.status.get()} }
                span { class: "audio-status", {|| format!("♪ {}", store.audio_status.get())} }
            }
        }
    }
}

fn main() {
    App::new(app)
        .title("compypal")
        .size(1280, 800)
        .theme(ThemeProviderProps {
            primary_color: Some("violet".into()),
            dark_mode: true,
            ..Default::default()
        })
        .run();
}
