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

    /// All notes on the track in absolute ticks, sorted by start.
    pub fn absolute_notes(&self) -> Vec<Note> {
        let mut notes: Vec<Note> = self.clips.iter().flat_map(|c| c.absolute_notes()).collect();
        notes.sort_by_key(|n| (n.start, n.pitch));
        notes
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
}

impl Clip {
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
        };
        let notes: Vec<_> = clip.absolute_notes().collect();
        assert_eq!(notes, vec![Note { pitch: 60, velocity: 90, start: 140, duration: 10 }]);
    }
}
