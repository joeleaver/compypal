//! The project model. Everything an arrangement is made of lives here, with
//! time in ticks at [`PPQ`] ticks per quarter note. Raw recordings live in
//! [`crate::session`] and are measured in seconds instead.

use serde::{Deserialize, Serialize};

use crate::session::Session;

/// Ticks per quarter note. 960 divides evenly by 2, 3, 4, 5, 6, 8, 16 and 32,
/// so triplets, quintuplets and 1/128ths all land on whole ticks.
pub const PPQ: u32 = 960;

pub type Tick = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub u64);

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub tempo: TempoMap,
    pub meter: Vec<MeterChange>,
    /// Concert key, used for notation export and as a hint to the agent.
    pub key: KeySignature,
    pub tracks: Vec<Track>,
    /// Raw takes recorded from a controller. Never modified after recording;
    /// clips are derived from them.
    pub sessions: Vec<Session>,
    /// Arrangement markers: verse, chorus, and so on.
    pub sections: Vec<Section>,
    next_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tempo: TempoMap::constant(120.0),
            meter: vec![MeterChange { tick: 0, numerator: 4, denominator: 4 }],
            key: KeySignature::default(),
            tracks: Vec::new(),
            sessions: Vec::new(),
            sections: Vec::new(),
            next_id: 1,
        }
    }

    pub fn alloc_id(&mut self) -> Id {
        let id = Id(self.next_id);
        self.next_id += 1;
        id
    }

    /// Lays `clip` on `track` over whatever plays there, which is muted
    /// rather than lost (see [`Track::add_over`]). Returns how many clips
    /// it muted.
    pub fn add_clip_over(&mut self, track: Id, clip: Clip) -> usize {
        let next = &mut self.next_id;
        let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else { return 0 };
        t.add_over(clip, &mut || {
            *next += 1;
            Id(*next - 1)
        })
    }

    /// Takes a session's music out of the song: its clips go, and whatever
    /// they had muted is heard again. The session itself stays. Returns how
    /// many clips went.
    pub fn take_out(&mut self, session: Id) -> usize {
        let mut gone = 0;
        for t in &mut self.tracks {
            let spans: Vec<(Tick, Tick)> =
                t.clips.iter().filter(|c| c.source_session == Some(session)).map(|c| (c.start, c.start + c.length)).collect();
            let ids: Vec<Id> = t.clips.iter().filter(|c| c.source_session == Some(session)).map(|c| c.id).collect();
            gone += spans.len();
            t.clips.retain(|c| c.source_session != Some(session));
            // What these clips muted, exactly, is heard again.
            for c in t.clips.iter_mut().filter(|c| c.muted_by.is_some_and(|by| ids.contains(&by))) {
                c.muted = false;
                c.muted_by = None;
            }
            // What was under it comes back only where nothing else is heard,
            // the newest alternative first: never two takes at once.
            let mut under: Vec<usize> = (0..t.clips.len())
                .filter(|&i| {
                    let c = &t.clips[i];
                    c.muted && spans.iter().any(|(a, b)| c.start < *b && c.start + c.length > *a)
                })
                .collect();
            under.sort_by_key(|&i| std::cmp::Reverse(t.clips[i].id));
            for i in under {
                let (from, to) = (t.clips[i].start, t.clips[i].start + t.clips[i].length);
                let covered = t.clips.iter().any(|o| !o.muted && o.start < to && o.start + o.length > from && o.absolute_notes().next().is_some());
                if !covered {
                    t.clips[i].muted = false;
                }
            }
        }
        gone
    }

    /// Makes clip `clip` the one heard on its track over its span.
    pub fn prefer_clip(&mut self, clip: Id) -> usize {
        let next = &mut self.next_id;
        let Some(t) = self.tracks.iter_mut().find(|t| t.clips.iter().any(|c| c.id == clip)) else { return 0 };
        t.prefer(clip, &mut || {
            *next += 1;
            Id(*next - 1)
        })
    }

    pub fn add_track(&mut self, name: impl Into<String>, program: u8) -> Id {
        let id = self.alloc_id();
        let channel = self.next_free_channel();
        self.tracks.push(Track {
            id,
            name: name.into(),
            channel,
            program,
            volume: 100,
            pan: 64,
            muted: false,
            solo: false,
            clips: Vec::new(),
        });
        id
    }

    pub fn add_drum_track(&mut self, name: impl Into<String>) -> Id {
        let id = self.add_track(name, 0);
        self.track_mut(id).unwrap().channel = DRUM_CHANNEL;
        id
    }

    /// The lowest melodic channel no track is using, skipping the GM drum
    /// channel. Wraps to 0 once all fifteen are taken.
    fn next_free_channel(&self) -> u8 {
        (0..16u8)
            .filter(|&c| c != DRUM_CHANNEL)
            .find(|c| !self.tracks.iter().any(|t| t.channel == *c))
            .unwrap_or(0)
    }

    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn session(&self, id: Id) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// Finds a clip anywhere in the project, returning its track id with it.
    pub fn clip(&self, id: Id) -> Option<(Id, &Clip)> {
        self.tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t.id, c)))
    }

    pub fn clip_mut(&mut self, id: Id) -> Option<&mut Clip> {
        self.tracks.iter_mut().find_map(|t| t.clips.iter_mut().find(|c| c.id == id))
    }

    pub fn meter_at(&self, tick: Tick) -> MeterChange {
        self.meter
            .iter()
            .rev()
            .find(|m| m.tick <= tick)
            .copied()
            .unwrap_or(MeterChange { tick: 0, numerator: 4, denominator: 4 })
    }

    /// End of the last clip on any track.
    pub fn end_tick(&self) -> Tick {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.start + c.length)
            .max()
            .unwrap_or(0)
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

/// MIDI channel 10, zero-based.
pub const DRUM_CHANNEL: u8 = 9;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub name: String,
    /// Zero-based MIDI channel. Channel 9 is General MIDI percussion.
    pub channel: u8,
    /// General MIDI program number, 0-127.
    pub program: u8,
    pub volume: u8,
    pub pan: u8,
    pub muted: bool,
    pub solo: bool,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn is_drums(&self) -> bool {
        self.channel == DRUM_CHANNEL
    }

    /// The notes that are heard, in absolute ticks, sorted by start.
    /// Muted clips (the alternatives under something laid over them) are
    /// left out.
    pub fn absolute_notes(&self) -> Vec<Note> {
        let mut notes: Vec<Note> = self.clips.iter().filter(|c| !c.muted).flat_map(|c| c.absolute_notes()).collect();
        notes.sort_by_key(|n| (n.start, n.pitch));
        notes
    }

    /// The notes of muted clips: what was there before something was laid
    /// over it.
    pub fn muted_notes(&self) -> Vec<Note> {
        let mut notes: Vec<Note> = self.clips.iter().filter(|c| c.muted).flat_map(|c| c.absolute_notes()).collect();
        notes.sort_by_key(|n| (n.start, n.pitch));
        notes
    }

    /// Splits clips running across `at` in two, so that `at` is an edge.
    fn cut_at(&mut self, at: Tick, alloc: &mut dyn FnMut() -> Id) {
        let mut added = Vec::new();
        for c in &mut self.clips {
            if c.start < at && c.start + c.length > at {
                added.push(c.split_at(at, alloc()));
            }
        }
        self.clips.extend(added);
        self.clips.sort_by_key(|c| c.start);
    }

    /// Lays `clip` over whatever plays there. What it covers isn't lost:
    /// those clips are cut at its edges and the covered parts muted, kept
    /// as alternatives (see [`Track::prefer`]). Returns how many clips were
    /// muted.
    pub fn add_over(&mut self, clip: Clip, alloc: &mut dyn FnMut() -> Id) -> usize {
        let id = clip.id;
        self.clips.push(clip);
        self.clips.sort_by_key(|c| c.start);
        self.prefer(id, alloc)
    }

    /// Makes clip `id` the one heard over its span: it's unmuted, and
    /// every other clip there is cut at its edges and muted. Returns how
    /// many clips were muted.
    pub fn prefer(&mut self, id: Id, alloc: &mut dyn FnMut() -> Id) -> usize {
        let Some((from, to)) = self.clips.iter().find(|c| c.id == id).map(|c| (c.start, c.start + c.length)) else { return 0 };
        self.cut_at(from, alloc);
        self.cut_at(to, alloc);
        let mut muted = 0;
        for c in &mut self.clips {
            if c.id == id {
                c.muted = false;
                c.muted_by = None;
            } else if c.start < to && c.start + c.length > from && !c.muted && c.absolute_notes().next().is_some() {
                c.muted = true;
                c.muted_by = Some(id);
                muted += 1;
            }
        }
        muted
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub name: String,
    pub start: Tick,
    pub length: Tick,
    /// Note starts are relative to the clip start.
    pub notes: Vec<Note>,
    /// The session this clip was derived from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session: Option<Id>,
    /// How far into its session's notes this clip begins, once split:
    /// re-deriving it from the take refills only its own window.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_offset: Tick,
    /// Not heard: something else was laid over it. Kept as an alternative.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// The clip whose arrival muted this one: taking that out brings this
    /// back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_by: Option<Id>,
}

fn is_zero(t: &Tick) -> bool {
    *t == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Clip {
    /// Splits off the part from `at` (absolute) on, as a new clip `id`;
    /// this one keeps what's before. Notes sounding across `at` are cut
    /// there.
    pub fn split_at(&mut self, at: Tick, id: Id) -> Clip {
        let rel = at.saturating_sub(self.start).min(self.length);
        let (before, after): (Vec<Note>, Vec<Note>) = self.notes.iter().partition(|n| n.start < rel);
        let right = Clip {
            id,
            name: self.name.clone(),
            start: self.start + rel,
            length: self.length - rel,
            notes: after.into_iter().map(|n| Note { start: n.start - rel, ..n }).collect(),
            source_session: self.source_session,
            source_offset: self.source_offset + rel,
            muted: self.muted,
            muted_by: self.muted_by,
        };
        self.notes = before.into_iter().map(|n| Note { duration: n.duration.min(rel - n.start).max(1), ..n }).collect();
        self.length = rel;
        right
    }

    /// Refills the clip from its take's notes (as imported, relative to
    /// the take's own start), keeping to this clip's window.
    pub fn refill(&mut self, fresh: &[Note]) {
        let (from, to) = (self.source_offset, self.source_offset + self.length);
        self.notes = fresh.iter().filter(|n| n.start >= from && n.start < to).map(|n| Note { start: n.start - from, ..*n }).collect();
    }

    /// Notes in absolute ticks. Notes that start past the clip end are
    /// hidden; notes that run past it are cut at the boundary.
    pub fn absolute_notes(&self) -> impl Iterator<Item = Note> + '_ {
        self.notes.iter().filter(|n| n.start < self.length).map(|n| Note {
            start: self.start + n.start,
            duration: n.duration.min(self.length - n.start),
            ..*n
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Note {
    pub pitch: u8,
    pub velocity: u8,
    pub start: Tick,
    pub duration: Tick,
}

impl Note {
    pub fn end(&self) -> Tick {
        self.start + self.duration
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: Id,
    pub name: String,
    pub start: Tick,
    pub length: Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterChange {
    pub tick: Tick,
    pub numerator: u8,
    /// Power of two: 2, 4, 8, 16.
    pub denominator: u8,
}

impl MeterChange {
    pub fn ticks_per_bar(&self) -> Tick {
        self.numerator as Tick * self.ticks_per_beat()
    }

    pub fn ticks_per_beat(&self) -> Tick {
        PPQ as Tick * 4 / self.denominator as Tick
    }
}

/// A key as a count of sharps (positive) or flats (negative), plus mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySignature {
    pub fifths: i8,
    pub minor: bool,
}

impl KeySignature {
    pub fn name(&self) -> &'static str {
        const MAJOR: [&str; 15] =
            ["Cb", "Gb", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#"];
        const MINOR: [&str; 15] =
            ["Abm", "Ebm", "Bbm", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm", "F#m", "C#m", "G#m", "D#m", "A#m"];
        let i = (self.fifths.clamp(-7, 7) + 7) as usize;
        if self.minor { MINOR[i] } else { MAJOR[i] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoChange {
    pub tick: Tick,
    pub bpm: f64,
}

/// Piecewise-constant tempo. The first entry is always at tick 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoMap {
    changes: Vec<TempoChange>,
}

impl TempoMap {
    pub fn constant(bpm: f64) -> Self {
        Self { changes: vec![TempoChange { tick: 0, bpm }] }
    }

    pub fn changes(&self) -> &[TempoChange] {
        &self.changes
    }

    pub fn bpm_at(&self, tick: Tick) -> f64 {
        self.changes.iter().rev().find(|c| c.tick <= tick).map_or(120.0, |c| c.bpm)
    }

    /// Inserts or replaces the tempo at `tick`.
    pub fn set(&mut self, tick: Tick, bpm: f64) {
        match self.changes.iter_mut().find(|c| c.tick == tick) {
            Some(c) => c.bpm = bpm,
            None => {
                self.changes.push(TempoChange { tick, bpm });
                self.changes.sort_by_key(|c| c.tick);
            }
        }
    }

    /// Moves every change after `at` by `delta` ticks (never the first).
    pub fn shift_from(&mut self, at: Tick, delta: i64) {
        for c in self.changes.iter_mut().skip(1).filter(|c| c.tick >= at) {
            c.tick = c.tick.saturating_add_signed(delta);
        }
    }

    /// Drops changes in `from..to` and closes the gap. The tempo in force
    /// at `to` carries on from `from`.
    pub fn remove_range(&mut self, from: Tick, to: Tick) {
        let carried = self.bpm_at(to);
        let had_change_inside = self.changes.iter().any(|c| c.tick > from && c.tick <= to);
        self.changes.retain(|c| c.tick == 0 || c.tick < from || c.tick > to);
        for c in self.changes.iter_mut().skip(1).filter(|c| c.tick > to) {
            c.tick -= to - from;
        }
        if had_change_inside && self.bpm_at(from) != carried {
            self.set(from, carried);
        }
    }

    pub fn tick_to_seconds(&self, tick: f64) -> f64 {
        let mut secs = 0.0;
        for (i, c) in self.changes.iter().enumerate() {
            let seg_end = self.changes.get(i + 1).map_or(f64::INFINITY, |n| n.tick as f64);
            let seg_start = c.tick as f64;
            if tick <= seg_start {
                break;
            }
            secs += (tick.min(seg_end) - seg_start) * secs_per_tick(c.bpm);
        }
        secs
    }

    pub fn seconds_to_tick(&self, secs: f64) -> f64 {
        let mut elapsed = 0.0;
        for (i, c) in self.changes.iter().enumerate() {
            let spt = secs_per_tick(c.bpm);
            let Some(next) = self.changes.get(i + 1) else {
                return c.tick as f64 + (secs - elapsed) / spt;
            };
            let seg_secs = (next.tick - c.tick) as f64 * spt;
            if secs < elapsed + seg_secs {
                return c.tick as f64 + (secs - elapsed) / spt;
            }
            elapsed += seg_secs;
        }
        unreachable!("tempo map always has an entry at tick 0")
    }
}

fn secs_per_tick(bpm: f64) -> f64 {
    60.0 / bpm / PPQ as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_round_trip_across_changes() {
        let mut map = TempoMap::constant(120.0);
        map.set(PPQ as Tick * 4, 60.0);
        // One bar at 120 is two seconds; a beat at 60 is one more.
        assert!((map.tick_to_seconds(PPQ as f64 * 5.0) - 3.0).abs() < 1e-9);
        assert!((map.seconds_to_tick(3.0) - PPQ as f64 * 5.0).abs() < 1e-6);
        assert!((map.seconds_to_tick(1.0) - PPQ as f64 * 2.0).abs() < 1e-6);
    }

    #[test]
    fn channels_skip_drums() {
        let mut p = Project::new("t");
        for i in 0..10 {
            p.add_track(format!("t{i}"), 0);
        }
        assert!(p.tracks.iter().all(|t| t.channel != DRUM_CHANNEL));
        assert_eq!(p.tracks[9].channel, 10);
    }

    #[test]
    fn clip_truncates_notes_at_boundary() {
        let clip = Clip {
            id: Id(1),
            name: String::new(),
            start: 100,
            length: 50,
            notes: vec![
                Note { pitch: 60, velocity: 90, start: 40, duration: 30 },
                Note { pitch: 62, velocity: 90, start: 60, duration: 10 },
            ],
            source_session: None,
            ..Default::default()
        };
        let notes: Vec<_> = clip.absolute_notes().collect();
        assert_eq!(notes, vec![Note { pitch: 60, velocity: 90, start: 140, duration: 10 }]);
    }

    fn clip(id: u64, start: Tick, length: Tick, every: Tick) -> Clip {
        let notes = (0..length / every).map(|i| Note { pitch: 60, velocity: 90, start: i * every, duration: every / 2 }).collect();
        Clip { id: Id(id), name: format!("c{id}"), start, length, notes, ..Default::default() }
    }

    #[test]
    fn laying_over_music_mutes_only_what_it_covers() {
        let mut p = Project::new("t");
        let t = p.add_track("Keys", 0);
        let q = PPQ as Tick;
        // Bars 1-4 played; a new take over bars 3-4.
        let old = clip(100, 0, 16 * q, q);
        p.track_mut(t).unwrap().clips.push(old);
        let muted = p.add_clip_over(t, clip(200, 8 * q, 8 * q, q / 2));
        assert_eq!(muted, 1);
        let track = p.track(t).unwrap();
        let heard = track.absolute_notes();
        // Bars 1-2 of the old part, then the new one: nothing doubled.
        assert_eq!(heard.iter().filter(|n| n.start < 8 * q).count(), 8);
        assert_eq!(heard.iter().filter(|n| n.start >= 8 * q).count(), 16);
        assert_eq!(track.muted_notes().len(), 8, "the covered half of the old part is kept, muted");
        // The old half keeps its place in the take, for re-deriving.
        let half = track.clips.iter().find(|c| c.muted).unwrap();
        assert_eq!((half.start, half.source_offset), (8 * q, 8 * q));

        // Changing your mind: the old part again.
        let id = half.id;
        p.prefer_clip(id);
        let track = p.track(t).unwrap();
        assert_eq!(track.absolute_notes().len(), 16);
        assert_eq!(track.muted_notes().len(), 16);
    }

    #[test]
    fn taking_a_sketch_out_brings_back_what_it_covered() {
        let mut p = Project::new("t");
        let t = p.add_track("Keys", 0);
        let q = PPQ as Tick;
        p.track_mut(t).unwrap().clips.push(clip(100, 0, 16 * q, q));
        let over = Clip { source_session: Some(Id(7)), ..clip(200, 8 * q, 8 * q, q / 2) };
        p.add_clip_over(t, over);
        assert_eq!(p.take_out(Id(7)), 1);
        let track = p.track(t).unwrap();
        assert_eq!(track.absolute_notes().len(), 16, "all of the old part is heard again");
        assert!(track.muted_notes().is_empty());
    }

    #[test]
    fn taking_out_never_leaves_two_takes_heard() {
        // Three takes of bars 3-4: A (oldest), B over it, then A preferred
        // again; then C over both. Taking C out brings back A only.
        let mut p = Project::new("t");
        let t = p.add_track("Keys", 0);
        let q = PPQ as Tick;
        p.track_mut(t).unwrap().clips.push(Clip { source_session: Some(Id(1)), ..clip(100, 8 * q, 8 * q, q) });
        p.add_clip_over(t, Clip { source_session: Some(Id(2)), ..clip(200, 8 * q, 8 * q, q) });
        p.prefer_clip(Id(100));
        p.add_clip_over(t, Clip { source_session: Some(Id(3)), ..clip(300, 8 * q, 8 * q, q) });
        p.take_out(Id(3));
        let heard: Vec<&Clip> = p.track(t).unwrap().clips.iter().filter(|c| !c.muted).collect();
        assert_eq!(heard.len(), 1, "{heard:?}");
        assert_eq!(heard[0].id, Id(100), "what was heard before C");
        assert_eq!(p.track(t).unwrap().absolute_notes().len(), 8);
    }

    #[test]
    fn a_split_clip_refills_only_its_window() {
        let q = PPQ as Tick;
        let mut c = clip(1, 0, 8 * q, q);
        let mut right = c.split_at(4 * q, Id(2));
        let fresh: Vec<Note> = (0..8).map(|i| Note { pitch: 64, velocity: 80, start: i * q, duration: q }).collect();
        c.refill(&fresh);
        right.refill(&fresh);
        assert_eq!(c.notes.iter().map(|n| n.start).collect::<Vec<_>>(), [0, q, 2 * q, 3 * q]);
        assert_eq!(right.notes.iter().map(|n| n.start).collect::<Vec<_>>(), [0, q, 2 * q, 3 * q]);
        assert!(right.notes.iter().all(|n| n.pitch == 64));
    }
}
