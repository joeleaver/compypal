//! The Sketches view: everything you played, one idea at a time. Sketches
//! come from two places, shown as one list: the journal (whatever was
//! played while listening, split into ideas by `jam::split_sketches`) and
//! the song's own sessions (recorded takes, and sketches already added).
//! A take recorded while listening is in both; it's shown once, as part of
//! the song.

use std::rc::Rc;

use compypal_core::jam::{self, Sketch};
use compypal_core::figure::{self, FigureSettings};
use compypal_core::{Id, MeterChange, Project, RawEvent, RawMsg, Session, text};
use compypal_mcp::tools::{day_label, mmss, when};
use rinch::prelude::*;

use crate::agent;
use crate::store::{Store, View};

/// Where a sketch lives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// In the journal, by its id (unix seconds of its start).
    Journal(u64),
    /// A session of the open song.
    Session(Id),
}

/// A sketch from the journal, as the journal thread hands it over.
#[derive(Clone, Debug, PartialEq)]
pub struct Played {
    pub sketch: Sketch,
    /// Still being played.
    pub live: bool,
    /// Put out of sight; shown only under the Hidden filter.
    pub hidden: bool,
    pub thumb: Vec<(f32, f32)>,
}

/// Which sketches the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Filter {
    #[default]
    All,
    InSong,
    NotInSong,
    Hidden,
}

/// A small picture of the notes: (time, pitch), both 0..1.
pub fn thumb(events: &[RawEvent]) -> Vec<(f32, f32)> {
    let ons: Vec<(f64, u8)> = events
        .iter()
        .filter_map(|e| match e.msg {
            RawMsg::NoteOn { pitch, velocity } if velocity > 0 => Some((e.t, pitch)),
            _ => None,
        })
        .collect();
    let (Some(first), Some(last)) = (ons.first(), ons.last()) else { return Vec::new() };
    let span = (last.0 - first.0).max(1e-3);
    let lo = ons.iter().map(|o| o.1).min().unwrap_or(0) as f32;
    let hi = ons.iter().map(|o| o.1).max().unwrap_or(127) as f32;
    let range = (hi - lo).max(6.0);
    // A few dozen dots tell the shape; more is noise at this size.
    let step = (ons.len() / 28).max(1);
    ons.iter()
        .step_by(step)
        .map(|(t, p)| (((t - first.0) / span) as f32, 1.0 - (*p as f32 - lo) / range))
        .collect()
}

/// One line in the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub key: String,
    pub source: Source,
    pub name: String,
    pub day: String,
    pub length: String,
    pub detail: String,
    /// Where it's used in the song, if it is.
    pub in_song: Option<String>,
    /// The line saying so, and its class.
    pub tag: String,
    pub tag_class: &'static str,
    pub live: bool,
    pub hidden: bool,
    /// Heard through a microphone: the notes are a transcription.
    pub audio: bool,
    pub thumb: Vec<(f32, f32)>,
}

/// What a session is called in the list. Takes recorded before sketches
/// had names ("Take 3") are named by their music.
pub fn session_name(s: &Session) -> String {
    let numbered = s.name.strip_prefix("Take ").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    match (numbered, jam::summarize(&s.events)) {
        (true, Some(sum)) => jam::sketch_name(&sum),
        _ => s.name.clone(),
    }
}

/// Where a session's music is in the song, if anywhere, and whether it's
/// heard there (something laid over it mutes it).
fn used_in(p: &Project, session: Id) -> Option<String> {
    p.tracks.iter().find_map(|t| {
        let clips: Vec<_> = t.clips.iter().filter(|c| c.source_session == Some(session)).collect();
        let (first, last) = (clips.first()?, clips.last()?);
        let (a, b) = (text::bar_of(p, first.start), text::bar_of(p, (last.start + last.length).saturating_sub(1)));
        let bars = if a == b { format!("bar {a}") } else { format!("bars {a}–{b}") };
        Some(match clips.iter().filter(|c| c.muted).count() {
            0 => format!("{}, {bars}", t.name),
            n if n == clips.len() => format!("{}, {bars} (muted)", t.name),
            _ => format!("{}, {bars} (partly muted)", t.name),
        })
    })
}

fn tempo_text(s: &Sketch) -> String {
    match s.tempo {
        Some(t) if t.confidence >= 0.35 => format!("~{:.0} BPM", t.bpm),
        _ => "free time".into(),
    }
}

/// The list: the song's sessions and the journal's sketches, newest first,
/// each played thing once.
pub fn items(p: &Project, played: &[Played], now: f64, filter: Filter) -> Vec<Item> {
    let mut out: Vec<(f64, Item)> = Vec::new();
    for s in &p.sessions {
        let start = s.recorded_at as f64;
        let sum = jam::summarize(&s.events);
        let detail = match (&s.click_bpm, &sum) {
            (Some(b), _) if !s.own_tempo => format!("{b:.0} BPM, to the click"),
            (_, Some(sum)) => format!("{} · {}", tempo_text(sum), sum.key.as_deref().unwrap_or("?")),
            _ => String::new(),
        };
        out.push((
            start,
            Item {
                key: format!("s{}:{}", s.id.0, s.name),
                source: Source::Session(s.id),
                name: session_name(s),
                day: day_label(start, now),
                length: mmss(s.duration()),
                detail,
                in_song: used_in(p, s.id),
                tag: String::new(),
                tag_class: "",
                live: false,
                hidden: false,
                audio: s.audio.is_some(),
                thumb: thumb(&s.events),
            },
        ));
    }
    for j in played {
        let s = &j.sketch;
        // The same playing as one of the song's sketches (recorded into it,
        // or added): the song's copy stands for it.
        if p.sessions.iter().any(|x| x.overlaps(s.start, s.end)) {
            continue;
        }
        // A recording not yet written down.
        if s.audio.is_some() && s.notes == 0 {
            out.push((
                s.start,
                Item {
                    key: format!("a{}:{}", s.id, j.live),
                    source: Source::Journal(s.id),
                    name: if j.live { "Listening…".into() } else { "Writing it down…".into() },
                    day: day_label(s.start, now),
                    length: if j.live { "● now".into() } else { mmss(s.duration()) },
                    detail: format!("{} · heard through the mic", when(s.start, now)),
                    in_song: None,
                    tag: String::new(),
                    tag_class: "",
                    live: j.live,
                    hidden: j.hidden,
                    audio: true,
                    thumb: Vec::new(),
                },
            ));
            continue;
        }
        out.push((
            s.start,
            Item {
                key: format!("j{}:{}:{}", s.id, s.notes, j.live),
                source: Source::Journal(s.id),
                name: jam::sketch_name(s),
                day: day_label(s.start, now),
                length: if j.live { "● now".into() } else { mmss(s.duration()) },
                detail: format!("{} · {} · {}", when(s.start, now), tempo_text(s), s.key.as_deref().unwrap_or("?")),
                in_song: None,
                tag: String::new(),
                tag_class: "",
                live: j.live,
                hidden: j.hidden,
                audio: s.audio.is_some(),
                thumb: j.thumb.clone(),
            },
        ));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.retain(|(_, i)| match filter {
        Filter::All => !i.hidden,
        Filter::InSong => matches!(i.source, Source::Session(_)),
        Filter::NotInSong => !i.hidden && i.in_song.is_none(),
        Filter::Hidden => i.hidden,
    });
    out.into_iter()
        .map(|(_, mut i)| {
            (i.tag, i.tag_class) = match &i.in_song {
                Some(place) if place.ends_with("(muted)") => (format!("In the song: {place}"), "sketch-tag"),
                Some(place) => (format!("In the song: {place}"), "sketch-tag used"),
                None => ("Not in the song".into(), "sketch-tag"),
            };
            i
        })
        .collect()
}

/// The selected sketch, drawn to fit: notes and chords as fractions of
/// its length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub name: String,
    pub sub: String,
    pub chips: Vec<String>,
    /// left %, width %, top %.
    pub notes: Vec<(f32, f32, f32)>,
    /// left %, width %, name.
    pub chords: Vec<(f32, f32, String)>,
    pub in_song: Option<String>,
    pub hidden: bool,
    /// Heard through a microphone; "Hear it" plays the recording.
    pub audio: bool,
    /// Still being written down: no notes yet.
    pub pending: bool,
}

fn events_of(store: Store, source: Source) -> Vec<RawEvent> {
    match source {
        Source::Session(id) => store.project.with(|p| p.session(id).map(|s| s.events.clone()).unwrap_or_default()),
        Source::Journal(id) => store
            .journal
            .and_then(|j| j.sketch(id).map(|(s, _)| j.sketch_events(&s, s.start - 0.01, s.end + 0.01)))
            .unwrap_or_default(),
    }
}

/// The recording behind a sketch, if it was heard rather than played,
/// with the stretch its notes cover (in the recording's clock).
fn recording_of(store: Store, source: Source) -> Option<(compypal_core::AudioRef, f64, f64)> {
    match source {
        Source::Session(id) => store.project.with(|p| {
            let s = p.session(id)?;
            let (a, b) = (s.events.first()?.t, s.events.last()?.t);
            Some((s.audio?, a, b))
        }),
        Source::Journal(id) => {
            let (s, _) = store.journal?.sketch(id)?;
            Some((s.audio?, s.start, s.end))
        }
    }
}

pub fn detail(store: Store, source: Source) -> Option<Detail> {
    let audio = recording_of(store, source).is_some();
    if let Source::Journal(id) = source
        && let Some((s, live)) = store.journal.and_then(|j| j.sketch(id)).filter(|(s, _)| s.audio.is_some() && s.notes == 0)
    {
        let now = compypal_audio::journal::now();
        return Some(Detail {
            name: if live { "Listening…".into() } else { "Writing it down…".into() },
            sub: format!("{} · {}", when(s.start, now), mmss(s.duration())),
            chips: vec!["audio".into(), if live { "still playing".into() } else { "finding the notes".into() }],
            audio: true,
            pending: true,
            ..Default::default()
        });
    }
    let events = events_of(store, source);
    let sum = jam::summarize(&events)?;
    let notes = jam::jam_notes(&events, sum.tempo.as_ref().filter(|t| t.confidence >= 0.3));
    // Drawn from the first note: a pickup bar's lead-in is empty space.
    let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
    let total = (notes.iter().map(|n| n.end()).max().unwrap_or(1) - first).max(1) as f32;
    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60) as f32;
    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(72) as f32;
    let range = (hi - lo).max(12.0);
    let pct = |t: u64| t as f32 / total * 100.0;
    let at = |t: u64| pct(t.saturating_sub(first));
    let drawn = notes.iter().map(|n| (at(n.start), (pct(n.duration)).max(0.6), 6.0 + (1.0 - (n.pitch as f32 - lo) / range) * 82.0)).collect();
    let chords = figure::analyze(&notes, &MeterChange::default_four(), &FigureSettings::default())
        .into_iter()
        .map(|f| (at(f.start), pct(f.end - f.start.max(first)), f.chord.name(compypal_core::theory::Spelling::Mixed)))
        .collect();
    let (name, in_song, when_text) = store.project.with(|p| match source {
        Source::Session(id) => {
            let s = p.session(id);
            (s.map(session_name).unwrap_or_default(), used_in(p, id), s.map(|s| when(s.recorded_at as f64, compypal_audio::journal::now())).unwrap_or_default())
        }
        Source::Journal(_) => (jam::sketch_name(&sum), None, when(sum.start, compypal_audio::journal::now())),
    });
    // How a recording was written down: as strums, as one line, as notes.
    let read_as = recording_of(store, source).and_then(|(a, _, _)| store.journal?.transcript(a.file)).map(|t| match t.reading {
        compypal_audio::audio_journal::Reading::Strums => "read as strums",
        compypal_audio::audio_journal::Reading::Line => "read as one line",
        compypal_audio::audio_journal::Reading::Notes => "read as notes",
    });
    let mut chips = if audio { vec!["audio".to_string(), tempo_text(&sum)] } else { vec![tempo_text(&sum)] };
    if let Some(r) = read_as {
        chips.insert(1, r.to_string());
    }
    chips.extend(sum.key.clone());
    chips.push(format!("{} notes", sum.notes));
    if sum.sustain_pedal {
        chips.push("pedal".into());
    }
    let hidden = matches!(source, Source::Journal(id) if store.journal.is_some_and(|j| j.is_hidden(id)));
    Some(Detail { name, sub: format!("{when_text} · {}", mmss(sum.duration())), chips, notes: drawn, chords, in_song, hidden, audio, pending: false })
}

/// Puts a journal sketch out of sight, or back. The list refreshes from the
/// journal thread; meanwhile the flag is flipped here so it moves at once.
fn set_hidden(store: Store, id: u64, hidden: bool) {
    let Some(j) = store.journal else { return };
    match j.set_hidden(id, hidden) {
        Ok(()) => {
            store.played.update(|ps| {
                for p in ps.iter_mut().filter(|p| p.sketch.id == id) {
                    p.hidden = hidden;
                }
            });
            store.sketch_sel.set(None);
            store.status.set(if hidden { "Hidden: it's under Hidden if you want it back".into() } else { "Back in the list".into() });
        }
        Err(e) => store.status.set(format!("Couldn't hide it: {e}")),
    }
}

/// Plays a sketch as it was played: the recording, if it was heard
/// through a microphone.
pub fn hear(store: Store, source: Source) {
    if let Some((a, from, to)) = recording_of(store, source) {
        let samples = store.journal.and_then(|j| j.sketch_audio(a, from - 0.2, to + 1.5));
        match (store.engine, samples) {
            (Some(e), Some(samples)) => {
                e.play_clip(samples, compypal_audio::analysis::RATE);
                store.status.set("Playing the recording".into());
            }
            (None, _) => store.status.set("No audio output".into()),
            (_, None) => store.status.set("The recording isn't there any more".into()),
        }
        return;
    }
    hear_notes(store, source);
}

/// Plays a sketch's notes: for a heard sketch, what was written down.
pub fn hear_notes(store: Store, source: Source) {
    let events = events_of(store, source);
    match store.engine {
        Some(e) if !events.is_empty() => {
            e.audition(compypal_audio::schedule::from_raw(&events, 15, 0));
            store.status.set("Playing the sketch as you played it".into());
        }
        _ => store.status.set("Nothing to play".into()),
    }
}

/// Adds a sketch to the song: onto `track` (or a new one), at the end of
/// the song or at the cursor's bar.
pub fn add(store: Store, source: Source) {
    let track = store.sketch_track.get().and_then(|id| store.project.with(|p| p.track(id).map(|t| t.name.clone())));
    let bar = target_bar(store);
    let r = match source {
        Source::Journal(id) => agent::run_tool(store, "add_sketch", serde_json::json!({"sketch": id, "track": track, "at_bar": bar})),
        Source::Session(id) => store.add_session(id, store.sketch_track.get(), bar),
    };
    match r {
        Ok(msg) => {
            store.status.set(msg);
            store.sketch_sel.set(None);
        }
        Err(e) => store.status.set(e),
    }
}

/// The bar a sketch would land on: the end of the song, or the cursor's.
pub fn target_bar(store: Store) -> u32 {
    // Signals first: reading one inside `project.with` panics.
    let (at_cursor, cursor) = (store.sketch_at_cursor.get(), store.cursor.get());
    store.project.with(|p| {
        if at_cursor {
            text::bar_of(p, cursor)
        } else if p.tracks.iter().all(|t| t.clips.iter().all(|c| c.notes.is_empty())) {
            1
        } else {
            text::bar_of(p, p.end_tick().saturating_sub(1)) + 1
        }
    })
}

/// Shows where a song's sketch is used: its track, at its bars.
fn show_in_song(store: Store, session: Id) {
    let place = store.project.with(|p| {
        p.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.source_session == Some(session)).map(|c| (t.id, c.start)))
    });
    if let Some((track, start)) = place {
        store.select_track(track);
        store.seek(start);
        store.view.set(View::Edit);
    }
}

/// Opens the agent panel about this sketch.
fn ask(store: Store, source: Source, name: &str) {
    let about = match source {
        Source::Journal(id) => format!("About the sketch \"{name}\" (sketch {id}): "),
        Source::Session(_) => format!("About \"{name}\", a sketch in this song: "),
    };
    store.chat_draft.set(about);
    if !store.chat_open.get() {
        crate::chat::toggle(store);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    key: String,
    heading: Option<String>,
    item: Item,
}

#[component]
pub fn SketchesView() -> NodeHandle {
    let store = use_store::<Store>();
    let list = Memo::new(move || {
        let played = store.played.get();
        let now = compypal_audio::journal::now();
        let filter = store.sketch_filter.get();
        let items = store.project.with(|p| items(p, &played, now, filter));
        let mut last_day = String::new();
        items
            .into_iter()
            .map(|item| {
                let heading = (item.day != last_day).then(|| item.day.clone());
                last_day = item.day.clone();
                Row { key: format!("{}:{}", item.key, heading.is_some()), heading, item }
            })
            .collect::<Vec<_>>()
    });
    let shown = Memo::new(move || {
        let sel = store.sketch_sel.get().or_else(|| list.get().first().map(|r| r.item.source));
        let _ = store.project.with(|p| p.sessions.len());
        sel.and_then(|s| detail(store, s).map(|d| (s, d)))
    });
    rsx! {
        div { class: "sketches",
            div { class: "sketch-list",
                div { class: "sketch-filters",
                    for (f, label) in [(Filter::All, "All"), (Filter::InSong, "In this song"), (Filter::NotInSong, "Not in the song"), (Filter::Hidden, "Hidden")] {
                        span { key: label,
                            class: {move || if store.sketch_filter.get() == f { "sketch-filter on" } else { "sketch-filter" }},
                            onclick: move || {
                                store.sketch_filter.set(f);
                                store.sketch_sel.set(None);
                            },
                            {label}
                        }
                    }
                }
                if list.get().is_empty() && store.sketch_filter.get() != Filter::All {
                    div { class: "empty", "Nothing here" }
                }
                if list.get().is_empty() && store.sketch_filter.get() == Filter::All {
                    div { class: "sketch-empty",
                        div { class: "chat-empty-title", "Nothing played yet" }
                        div { class: "chat-empty-body",
                            "Turn on Listen and play: whatever you play is kept and split into sketches here, \
                             one idea each. Recording with Rec makes one too."
                        }
                    }
                }
                for row in list.get() {
                    div { key: row.key.clone(),
                        if let Some(h) = row.heading.clone() {
                            div { class: "sidebar-heading", {h} }
                        }
                        div {
                            class: {
                                let source = row.item.source;
                                let live = row.item.live;
                                move || {
                                    let picked = shown.get().is_some_and(|(s, _)| s == source);
                                    match (picked, live) {
                                        (true, _) => "sketch-row picked",
                                        (false, true) => "sketch-row live",
                                        _ => "sketch-row",
                                    }
                                }
                            },
                            onclick: {
                                let source = row.item.source;
                                move || store.sketch_sel.set(Some(source))
                            },
                            div { class: "sketch-thumb",
                                for (i, (x, y)) in row.item.thumb.iter().copied().enumerate() {
                                    div { key: i, class: "sketch-dot", style: {format!("left: {:.1}%; top: {:.1}%;", x * 92.0, 8.0 + y * 80.0)} }
                                }
                            }
                            div { class: "sketch-text",
                                div { class: "sketch-title",
                                    if row.item.audio {
                                        span { class: "sketch-audio", "mic" }
                                    }
                                    span { class: "sketch-name", {row.item.name.clone()} }
                                    span { class: "sketch-length", {row.item.length.clone()} }
                                }
                                div { class: "track-detail", {row.item.detail.clone()} }
                                div { class: {row.item.tag_class}, {row.item.tag.clone()} }
                            }
                        }
                    }
                }
            }
            div { class: "sketch-detail",
                // Keyed so a different sketch (or a changed one) is drawn
                // afresh: an `if let` that stays `Some` keeps its old body.
                for (key, source, d) in shown.get().into_iter().map(|(s, d)| (format!("{s:?}:{}:{}:{}", d.name, d.sub, d.notes.len()), s, d)) {
                  div { key: key, class: "sketch-detail-body",
                    div { class: "sketch-head",
                        span { class: "sketch-big", {d.name.clone()} }
                        span { class: "sketch-sub", {d.sub.clone()} }
                    }
                    div { class: "sketch-chips",
                        for c in d.chips.clone() {
                            span { key: c.clone(), class: "sketch-chip", {c.clone()} }
                        }
                    }
                    div { class: "sketch-roll",
                        for (i, (x, w, y)) in d.notes.iter().copied().enumerate() {
                            div { key: i, class: "sketch-note", style: {format!("left: {x:.2}%; width: {w:.2}%; top: {y:.1}%;")} }
                        }
                        div { class: "sketch-chords",
                            for (i, (x, w, name)) in d.chords.clone().into_iter().enumerate() {
                                div { key: i, class: "sketch-chord", style: {format!("left: {x:.2}%; width: {w:.2}%;")}, {name} }
                            }
                        }
                    }
                    div { class: "sketch-actions",
                        Button { size: "sm", variant: "default", onclick: move || hear(store, source), "▶ Hear it" }
                        if d.audio && !d.pending {
                            Button { size: "sm", variant: "subtle", onclick: move || hear_notes(store, source), "Hear the notes" }
                        }
                        Button { size: "sm", variant: "default",
                            onclick: {
                                let name = d.name.clone();
                                move || ask(store, source, &name)
                            },
                            "Ask the agent"
                        }
                        div { class: "spacer" }
                        match (d.in_song.clone(), source) {
                            _ if d.pending => div { class: "sketch-place",
                                span { class: "sketch-tag", "The notes come when it's written down" }
                            },
                            (Some(place), Source::Session(id)) => div { class: "sketch-place",
                                span { class: "sketch-tag used", {format!("In the song: {place}")} }
                                if place.contains("muted") {
                                    Button { size: "sm", variant: "light", onclick: move || store.use_session(id), "Use this one" }
                                }
                                Button { size: "sm", variant: "subtle", onclick: move || store.take_out_session(id), "Take out" }
                                Button { size: "sm", variant: "light", onclick: move || store.tidy_session(id), "Tidy" }
                                Button { size: "sm", variant: "filled", onclick: move || show_in_song(store, id), "Show in song" }
                            },
                            (None, Source::Journal(id)) if d.hidden => div { class: "sketch-place",
                                span { class: "sketch-tag", "Hidden" }
                                Button { size: "sm", variant: "light", onclick: move || set_hidden(store, id, false), "Unhide" }
                            },
                            _ => div { class: "sketch-place",
                                match source {
                                    Source::Journal(id) => Button { size: "sm", variant: "subtle",
                                        onclick: move || set_hidden(store, id, true), "Hide"
                                    },
                                    Source::Session(id) => Button { size: "sm", variant: "subtle",
                                        onclick: move || store.remove_session(id), "Remove"
                                    },
                                }
                                span { class: "toolbar-label", "to" }
                                div { class: "agent-pick-anchor",
                                    Button { size: "sm", variant: "default",
                                        onclick: move || store.sketch_menu.update(|o| *o = !*o),
                                        {|| match store.sketch_track.get().and_then(|id| store.project.with(|p| p.track(id).map(|t| t.name.clone()))) {
                                            Some(name) => format!("{name} ▾"),
                                            None => "a new track ▾".into(),
                                        }}
                                    }
                                    if store.sketch_menu.get() {
                                        TrackMenu {}
                                    }
                                }
                                span { class: "toolbar-label", "at" }
                                Button { size: "sm", variant: "default",
                                    onclick: move || store.sketch_at_cursor.update(|c| *c = !*c),
                                    {|| if store.sketch_at_cursor.get() {
                                        format!("bar {} (the cursor) ▾", target_bar(store))
                                    } else {
                                        format!("bar {} (the end) ▾", target_bar(store))
                                    }}
                                }
                                Button { size: "sm", variant: "filled", onclick: move || add(store, source), "Add to song" }
                            },
                        }
                    }
                  }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TrackRow {
    id: Option<Id>,
    name: String,
}

/// Picks the track a sketch goes onto.
#[component]
fn TrackMenu() -> NodeHandle {
    let store = use_store::<Store>();
    let mut rows: Vec<TrackRow> = store.project.with(|p| p.tracks.iter().map(|t| TrackRow { id: Some(t.id), name: t.name.clone() }).collect());
    rows.push(TrackRow { id: None, name: "A new track".into() });
    let root = rsx! {
        div { class: "agent-menu",
            for row in rows.clone() {
                div { key: row.name.clone(),
                    class: {
                        let id = row.id;
                        move || if store.sketch_track.get() == id { "agent-row current" } else { "agent-row" }
                    },
                    onclick: {
                        let id = row.id;
                        move || {
                            store.sketch_track.set(id);
                            store.sketch_menu.set(false);
                        }
                    },
                    div { class: "track-name", {row.name.clone()} }
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
                store.sketch_menu.set(false);
                return true;
            }
            false
        }),
        Rc::new(move || store.sketch_menu.set(false)),
    );
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_recorded_while_listening_is_listed_once() {
        let mut p = Project::new("Song");
        let ev: Vec<RawEvent> = (0..16)
            .flat_map(|i| {
                let t = i as f64 * 0.3;
                [
                    RawEvent { t, channel: 0, msg: RawMsg::NoteOn { pitch: 60 + (i % 4) as u8 * 4, velocity: 90 } },
                    RawEvent { t: t + 0.2, channel: 0, msg: RawMsg::NoteOff { pitch: 60 + (i % 4) as u8 * 4 } },
                ]
            })
            .collect();
        let at = 1_800_000_000.0;
        let heard: Vec<RawEvent> = ev.iter().map(|e| RawEvent { t: e.t + at, ..*e }).collect();
        let played = vec![Played { sketch: jam::summarize(&heard).unwrap(), live: false, hidden: false, thumb: thumb(&heard) }];
        assert_eq!(items(&p, &played, at + 60.0, Filter::All).len(), 1);
        let id = p.alloc_id();
        p.sessions.push(Session {
            id,
            name: "Take 1".into(),
            recorded_at: at as u64,
            click_bpm: Some(100.0),
            meter: MeterChange::default_four(),
            downbeat_offset: 0.0,
            start_tick: 0,
            own_tempo: false,
            events: ev.clone(),
            audio: None,
            // An older song: no `played_at`, `recorded_at` stands in.
            played_at: None,
        });
        let list = items(&p, &played, at + 60.0, Filter::All);
        assert_eq!(list.len(), 1, "{list:?}");
        // Not on any track yet: it's what "Not in the song" shows.
        assert_eq!(items(&p, &played, at + 60.0, Filter::NotInSong).len(), 1);
        let mut gone = played.clone();
        gone[0].hidden = true;
        assert_eq!(items(&Project::new("Empty"), &gone, at + 60.0, Filter::All).len(), 0, "hidden is out of sight");
        assert_eq!(items(&Project::new("Empty"), &gone, at + 60.0, Filter::Hidden).len(), 1);
        assert_eq!(list[0].source, Source::Session(id));
        assert_ne!(list[0].name, "Take 1", "named by its music");
        assert_eq!(items(&p, &played, at + 60.0, Filter::InSong).len(), 1);
    }

    #[test]
    fn a_long_recording_split_into_sketches_is_one_song_sketch() {
        // A Rec of 20 s with a pause in the middle: the library splits it
        // in two, and both halves are the song's.
        let mut p = Project::new("Song");
        let half = |from: f64| -> Vec<RawEvent> {
            (0..16)
                .flat_map(|i| {
                    let t = from + i as f64 * 0.3;
                    [
                        RawEvent { t, channel: 0, msg: RawMsg::NoteOn { pitch: 60 + (i % 4) as u8 * 4, velocity: 90 } },
                        RawEvent { t: t + 0.2, channel: 0, msg: RawMsg::NoteOff { pitch: 60 + (i % 4) as u8 * 4 } },
                    ]
                })
                .collect()
        };
        let ev: Vec<RawEvent> = half(0.0).into_iter().chain(half(12.0)).collect();
        let at = 1_800_000_000.0;
        let sketch = |from: f64| {
            let heard: Vec<RawEvent> = half(from).iter().map(|e| RawEvent { t: e.t + at, ..*e }).collect();
            Played { sketch: jam::summarize(&heard).unwrap(), live: false, hidden: false, thumb: thumb(&heard) }
        };
        let played = vec![sketch(12.0), sketch(0.0)];
        let id = p.alloc_id();
        p.sessions.push(Session {
            id,
            name: "Recording 1".into(),
            recorded_at: (at + 20.0) as u64,
            click_bpm: Some(100.0),
            meter: MeterChange::default_four(),
            downbeat_offset: 0.0,
            start_tick: 0,
            own_tempo: false,
            events: ev,
            audio: None,
            played_at: Some(at),
        });
        let list = items(&p, &played, at + 60.0, Filter::All);
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].source, Source::Session(id));
    }
}
