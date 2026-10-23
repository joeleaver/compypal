//! The Settings dialog (Edit > Settings…): where the devices are picked,
//! out of the way of the music. Audio: what plays and the input heard for
//! a guitar or a voice. MIDI: the keyboard played in.

use std::rc::Rc;

use rinch::prelude::*;

use crate::store::Store;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Audio,
    Midi,
}

pub fn open(store: Store, tab: Tab) {
    store.settings_tab.set(tab);
    store.settings_open.set(true);
    // Inputs come and go (an interface plugged in): list them afresh.
    let devices = store.audio_devices;
    std::thread::spawn(move || devices.send(compypal_audio::mic::devices()));
}

#[derive(Clone, Debug, PartialEq)]
struct PortRow {
    index: usize,
    name: String,
}

fn port_rows() -> Vec<PortRow> {
    compypal_audio::input::ports().into_iter().enumerate().map(|(index, name)| PortRow { index, name }).collect()
}

/// "KeyLab mkII 49:KeyLab mkII 49 MIDI 24:0" -> "KeyLab mkII 49 MIDI".
pub fn short_port_name(name: &str) -> String {
    let port = name.split_once(':').map_or(name, |(_, p)| p);
    match port.rsplit_once(' ') {
        Some((p, ids)) if ids.contains(':') => p.to_string(),
        _ => port.to_string(),
    }
}

#[component]
pub fn SettingsDialog() -> NodeHandle {
    let store = use_store::<Store>();
    let root = rsx! {
        div { class: "confirm-backdrop",
            div { class: "settings-box",
                div { class: "settings-head",
                    div { class: "confirm-title", "Settings" }
                    div { class: "spacer" }
                    Button { size: "xs",
                        variant: {|| if store.settings_tab.get() == Tab::Audio { "filled" } else { "default" }},
                        onclick: move || store.settings_tab.set(Tab::Audio),
                        "Audio"
                    }
                    Button { size: "xs",
                        variant: {|| if store.settings_tab.get() == Tab::Midi { "filled" } else { "default" }},
                        onclick: move || store.settings_tab.set(Tab::Midi),
                        "MIDI"
                    }
                }
                div { class: "settings-body",
                    match store.settings_tab.get() {
                        Tab::Audio => AudioTab {},
                        Tab::Midi => MidiTab {},
                    }
                }
                div { class: "confirm-actions",
                    div { class: "spacer" }
                    Button { size: "xs", variant: "filled", onclick: move || store.settings_open.set(false), "Done" }
                }
            }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| {
            if k.key == "Escape" {
                store.settings_open.set(false);
                return true;
            }
            false
        }),
        Rc::new(|| {}),
    );
    root
}

#[component]
fn AudioTab() -> NodeHandle {
    let store = use_store::<Store>();
    let none_selected = store.audio_device.get().is_none();
    rsx! {
        div {
            div { class: "settings-section", "Output" }
            div { class: "settings-note", {|| store.audio_status.get()} }
            div { class: "settings-section", "Input" }
            div { class: "settings-note",
                "For an instrument without MIDI: a guitar, a voice. Heard while Listen is on, and written down as sketches. Off by default: a microphone hears the room."
            }
            div {
                class: if none_selected { "port-row selected" } else { "port-row" },
                onclick: move || {
                    if let Some(i) = store.audio_device_index() {
                        store.toggle_audio_in(i);
                    }
                },
                "None"
            }
            for (i, name) in store.audio_devices.get().into_iter().map(|d| d.name).enumerate() {
                div {
                    key: i,
                    class: move || if store.audio_device_index() == Some(i) { "port-row selected" } else { "port-row" },
                    onclick: move || store.toggle_audio_in(i),
                    {name}
                }
            }
            if store.audio_device.get().is_some() {
                div { class: "level-meter",
                    div { class: "level-fill",
                        style: {|| {
                            // -80 dB to 0 dB across the meter: a quiet room
                            // shows a sliver, so you can see it's alive.
                            let db = 20.0 * (store.audio_level.get() + 1e-6).log10();
                            format!("width: {:.0}%;", ((db + 80.0) / 80.0 * 100.0).clamp(0.0, 100.0))
                        }}
                    }
                }
            }
        }
    }
}

#[component]
fn MidiTab() -> NodeHandle {
    let store = use_store::<Store>();
    let ports = port_rows();
    let empty = ports.is_empty();
    rsx! {
        div {
            div { class: "settings-section", "Input" }
            div { class: "settings-note",
                "The keyboard you play: heard through the selected track's instrument, recorded by Rec, and kept as sketches while Listen is on. Click the connected one again to disconnect."
            }
            if empty {
                div { class: "settings-note", "No MIDI inputs found. Plug one in and open Settings again." }
            }
            for port in ports.clone() {
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
