//! A small project with a deliberately sloppy take, for the UI to show
//! before any controller is plugged in, and for tests to exercise cleanup.

use crate::cleanup::{self, Quantize};
use crate::model::{Clip, Note, PPQ, Project, Section, Tick};
use crate::session::{RawEvent, RawMsg, Session};

/// Deterministic jitter, so the demo looks the same every run.
struct Lcg(u64);

impl Lcg {
    /// Uniform in [-1, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Four bars of I-vi-IV-V eighth-note arpeggios at 100 BPM, played a little
/// late and unevenly, with two grazed neighbour keys and one double strike.
pub fn sloppy_take(id: crate::model::Id) -> Session {
    let bpm = 100.0;
    let eighth = 60.0 / bpm / 2.0;
    let downbeat = 4.0 * 60.0 / bpm; // one bar of count-in
    let chords: [[u8; 4]; 4] = [[60, 64, 67, 72], [57, 60, 64, 69], [53, 57, 60, 65], [55, 59, 62, 67]];
    let mut rng = Lcg(7);
    let mut events = Vec::new();
    let mut on = |t: f64, pitch: u8, velocity: u8, len: f64| {
        events.push(RawEvent { t, channel: 0, msg: RawMsg::NoteOn { pitch, velocity } });
        events.push(RawEvent { t: t + len, channel: 0, msg: RawMsg::NoteOff { pitch } });
    };
    for (bar, chord) in chords.iter().enumerate() {
        for step in 0..8 {
            let pitch = chord[[0, 1, 2, 3, 2, 1, 2, 1][step]];
            let t = downbeat + (bar * 8 + step) as f64 * eighth + 0.012 + rng.next() * 0.025;
            let vel = (84.0 + rng.next() * 14.0 + if step % 2 == 0 { 8.0 } else { 0.0 }) as u8;
            on(t, pitch, vel, eighth * (0.8 + rng.next() * 0.15));
        }
    }
    // Mistakes: two grazed keys and a double strike.
    on(downbeat + 3.0 * eighth + 0.03, 61, 22, 0.03);
    on(downbeat + 19.0 * eighth + 0.01, 66, 18, 0.04);
    on(downbeat + 24.0 * eighth + 0.04, 55, 70, 0.2);

    events.sort_by(|a, b| a.t.total_cmp(&b.t));
    Session {
        id,
        name: "Arpeggio take 1".into(),
        recorded_at: 0,
        click_bpm: Some(bpm),
        meter: crate::model::MeterChange { tick: 0, numerator: 4, denominator: 4 },
        downbeat_offset: downbeat,
        start_tick: 0,
        own_tempo: false,
        events,
    }
}

pub fn project() -> Project {
    let mut p = Project::new("Demo");
    p.tempo.set(0, 100.0);
    let bar = 4 * PPQ as Tick;

    let session_id = p.alloc_id();
    let take = sloppy_take(session_id);

    let mut notes = cleanup::import_session(&take, &p.tempo, false).notes;
    cleanup::remove_quiet(&mut notes, 30);
    cleanup::merge_double_strikes(&mut notes, PPQ as Tick / 8);
    cleanup::quantize(&mut notes, &Quantize { strength: 0.9, ..Quantize::new(PPQ as Tick / 2) });
    cleanup::fix_overlaps(&mut notes);
    p.sessions.push(take);

    let keys = p.add_track("Keys", 0);
    let clip_id = p.alloc_id();
    p.track_mut(keys).unwrap().clips.push(Clip {
        id: clip_id,
        name: "Arpeggio".into(),
        start: 0,
        length: 4 * bar,
        notes,
        source_session: Some(session_id),
    });

    let bass = p.add_track("Bass", 33);
    let roots = [36u8, 33, 29, 31];
    let bass_notes = roots
        .iter()
        .enumerate()
        .flat_map(|(i, &r)| {
            let s = i as Tick * bar;
            [
                Note { pitch: r, velocity: 100, start: s, duration: PPQ as Tick * 3 / 2 },
                Note { pitch: r, velocity: 85, start: s + PPQ as Tick * 3 / 2, duration: PPQ as Tick / 2 },
                Note { pitch: r + 12, velocity: 90, start: s + PPQ as Tick * 2, duration: PPQ as Tick * 2 },
            ]
        })
        .collect();
    let clip_id = p.alloc_id();
    p.track_mut(bass).unwrap().clips.push(Clip {
        id: clip_id,
        name: "Roots".into(),
        start: 0,
        length: 4 * bar,
        notes: bass_notes,
        source_session: None,
    });

    let drums = p.add_drum_track("Drums");
    let beat = PPQ as Tick;
    let mut drum_notes = Vec::new();
    for b in 0..16 {
        let s = b * beat;
        drum_notes.push(Note { pitch: 42, velocity: 70, start: s, duration: beat / 4 });
        drum_notes.push(Note { pitch: 42, velocity: 55, start: s + beat / 2, duration: beat / 4 });
        let kick_or_snare = if b % 2 == 0 { 36 } else { 38 };
        drum_notes.push(Note { pitch: kick_or_snare, velocity: 100, start: s, duration: beat / 4 });
    }
    let clip_id = p.alloc_id();
    p.track_mut(drums).unwrap().clips.push(Clip {
        id: clip_id,
        name: "Beat".into(),
        start: 0,
        length: 4 * bar,
        notes: drum_notes,
        source_session: None,
    });

    let id = p.alloc_id();
    p.sections.push(Section { id, name: "Intro".into(), start: 0, length: 4 * bar });
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_removes_the_planted_mistakes() {
        let p = project();
        let keys = &p.tracks[0].clips[0].notes;
        assert_eq!(keys.len(), 32, "32 arpeggio notes survive, mistakes gone");
        assert!(keys.iter().all(|n| n.pitch != 61 && n.pitch != 66));
        let r = cleanup::timing_report(keys, PPQ as Tick / 2);
        assert!(r.mean_abs_deviation < 10.0, "{r:?}");
    }

    #[test]
    fn project_json_round_trips() {
        let p = project();
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(p, back);
    }
}
