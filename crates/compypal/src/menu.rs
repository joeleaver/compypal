//! The menu bar: File, Edit, View, Transport.
//!
//! Built before the app starts, so each item reaches the store through
//! `Store::current`. Space (play) and the piano roll's keys stay with the
//! keyboard interceptor in `main`: a menu chord would take them from text
//! fields.

use rinch::menu::{Menu, MenuItem};

use crate::store::{Pending, Store, View, ZoomReq};

/// An item that runs `f` on the store.
fn item(label: &str, f: impl Fn(Store) + 'static) -> MenuItem {
    MenuItem::new(label).on_click(move || {
        if let Some(store) = Store::current() {
            f(store);
        }
    })
}

pub fn bar() -> Vec<(&'static str, Menu)> {
    let export = Menu::new()
        .item(item("MIDI file", |s| {
            let _ = s.export_midi();
        }))
        .item(item("ABC notation", |s| {
            let _ = s.export_abc();
        }))
        .item(item("MP3", |s| {
            s.export_audio("mp3");
        }))
        .item(item("WAV", |s| {
            s.export_audio("wav");
        }));
    let file = Menu::new()
        .item(item("New", |s| s.guard(Pending::New)).shortcut("Ctrl+N"))
        .item(item("Open…", |s| s.open_dialog()).shortcut("Ctrl+O"))
        .item(item("Open Recent…", |s| s.toggle_recent()).shortcut("Ctrl+Shift+O"))
        .separator()
        .item(item("Save", |s| {
            let _ = s.save();
        }).shortcut("Ctrl+S"))
        .item(item("Save As…", |s| {
            let _ = s.save_as();
        }).shortcut("Ctrl+Shift+S"))
        .separator()
        .submenu("Export", export)
        .separator()
        .item(item("Quit", |s| s.guard(Pending::Quit)).shortcut("Ctrl+Q"));
    let edit = Menu::new()
        .item(item("Undo", |s| {
            if s.undo().is_none() {
                s.status.set("Nothing to undo".into());
            }
        }).shortcut("Ctrl+Z"))
        .item(item("Redo", |s| s.redo()).shortcut("Ctrl+Shift+Z"))
        .separator()
        .item(item("Rename Song…", |s| s.toggle_rename()))
        .item(item("Add Track", |s| s.add_track()))
        .separator()
        .item(item("Settings…", |s| crate::settings::open(s, crate::settings::Tab::Audio)).shortcut("Ctrl+,"));
    let view = Menu::new()
        .item(item("Sketches", |s| s.view.set(View::Sketches)).shortcut("Ctrl+1"))
        .item(item("Arrange", |s| s.view.set(View::Arrange)).shortcut("Ctrl+2"))
        .item(item("Edit", |s| s.view.set(View::Edit)).shortcut("Ctrl+3"))
        .separator()
        .item(item("Zoom In", |s| s.zoom(ZoomReq::In)).shortcut("Ctrl+="))
        .item(item("Zoom Out", |s| s.zoom(ZoomReq::Out)).shortcut("Ctrl+-"))
        .item(item("Fit Song", |s| s.zoom(ZoomReq::Fit)).shortcut("Ctrl+0"))
        .item(item("Taller Rows", |s| s.zoom(ZoomReq::Taller)).shortcut("Ctrl+Shift+="))
        .item(item("Shorter Rows", |s| s.zoom(ZoomReq::Shorter)).shortcut("Ctrl+Shift+-"))
        .separator()
        .item(item("All Tracks", |s| s.stacked.update(|v| *v = !*v)))
        .item(item("What Was Played", |s| s.show_raw.update(|v| *v = !*v)))
        .separator()
        .item(item("Agent", crate::chat::toggle).shortcut("Ctrl+L"));
    let transport = Menu::new()
        .item(item("Play / Stop  (Space)", |s| s.toggle_play()))
        .item(item("Record", |s| if s.is_recording() { s.stop_recording() } else { s.record() }))
        .item(item("Free Time", |s| {
            if !s.is_recording() {
                s.free_rec.update(|f| *f = !*f);
            }
        }))
        .separator()
        .item(item("Loop", |s| s.toggle_looping()))
        .item(item("Click", |s| s.metronome.update(|m| *m = !*m)))
        .item(item("Listen", |s| s.toggle_listening()));
    vec![("File", file), ("Edit", edit), ("View", view), ("Transport", transport)]
}
