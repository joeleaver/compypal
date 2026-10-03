//! Editing time itself: inserting, deleting and duplicating bars across the
//! whole project at once. Everything positioned after the edit point moves
//! together: notes, clips, sections, tempo and meter changes, and the
//! anchors of recorded sessions, so raw takes stay lined up with their
//! clips.

use crate::model::{Clip, Note, Project, Section, Tick};

/// Opens `len` ticks of silence at `at`. A clip running across `at` is
/// stretched, with its later notes moved along.
pub fn insert_time(p: &mut Project, at: Tick, len: Tick) {
    if len == 0 {
        return;
    }
    for t in &mut p.tracks {
        for c in &mut t.clips {
            if c.start >= at {
                c.start += len;
            } else if c.start + c.length > at {
                let rel = at - c.start;
                for n in c.notes.iter_mut().filter(|n| n.start >= rel) {
                    n.start += len;
                }
                c.length += len;
            }
        }
    }
    for s in &mut p.sections {
        if s.start >= at {
            s.start += len;
        } else if s.start + s.length > at {
            s.length += len;
        }
    }
    p.tempo.shift_from(at, len as i64);
    for m in p.meter.iter_mut().filter(|m| m.tick >= at && m.tick > 0) {
        m.tick += len;
    }
    for s in p.sessions.iter_mut().filter(|s| s.start_tick >= at) {
        s.start_tick += len;
    }
}

/// Removes ticks `at..at+len`: notes starting there go, notes sounding into
/// it are cut at `at`, and everything after moves back.
pub fn delete_time(p: &mut Project, at: Tick, len: Tick) {
    if len == 0 {
        return;
    }
    let end = at + len;
    let moved = |t: Tick| if t >= end { t - len } else { t.min(at) };
    for t in &mut p.tracks {
        for c in &mut t.clips {
            let notes: Vec<Note> = c.absolute_notes().collect();
            let kept: Vec<Note> = notes
                .into_iter()
                .filter(|n| n.start < at || n.start >= end)
                .map(|n| {
                    let start = moved(n.start);
                    // A note sounding into the cut ends where the cut starts.
                    let stop = if n.start < at { n.end().min(at).max(start + 1) } else { moved(n.end()) };
                    Note { start, duration: stop - start, ..n }
                })
                .collect();
            let (cs, ce) = (moved(c.start), moved(c.start + c.length));
            c.start = cs;
            c.length = ce.saturating_sub(cs);
            c.notes = kept.into_iter().map(|n| Note { start: n.start - cs, ..n }).collect();
        }
        // An emptied clip goes, unless it still points at a recorded take.
        t.clips.retain(|c| c.length > 0 && (!c.notes.is_empty() || c.source_session.is_some()));
    }
    for s in &mut p.sections {
        let (ss, se) = (moved(s.start), moved(s.start + s.length));
        s.start = ss;
        s.length = se.saturating_sub(ss);
    }
    p.sections.retain(|s| s.length > 0);
    p.tempo.remove_range(at, end);
    p.meter.retain(|m| m.tick == 0 || m.tick < at || m.tick >= end);
    for m in p.meter.iter_mut().filter(|m| m.tick >= end) {
        m.tick -= len;
    }
    for s in &mut p.sessions {
        s.start_tick = moved(s.start_tick);
    }
}

/// Plays `from..from+len` twice: opens room right after it and copies
/// every track's notes, and any section that lies within, into the gap.
pub fn duplicate_time(p: &mut Project, from: Tick, len: Tick) {
    if len == 0 {
        return;
    }
    let to = from + len;
    let copies: Vec<(usize, Vec<Note>)> = p
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let notes = t
                .absolute_notes()
                .into_iter()
                .filter(|n| n.start >= from && n.start < to)
                .map(|n| Note { start: n.start + len, duration: n.duration.min(to - n.start), ..n })
                .collect();
            (i, notes)
        })
        .collect();
    let sections: Vec<Section> =
        p.sections.iter().filter(|s| s.start >= from && s.start + s.length <= to).cloned().collect();
    insert_time(p, to, len);
    for (i, notes) in copies {
        if notes.is_empty() {
            continue;
        }
        let id = p.tracks[i].id;
        let clip_id = p.alloc_id();
        let t = p.track_mut(id).unwrap();
        // Into the clip that now spans the gap, if one was stretched over it;
        // otherwise a clip of its own.
        match t.clips.iter_mut().find(|c| c.start <= to && c.start + c.length >= to + len) {
            Some(c) => {
                let cs = c.start;
                c.notes.extend(notes.iter().map(|n| Note { start: n.start - cs, ..*n }));
                c.notes.sort_by_key(|n| (n.start, n.pitch));
            }
            None => {
                t.clips.push(Clip {
                    id: clip_id,
                    name: "Copy".into(),
                    start: to,
                    length: len,
                    notes: notes.iter().map(|n| Note { start: n.start - to, ..*n }).collect(),
                    source_session: None,
                });
                t.clips.sort_by_key(|c| c.start);
            }
        }
    }
    for s in sections {
        let id = p.alloc_id();
        p.sections.push(Section { id, start: s.start + len, ..s });
    }
    p.sections.sort_by_key(|s| s.start);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use crate::model::PPQ;

    const BAR: Tick = 4 * PPQ as Tick;

    fn starts(p: &Project, track: usize) -> Vec<Tick> {
        p.tracks[track].absolute_notes().iter().map(|n| n.start).collect()
    }

    #[test]
    fn insert_opens_a_gap_everywhere() {
        let mut p = demo::project();
        p.tempo.set(2 * BAR, 90.0);
        let bass_before = starts(&p, 1);
        insert_time(&mut p, BAR, 2 * BAR);
        let bass_after = starts(&p, 1);
        assert_eq!(bass_after.len(), bass_before.len());
        for (a, b) in bass_before.iter().zip(&bass_after) {
            assert_eq!(*b, if *a >= BAR { a + 2 * BAR } else { *a });
        }
        assert_eq!(p.end_tick(), 6 * BAR);
        assert_eq!(p.sections[0].length, 6 * BAR);
        assert_eq!(p.tempo.bpm_at(4 * BAR), 90.0);
        assert_eq!(p.tempo.bpm_at(3 * BAR), 100.0);
    }

    #[test]
    fn delete_closes_it_again() {
        let original = demo::project();
        let mut p = original.clone();
        insert_time(&mut p, BAR, 2 * BAR);
        delete_time(&mut p, BAR, 2 * BAR);
        for i in 0..3 {
            assert_eq!(starts(&p, i), starts(&original, i), "track {i}");
        }
        assert_eq!(p.sections, original.sections);
    }

    #[test]
    fn delete_removes_and_trims() {
        let mut p = demo::project();
        // Bass: three notes per bar; delete bar 2.
        delete_time(&mut p, BAR, BAR);
        assert_eq!(starts(&p, 1).len(), 9);
        assert_eq!(p.end_tick(), 3 * BAR);
        // A note held across the cut is shortened to it.
        let mut q = Project::new("t");
        let t = q.add_track("x", 0);
        q.replace_notes(t, &[], &[Note { pitch: 60, velocity: 90, start: 0, duration: 2 * BAR }]);
        delete_time(&mut q, BAR, BAR);
        assert_eq!(q.tracks[0].absolute_notes()[0].duration, BAR);
    }

    #[test]
    fn duplicate_repeats_a_passage_and_its_sections() {
        let mut p = demo::project();
        let keys_before = starts(&p, 0);
        duplicate_time(&mut p, 0, 4 * BAR);
        assert_eq!(p.end_tick(), 8 * BAR);
        let keys = starts(&p, 0);
        assert_eq!(keys.len(), 64);
        assert_eq!(&keys[32..], &keys_before.iter().map(|s| s + 4 * BAR).collect::<Vec<_>>()[..]);
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[1].start, 4 * BAR);
    }

    #[test]
    fn duplicate_inside_a_clip() {
        let mut p = demo::project();
        duplicate_time(&mut p, BAR, BAR);
        // Bar 2 now plays twice, then bars 3 and 4 follow, all one clip each.
        assert_eq!(p.end_tick(), 5 * BAR);
        assert_eq!(p.tracks[1].clips.len(), 1);
        let bass = p.tracks[1].absolute_notes();
        let bar2: Vec<_> = bass.iter().filter(|n| n.start >= BAR && n.start < 2 * BAR).map(|n| n.pitch).collect();
        let bar3: Vec<_> = bass.iter().filter(|n| n.start >= 2 * BAR && n.start < 3 * BAR).map(|n| n.pitch).collect();
        assert_eq!(bar2, bar3);
    }
}
