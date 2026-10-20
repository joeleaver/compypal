//! Editing time itself: inserting, deleting and duplicating bars across the
//! whole project at once. Everything positioned after the edit point moves
//! together: notes, clips, sections, tempo and meter changes, and the
//! anchors of recorded sessions, so raw takes stay lined up with their
//! clips.

use crate::model::{Bend, Clip, Note, Project, Section, Tick, bend_at, bend_window};

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
                // The gap is unbent; the bend picks up again after it.
                let held = bend_at(&c.bend, rel);
                for b in c.bend.iter_mut().filter(|b| b.tick >= rel) {
                    b.tick += len;
                }
                if held != 0 {
                    c.bend.push(Bend { tick: rel, value: 0 });
                    c.bend.push(Bend { tick: rel + len, value: held });
                    c.bend.sort_by_key(|b| b.tick);
                    c.bend.dedup_by(|later, earlier| later.tick == earlier.tick && {
                        earlier.value = later.value;
                        true
                    });
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
            // Bend: what was in effect at the end of the cut holds from its
            // start; the points inside it go.
            let abs: Vec<Bend> = c.bend.iter().map(|b| Bend { tick: c.start + b.tick, ..*b }).collect();
            let mut bend: Vec<Bend> = abs.iter().filter(|b| b.tick < at).copied().collect();
            if abs.iter().any(|b| b.tick >= at && b.tick < end) {
                bend.push(Bend { tick: at, value: bend_at(&abs, end) });
            }
            bend.extend(abs.iter().filter(|b| b.tick >= end).map(|b| Bend { tick: b.tick - len, ..*b }));
            c.bend = bend.into_iter().filter(|b| b.tick >= cs).map(|b| Bend { tick: b.tick - cs, ..b }).collect();
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
    let copies: Vec<(usize, Vec<Note>, Vec<Bend>)> = p
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
            // The copy's bend, from the gap's start, back to none at its end.
            let mut bend = bend_window(&t.absolute_bend(), from, to);
            if bend.last().is_some_and(|b| b.value != 0) {
                bend.push(Bend { tick: len, value: 0 });
            }
            (i, notes, bend)
        })
        .collect();
    let sections: Vec<Section> =
        p.sections.iter().filter(|s| s.start >= from && s.start + s.length <= to).cloned().collect();
    insert_time(p, to, len);
    for (i, notes, bend) in copies {
        if notes.is_empty() && bend.is_empty() {
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
                // The gap was left unbent by the insert; the copy's bend
                // goes in it, and what followed resumes after.
                let (g0, g1) = (to - cs, to - cs + len);
                let after = bend_at(&c.bend, g1);
                c.bend.retain(|b| b.tick < g0 || b.tick > g1);
                c.bend.extend(bend.iter().map(|b| Bend { tick: g0 + b.tick, ..*b }));
                c.bend.push(Bend { tick: g1, value: after });
                c.bend.sort_by_key(|b| b.tick);
            }
            None => {
                t.clips.push(Clip {
                    id: clip_id,
                    name: "Copy".into(),
                    start: to,
                    length: len,
                    notes: notes.iter().map(|n| Note { start: n.start - to, ..*n }).collect(),
                    bend: bend.clone(),
                    source_session: None,
                    ..Default::default()
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
    use crate::model::{Id, PPQ};

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

    #[test]
    fn bend_moves_with_the_bars() {
        let mut p = Project::new("b");
        let t = p.add_track("Voice", 0);
        let notes: Vec<Note> = (0..4).map(|i| Note { pitch: 60, velocity: 90, start: i * BAR, duration: BAR / 2 }).collect();
        let bend = vec![Bend { tick: BAR + 10, value: 500 }, Bend { tick: 2 * BAR, value: 0 }];
        p.track_mut(t).unwrap().clips.push(Clip { id: Id(9), start: 0, length: 4 * BAR, notes, bend, ..Default::default() });
        duplicate_time(&mut p, BAR, BAR);
        let b = p.tracks[0].absolute_bend();
        // Bar 2's bend is in bar 2 and in its copy, bar 3.
        assert!(b.contains(&Bend { tick: BAR + 10, value: 500 }) && b.contains(&Bend { tick: 2 * BAR + 10, value: 500 }), "{b:?}");
        assert_eq!(crate::model::bend_at(&b, 2 * BAR + 20), 500);
        assert_eq!(crate::model::bend_at(&b, 3 * BAR + 20), 0);
        delete_time(&mut p, BAR, BAR);
        let b = p.tracks[0].absolute_bend();
        assert_eq!(crate::model::bend_at(&b, BAR + 20), 500, "the copy is where the original was: {b:?}");
        assert_eq!(crate::model::bend_at(&b, 2 * BAR + 20), 0);
        insert_time(&mut p, BAR + 100, BAR);
        let b = p.tracks[0].absolute_bend();
        assert_eq!(crate::model::bend_at(&b, BAR + 200), 0, "the gap is unbent: {b:?}");
        assert_eq!(crate::model::bend_at(&b, 2 * BAR + 100), 500, "and it resumes after");
    }
}
