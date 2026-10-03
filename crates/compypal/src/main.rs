//! compypal: a MIDI composer and arranger built to work alongside an agent.

mod agent;
mod autosave;
mod store;

use std::rc::Rc;

use compypal_core::cleanup::grid_ticks;
use compypal_core::{Id, Project, gm};
use rinch::prelude::*;
use store::{ARR_ROW, ROW, Slot, Store, View};

/// Width of the key labels down the left of the roll.
const KEYS: f64 = 64.0;

const CSS: &str = include_str!("style.css");

#[derive(Clone, Debug, PartialEq)]
struct TrackRow {
    /// Changes whenever anything shown changes, so the row is rebuilt.
    key: String,
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
        .map(|t| {
            let instrument: String = if t.is_drums() { "Drums".into() } else { gm::program_name(t.program).into() };
            let notes = t.clips.iter().map(|c| c.notes.len()).sum();
            TrackRow {
                key: format!("{}:{}:{instrument}:{notes}", t.id, t.name),
                id: t.id,
                name: t.name.clone(),
                instrument,
                notes,
                channel: t.channel + 1,
            }
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
            div { class: "project-anchor",
                span {
                    class: "project-name",
                    onclick: move || store.toggle_projects(),
                    {|| format!("{} ▾", store.project.with(|p| p.name.clone()))}
                }
                if store.projects_open.get() {
                    ProjectsPanel {}
                }
            }
            div { class: "view-switch",
                Button { size: "xs",
                    variant: {|| if store.view.get() == View::Arrange { "filled" } else { "default" }},
                    onclick: move || store.view.set(View::Arrange),
                    "Arrange"
                }
                Button { size: "xs",
                    variant: {|| if store.view.get() == View::Edit { "filled" } else { "default" }},
                    onclick: move || store.view.set(View::Edit),
                    "Edit"
                }
            }
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
            Button { size: "xs",
                color: "teal",
                variant: {|| if store.listening.get() { "filled" } else { "default" }},
                disabled: store.journal.is_none(),
                onclick: move || store.toggle_listening(),
                {|| match (store.listening.get(), store.jams.with(|j| j.first().is_some_and(|r| r.live))) {
                    (true, true) => "◉ Listening (jam!)",
                    (true, false) => "◉ Listening",
                    _ => "○ Listen",
                }}
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
                onclick: move || { store.undo(); },
                "Undo"
            }
            Button { size: "xs", variant: "default",
                disabled: {|| store.history.with(|h| h.redo_label().is_none())},
                onclick: move || store.redo(),
                "Redo"
            }
            div { class: "divider" }
            Button { size: "xs", variant: "light", onclick: move || { let _ = store.export_midi(); }, "Export MIDI" }
            Button { size: "xs", variant: "light", onclick: move || { let _ = store.export_abc(); }, "Export ABC" }
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
                    key: t.key.clone(),
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
                        span {
                            class: "instrument-link",
                            onclick: {
                                let id = t.id;
                                move || {
                                    store.instrument_draft.set(String::new());
                                    store.instrument_edit.set(Some(id));
                                }
                            },
                            {t.instrument.clone()}
                        }
                        {format!(" · ch {} · {} notes", t.channel, t.notes)}
                    }
                    if store.instrument_edit.get() == Some(t.id) {
                        InstrumentPicker {}
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
            if store.listening.get() || store.jams.with(|j| !j.is_empty()) {
                div { class: "sidebar-heading", "Jams" }
            }
            for jam in store.jams.get() {
                div { key: jam.key.clone(), class: {if jam.live { "jam-row live" } else { "jam-row" }},
                    div { class: "session-head",
                        div { class: "track-name", {jam.when.clone()} }
                        div { class: "jam-actions",
                            Button { size: "xs", variant: "subtle",
                                onclick: move || {
                                    let r = agent::run_tool(store, "audition_jam", serde_json::json!({"jam": jam.id}));
                                    store.status.set(r.unwrap_or_else(|e| e));
                                },
                                "▶"
                            }
                            Button { size: "xs", variant: "subtle",
                                onclick: move || {
                                    let r = agent::run_tool(store, "keep_jam", serde_json::json!({"jam": jam.id}));
                                    store.status.set(r.unwrap_or_else(|e| e));
                                },
                                "Keep"
                            }
                        }
                    }
                    div { class: "track-detail", {jam.detail.clone()} }
                }
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
    // Only the grid scrolls vertically; the chord lane and ruler stay put.
    let viewport = rsx! {
        div { class: "grid-viewport",
                div {
                class: "grid",
                style: {|| format!("height: {}px;", roll.get().height)},
                // One handler for the whole grid: it hit-tests notes itself,
                // so nothing depends on how presses bubble.
                onmousedown: move || grid_press(store),
                for row in roll.get().rows {
                    div { key: row.key.clone(),
                        class: {if row.black { "row black" } else { "row" }},
                        style: {format!("top: {}px; height: {ROW}px;", row.top)},
                        span { class: "key-label", {row.label.clone()} }
                    }
                }
                for b in roll.get().bars {
                    div { key: b.key.clone(), class: "bar-line",
                        style: {format!("left: {}px;", b.left + 64.0)},
                    }
                }
                for n in roll.get().notes {
                    div { key: n.key.clone(), class: {if n.selected { "note selected" } else { "note" }},
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
    };
    let root = rsx! {
        div { class: "roll-scroll",
            div {
                class: "roll roll-fixed",
                style: {|| format!("width: {}px;", roll.get().width + 64.0)},
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
                {viewport.clone()}
            }
        }
    };
    follow_playhead(&root, store, move || store.zoom.get() / compypal_core::PPQ as f64, KEYS);
    // Bring the selected track's notes into view when it changes.
    {
        let viewport = viewport.clone();
        rinch::core::Effect::new(move || {
            let _ = store.selected_track.get();
            let (top, center) = untracked(|| (store.roll().top_pitch, store.center_pitch()));
            let viewport = viewport.clone();
            // After layout, so the viewport knows its height.
            set_timeout(0, move || {
                let h = viewport.client_height();
                let y = (top.saturating_sub(center)) as f64 * ROW - h / 2.0;
                viewport.set_scroll_top(y.max(0.0));
            });
        });
    }
    rsx! {
        div { class: "editor",
            div { class: "view-toolbar",
                Button { size: "xs",
                    variant: {|| if store.stacked.get() { "filled" } else { "default" }},
                    onclick: move || {
                        store.close_editor();
                        store.stacked.update(|v| *v = !*v);
                    },
                    "All tracks"
                }
                span { class: "view-title",
                    {|| if store.stacked.get() {
                        "Every track, under the song's chords".to_string()
                    } else {
                        store.selected_track.get().and_then(|id| store.project.with(|p| p.track(id).map(|t| t.name.clone()))).unwrap_or_default()
                    }}
                }
                Button { size: "xs", variant: "default",
                    onclick: move || store.snap.update(|s| {
                        // Cycle through the usual grids, then off.
                        let order = ["1/4", "1/8", "1/16", "1/32", "1/8t", "1/16t"];
                        let grids: Vec<u64> = order.iter().map(|g| grid_ticks(g).unwrap()).collect();
                        *s = match grids.iter().position(|g| g == s) {
                            Some(i) if i + 1 < grids.len() => grids[i + 1],
                            Some(_) => 0,
                            None => grids[0],
                        };
                    }),
                    {|| match store.snap.get() {
                        0 => "Snap off".to_string(),
                        t => format!("Snap {}", compypal_core::text::format_duration(t)),
                    }}
                }
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
            }
            if store.stacked.get() {
                StackedRolls {}
            } else {
                {root.clone()}
            }
        }
    }
}

/// Every track's piano roll stacked under one chord lane for the song.
/// Changing a chord there re-voices all the parts at once.
#[component]
fn StackedRolls() -> NodeHandle {
    let store = use_store::<Store>();
    let song = Memo::new(move || store.song_lane());
    let stack = Memo::new(move || store.stack());
    let roll = Memo::new(move || store.roll());
    let root = rsx! {
        div { class: "roll-scroll stacked",
            div { class: "roll", style: {|| format!("width: {}px;", roll.get().width + KEYS)},
                for b in roll.get().bars {
                    div { key: b.key.clone(), class: "bar-line", style: {format!("left: {}px;", b.left + KEYS)} }
                }
                div { class: "lane song-lane",
                    div { class: "lane-label", "Song" }
                    for item in song.get().items {
                        div {
                            key: item.key.clone(),
                            class: {
                                let i = item.index;
                                let unsure = item.unsure;
                                move || {
                                    let mut c = String::from("figure");
                                    if store.editing.get() == Some(Slot::Song(i)) { c.push_str(" editing"); }
                                    if unsure { c.push_str(" unsure"); }
                                    c
                                }
                            },
                            style: {format!("left: {}px; width: {}px;", item.left + KEYS, item.width - 2.0)},
                            onclick: {
                                let i = item.index;
                                move || store.open_editor(Slot::Song(i))
                            },
                            span { class: "figure-chord", {item.label.clone()} }
                            span { class: "figure-kind", "all tracks" }
                        }
                    }
                    if store.editing.get().is_some_and(|s| matches!(s, Slot::Song(_))) {
                        ChordEditor {}
                    }
                }
                div { class: "stack-sections",
                    div { class: "lane-label", "Sections" }
                    for sec in store.roll_sections() {
                        div {
                            key: sec.key.clone(),
                            class: "arr-section",
                            style: {format!("left: {}px; width: {}px;", sec.left + KEYS, sec.width - 2.0)},
                            onclick: {
                                let (a, b) = (sec.first, sec.last);
                                move || store.select_bars(a, b)
                            },
                            {sec.name.clone()}
                        }
                    }
                }
                for row in stack.get() {
                    div { key: row.key.clone(), class: "stack-track",
                        div {
                            class: {
                                let id = row.track;
                                move || if store.selected_track.get() == Some(id) { "stack-head selected" } else { "stack-head" }
                            },
                            onclick: {
                                let id = row.track;
                                move || store.select_track(id)
                            },
                            {format!("{}  ·  {}", row.name, row.instrument)}
                        }
                        div { class: "stack-roll", style: {format!("height: {}px;", row.height)},
                            for y in row.c_lines.clone() {
                                div { key: y.to_string(), class: "stack-c", style: {format!("top: {y}px;")} }
                            }
                            for n in row.notes.clone() {
                                div { key: n.key.clone(), class: "note stack-note",
                                    style: {format!("left: {}px; top: {}px; width: {}px; opacity: {:.2};",
                                        n.left + KEYS, n.top, n.width, 0.45 + n.velocity as f64 / 127.0 * 0.55)},
                                }
                            }
                        }
                    }
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
            }
        }
    };
    follow_playhead(&root, store, move || store.zoom.get() / compypal_core::PPQ as f64, KEYS);
    root
}

/// A press in the piano roll grid: hear a key in the key column, or start
/// a note gesture (select, move, resize, draw) that follows the pointer
/// until release.
fn grid_press(store: Store) {
    let c = get_click_context();
    if c.button != MouseButton::Left {
        return;
    }
    let x = (c.mouse_x - c.element_x) as f64 - KEYS;
    let y = (c.mouse_y - c.element_y) as f64;
    if x < 0.0 {
        let roll = store.roll();
        let pitch = (roll.top_pitch as i32 - (y / ROW).floor() as i32).clamp(0, 127) as u8;
        if let Some(t) = store.selected_track.get() {
            let q = compypal_core::PPQ as u64;
            store.audition(t, &[compypal_core::Note { pitch, velocity: 100, start: 0, duration: q }]);
        }
        return;
    }
    let Some(drag) = store.roll_press(x, y, c.modifiers.shift) else { return };
    let (sx, sy) = (c.mouse_x, c.mouse_y);
    let live = Rc::new(std::cell::RefCell::new(drag));
    let (on_move, on_end) = (live.clone(), live);
    Drag::absolute()
        .on_move(move |mx, my| store.roll_drag(&mut on_move.borrow_mut(), (mx - sx) as f64, (my - sy) as f64))
        .on_end(move |_, _| {
            let d = on_end.borrow().clone();
            store.roll_release(d);
        })
        .on_cancel(move |_, _| store.note_drag.set(None))
        .start();
}

/// Keeps the playhead in view while playing, scrolling a page at a time.
fn follow_playhead(scroller: &NodeHandle, store: Store, px_per_tick: impl Fn() -> f64 + 'static, offset: f64) {
    let scroller = scroller.clone();
    rinch::core::Effect::new(move || {
        let Some(secs) = store.song_seconds() else { return };
        let tick = store.project.with(|p| p.tempo.seconds_to_tick(secs));
        let x = tick * untracked(&px_per_tick) + offset;
        let (left, width) = (scroller.scroll_left(), scroller.client_width());
        if width > 0.0 && (x > left + width - 40.0 || x < left + offset) {
            scroller.set_scroll_left((x - offset - 40.0).max(0.0));
        }
    });
}

/// The whole song: sections across the top, a row per track with its
/// clips, and whole-bar editing of the selection.
#[component]
fn Arranger() -> NodeHandle {
    let store = use_store::<Store>();
    let arr = Memo::new(move || store.arrangement());
    let timeline = rsx! {
        div { class: "arr-scroll",
            div { class: "arr", style: {|| format!("width: {}px;", arr.get().width)},
                div { class: "arr-sections",
                    for sec in arr.get().sections {
                        div {
                            key: sec.key.clone(),
                            class: "arr-section",
                            style: {format!("left: {}px; width: {}px;", sec.left, sec.width - 2.0)},
                            onclick: {
                                let (a, b) = (sec.first, sec.last);
                                move || store.select_bars(a, b)
                            },
                            {sec.name.clone()}
                        }
                    }
                }
                div {
                    class: "arr-ruler",
                    onclick: move || {
                        let c = get_click_context();
                        let x = (c.mouse_x - c.element_x) as f64;
                        let tick = (x / arr.get().px).max(0.0) as u64;
                        let bar = store.project.with(|p| compypal_core::text::bar_of(p, tick));
                        store.select_bar(bar, c.modifiers.shift);
                    },
                    for b in arr.get().bars {
                        div { key: b.key.clone(), class: "bar-number", style: {format!("left: {}px;", b.left)},
                            {b.number.to_string()}
                        }
                    }
                }
                div { class: "arr-rows",
                    for b in arr.get().bars {
                        div { key: b.key.clone(), class: "bar-line", style: {format!("left: {}px;", b.left)} }
                    }
                    for row in arr.get().rows {
                        div {
                            key: row.key.clone(),
                            class: {if row.muted { "arr-row muted" } else { "arr-row" }},
                            style: {format!("height: {ARR_ROW}px;")},
                            for clip in row.clips.clone() {
                                div {
                                    key: clip.key.clone(),
                                    class: {if clip.recorded { "arr-clip recorded" } else { "arr-clip" }},
                                    style: {format!("left: {}px; width: {}px;", clip.left, clip.width - 2.0)},
                                    onclick: {
                                        let (track, start) = (clip.track, clip.start);
                                        move || store.open_clip(track, start)
                                    },
                                }
                            }
                            for n in row.notes.clone() {
                                div { key: n.key.clone(), class: "arr-note",
                                    style: {format!("left: {}px; top: {}px; width: {}px;", n.left, n.top, n.width)},
                                }
                            }
                        }
                    }
                    div {
                        class: "arr-selection",
                        style: {|| match store.bar_sel.get() {
                            Some((a, b)) => {
                                // Read the memo before borrowing the project: a
                                // memo recomputing inside `with` panics.
                                let px = arr.get().px;
                                store.project.with(|p| {
                                let (from, to) = (compypal_core::text::bar_start(p, a), compypal_core::text::bar_start(p, b + 1));
                                format!("left: {}px; width: {}px;", from as f64 * px, (to - from) as f64 * px)
                                })
                            }
                            None => "display: none;".into(),
                        }},
                    }
                    div {
                        class: "playhead",
                        style: {|| match store.song_seconds() {
                            Some(secs) => {
                                let tick = store.project.with(|p| p.tempo.seconds_to_tick(secs));
                                format!("left: {}px;", tick * arr.get().px)
                            }
                            None => "display: none;".into(),
                        }},
                    }
                }
            }
        }
    };
    follow_playhead(&timeline, store, move || arr.get().px, 0.0);
    rsx! {
        div { class: "arranger",
            div { class: "arr-toolbar",
                Button { size: "xs", variant: "default", onclick: move || store.arr_zoom.update(|z| *z = (*z / 1.5).max(4.0)), "−" }
                Button { size: "xs", variant: "default", onclick: move || store.arr_zoom.update(|z| *z = (*z * 1.5).min(120.0)), "+" }
                {|| match store.bar_sel.get() {
                    Some((a, b)) if a == b => format!("Bar {a}"),
                    Some((a, b)) => format!("Bars {a}–{b}"),
                    None => "Click a bar to select it; shift-click to select a range.".into(),
                }}
                if store.bar_sel.get().is_some() {
                    div { class: "arr-actions",
                        Button { size: "xs", variant: "light", onclick: move || store.bars_action("duplicate"), "Duplicate" }
                        Button { size: "xs", variant: "default", onclick: move || store.bars_action("insert"), "Insert empty before" }
                        Button { size: "xs", variant: "default", color: "red", onclick: move || store.bars_action("delete"), "Delete bars" }
                        div { class: "divider" }
                        div { class: "section-namer-anchor",
                            Button { size: "xs", variant: "light",
                                onclick: move || store.section_edit.update(|v| *v = !*v),
                                {|| match store.section_draft.get() {
                                    name if name.is_empty() => "Name section…".to_string(),
                                    name => format!("Section: {name} (rename…)"),
                                }}
                            }
                            if store.section_edit.get() {
                                SectionNamer {}
                            }
                        }
                    }
                }
            }
            div { class: "arr-body",
                div { class: "arr-heads",
                    div { class: "arr-head-spacer" }
                    for row in arr.get().rows {
                        div {
                            key: row.key.clone(),
                            class: {
                                let id = row.track;
                                move || if store.selected_track.get() == Some(id) { "arr-head selected" } else { "arr-head" }
                            },
                            style: {format!("height: {ARR_ROW}px;")},
                            onclick: {
                                let id = row.track;
                                move || store.select_track(id)
                            },
                            div { class: "track-name", {row.name.clone()} }
                            div { class: "track-detail", {row.instrument.clone()} }
                        }
                    }
                }
                {timeline}
            }
        }
    }
}

/// Names the selected bars. Enter saves (an empty name removes the
/// section), Escape cancels.
#[component]
fn SectionNamer() -> NodeHandle {
    let store = use_store::<Store>();
    let input = rsx! {
        input {
            class: "chord-input",
            placeholder: "Verse, Chorus, Bridge…",
            value: {|| store.section_draft.get()},
            oninput: move |v: String| store.section_draft.set(v),
        }
    };
    let root = rsx! {
        div { class: "section-namer",
            {input.clone()}
            div { class: "editor-hint", "Enter save · empty removes · Esc cancel" }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| match k.key.as_str() {
            "Enter" => {
                store.name_section();
                store.section_edit.set(false);
                true
            }
            "Escape" => {
                store.section_edit.set(false);
                true
            }
            _ => false,
        }),
        Rc::new(move || store.section_edit.set(false)),
    );
    input.focus();
    root
}

/// The projects popover: open one, or rename, duplicate or start anew.
#[component]
fn ProjectsPanel() -> NodeHandle {
    let store = use_store::<Store>();
    let input = rsx! {
        input {
            class: "chord-input",
            placeholder: "Project name",
            value: {|| store.rename_draft.get()},
            oninput: move |v: String| store.rename_draft.set(v),
        }
    };
    let now = compypal_audio::journal::now();
    let rows: Vec<ProjectRow> = store
        .project_list
        .get()
        .into_iter()
        .enumerate()
        .map(|(index, p)| ProjectRow {
            index,
            current: p.stem == store.project_file.get(),
            name: p.name.clone(),
            detail: format!(
                "{} · {} track(s) · {} notes{}",
                compypal_mcp::tools::when(p.modified, now),
                p.tracks,
                p.notes,
                if p.sessions > 0 { format!(" · {} take(s)", p.sessions) } else { String::new() }
            ),
        })
        .collect();
    let root = rsx! {
        div { class: "projects-panel",
            div { class: "rename-row",
                {input.clone()}
                Button { size: "xs", variant: "light",
                    onclick: move || store.rename_project(&store.rename_draft.get()),
                    "Rename"
                }
            }
            div { class: "project-actions",
                Button { size: "xs", variant: "light", onclick: move || store.new_project(), "New project" }
                Button { size: "xs", variant: "default", onclick: move || store.duplicate_project(), "Duplicate" }
            }
            div { class: "project-list",
                for row in rows.clone() {
                    div {
                        key: row.index,
                        class: {if row.current { "project-row current" } else { "project-row" }},
                        onclick: move || {
                            let stem = store.project_list.with(|l| l.get(row.index).map(|p| p.stem.clone()));
                            if let Some(stem) = stem
                                && let Err(e) = store.open_project(&stem)
                            {
                                store.status.set(e);
                            }
                        },
                        div { class: "track-name", {row.name.clone()} }
                        div { class: "track-detail", {row.detail.clone()} }
                    }
                }
            }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| match k.key.as_str() {
            "Enter" => {
                store.rename_project(&store.rename_draft.get());
                true
            }
            "Escape" => {
                store.projects_open.set(false);
                true
            }
            _ => false,
        }),
        Rc::new(move || store.projects_open.set(false)),
    );
    root
}

#[derive(Clone, Debug, PartialEq)]
struct ProjectRow {
    index: usize,
    current: bool,
    name: String,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InstrumentRow {
    program: u8,
    name: &'static str,
}

/// Picks a General MIDI instrument by typing part of its name.
#[component]
fn InstrumentPicker() -> NodeHandle {
    let store = use_store::<Store>();
    let input = rsx! {
        input {
            class: "chord-input",
            placeholder: "piano, strings, bass…",
            value: {|| store.instrument_draft.get()},
            oninput: move |v: String| store.instrument_draft.set(v),
        }
    };
    let root = rsx! {
        div { class: "instrument-picker",
            {input.clone()}
            div { class: "suggestions",
                for item in store.instrument_suggestions().into_iter().map(|(program, name)| InstrumentRow { program, name }) {
                    div {
                        key: item.program,
                        class: "suggestion",
                        onclick: move || {
                            if let Some(t) = store.instrument_edit.get() {
                                store.set_instrument(t, item.program);
                            }
                        },
                        span { class: "suggestion-chord", {item.name} }
                        span { class: "suggestion-alt", {format!("{}", item.program + 1)} }
                    }
                }
            }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| match k.key.as_str() {
            "Enter" => {
                if let (Some(t), Some((program, _))) = (store.instrument_edit.get(), store.instrument_suggestions().first().copied()) {
                    store.set_instrument(t, program);
                }
                true
            }
            "Escape" => {
                store.instrument_edit.set(None);
                true
            }
            _ => false,
        }),
        Rc::new(move || store.instrument_edit.set(None)),
    );
    input.focus();
    root
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
            if store.editing.get().is_some_and(|s| !matches!(s, Slot::Song(_))) {
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
            style: {|| {
                let left = match store.editing.get() {
                    Some(Slot::Song(_)) => store.song_lane().editor_left,
                    _ => store.lane().editor_left,
                };
                format!("left: {}px;", left.unwrap_or(0.0) + KEYS)
            }},
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
    // The journal lives as long as the app; MIDI input holds a handle too.
    let journal: Option<&'static std::sync::Arc<compypal_audio::journal::Journal>> =
        match autosave::journal_dir().map(compypal_audio::journal::Journal::open) {
            Some(Ok(j)) => {
                j.set_listening(autosave::load_settings().listening);
                Some(Box::leak(Box::new(j)))
            }
            Some(Err(e)) => {
                eprintln!("journal unavailable: {e}");
                None
            }
            None => None,
        };
    let midi: &'static compypal_audio::input::MidiIn =
        Box::leak(Box::new(compypal_audio::input::MidiIn::new(engine, journal.cloned())));
    match compypal_audio::input::default_port() {
        Some(port) => match midi.connect(&port) {
            Ok(()) => eprintln!("MIDI input: {port}"),
            Err(e) => eprintln!("MIDI input: {e}"),
        },
        None => eprintln!("MIDI input: none found"),
    }
    let (project, stem) = autosave::open_initial();
    let store = create_store(Store::new(project, engine, Some(midi), journal.map(|j| &**j), stem));
    store.select_track(store.selected_track.get().unwrap_or_default());
    agent::start(store);

    // Keep the sidebar's jam list fresh: a jam appears while it's played and
    // settles once the silence after it is long enough.
    if let Some(j) = journal {
        let (j, jams) = (j.clone(), store.jams);
        std::thread::spawn(move || {
            let mut last: Vec<store::JamRow> = Vec::new();
            loop {
                let now = compypal_audio::journal::now();
                let rows: Vec<store::JamRow> = j
                    .jams()
                    .into_iter()
                    .take(8)
                    .map(|(s, live)| {
                        let when = if live { "● now".to_string() } else { compypal_mcp::tools::when(s.start, now) };
                        let tempo = match s.tempo {
                            Some(t) if t.confidence >= 0.35 => format!(" · ~{:.0} BPM", t.bpm),
                            _ => String::new(),
                        };
                        let detail = format!(
                            "{} · {} notes{tempo}{}",
                            compypal_mcp::tools::mmss(s.duration()),
                            s.notes,
                            s.key.as_deref().map(|k| format!(" · {k}")).unwrap_or_default()
                        );
                        store::JamRow { key: format!("{}:{when}:{detail}", s.id), id: s.id, when, detail, live }
                    })
                    .collect();
                if rows != last {
                    jams.send(rows.clone());
                    last = rows;
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
    }

    // Every change is saved, so a take is never lost to a crash or a quit.
    rinch::core::Effect::new(move || {
        let stem = untracked(|| store.project_file.get());
        store.project.with(|p| {
            if let Err(e) = autosave::save_project(&stem, p) {
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
        if k.is_down()
            && !store.is_typing()
            && store.view.get() == View::Edit
            && !store.stacked.get()
            && !k.is_space()
            && store.roll_key(&k.key, k.shift, k.ctrl || k.meta)
        {
            return true;
        }
        if k.is_space() && k.is_down() && !store.is_typing() {
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
                match store.view.get() {
                    View::Edit => PianoRoll {},
                    View::Arrange => Arranger {},
                }
            }
            div { class: "statusbar",
                span { {|| store.status.get()} }
                span { class: "right-status",
                    span { class: {|| if store.agents.get() > 0 { "agent-status on" } else { "agent-status" }},
                        {|| agent::status_text(store.agents.get())}
                    }
                    span { class: "audio-status", {|| format!("♪ {}", store.audio_status.get())} }
                }
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
    agent::shutdown();
}
