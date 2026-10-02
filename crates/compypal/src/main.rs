//! compypal: a MIDI composer and arranger built to work alongside an agent.

mod store;

use compypal_core::cleanup::grid_ticks;
use compypal_core::{Id, Project, gm};
use rinch::prelude::*;
use store::{ROW, Store};

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
                variant: {|| if store.show_raw.get() { "filled" } else { "default" }},
                onclick: move || store.show_raw.update(|v| *v = !*v),
                "Show raw take"
            }
            Button { size: "xs", variant: "default", onclick: move || store.zoom.update(|z| *z = (*z / 1.5).max(8.0)), "−" }
            Button { size: "xs", variant: "default", onclick: move || store.zoom.update(|z| *z = (*z * 1.5).min(400.0)), "+" }
            div { class: "divider" }
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
            div { class: "sidebar-heading", "Tracks" }
            for t in store.project.with(track_rows) {
                div {
                    key: t.id.0,
                    class: {
                        let id = t.id;
                        move || if store.selected_track.get() == Some(id) { "track-row selected" } else { "track-row" }
                    },
                    onclick: {
                        let id = t.id;
                        move || store.selected_track.set(Some(id))
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
                    div { class: "track-name", {s.name.clone()} }
                    div { class: "track-detail", {s.detail.clone()} }
                }
            }
            if store.project.with(|p| p.sessions.is_empty()) {
                div { class: "empty", "No recordings yet" }
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
                div { class: "ruler",
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
                            span { class: "key-label", {row.label.clone()} }
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

#[component]
fn app() -> NodeHandle {
    let store = create_store(Store::new(compypal_core::demo::project()));
    rsx! {
        div { class: "app",
            style { {CSS} }
            Transport {}
            div { class: "main",
                Sidebar {}
                PianoRoll {}
            }
            div { class: "statusbar", {|| store.status.get()} }
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
