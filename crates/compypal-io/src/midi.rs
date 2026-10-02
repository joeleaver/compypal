//! Standard MIDI File import and export (format 1).

use compypal_core::{Clip, KeySignature, MeterChange, Note, PPQ, Project, Tick};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

#[derive(Debug, thiserror::Error)]
pub enum MidiError {
    #[error("not a valid MIDI file: {0}")]
    Parse(#[from] midly::Error),
    #[error("SMPTE-timed MIDI files are not supported")]
    Smpte,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Writes the project as a format 1 file: a conductor track with tempo,
/// meter and key, then one track per project track. Mute and solo are
/// ignored; everything is exported.
pub fn export(project: &Project) -> Result<Vec<u8>, MidiError> {
    let mut tracks = Vec::with_capacity(project.tracks.len() + 1);

    let mut conductor: Vec<(Tick, u8, TrackEventKind)> = vec![(
        0,
        0,
        TrackEventKind::Meta(MetaMessage::TrackName(project.name.as_bytes())),
    )];
    for c in project.tempo.changes() {
        let usec = (60_000_000.0 / c.bpm).round() as u32;
        conductor.push((c.tick, 1, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(usec)))));
    }
    for m in &project.meter {
        let log2 = m.denominator.max(1).trailing_zeros() as u8;
        conductor.push((
            m.tick,
            1,
            TrackEventKind::Meta(MetaMessage::TimeSignature(m.numerator, log2, 24, 8)),
        ));
    }
    conductor.push((
        0,
        1,
        TrackEventKind::Meta(MetaMessage::KeySignature(project.key.fifths, project.key.minor)),
    ));
    for s in &project.sections {
        conductor.push((s.start, 2, TrackEventKind::Meta(MetaMessage::Marker(s.name.as_bytes()))));
    }
    tracks.push(finish(conductor, project.end_tick()));

    for t in &project.tracks {
        let ch = u4::new(t.channel & 0x0f);
        let midi = |message| TrackEventKind::Midi { channel: ch, message };
        let mut evs = vec![(0, 0, TrackEventKind::Meta(MetaMessage::TrackName(t.name.as_bytes())))];
        if !t.is_drums() {
            evs.push((0, 1, midi(MidiMessage::ProgramChange { program: u7::new(t.program & 0x7f) })));
        }
        evs.push((0, 1, midi(MidiMessage::Controller { controller: u7::new(7), value: u7::new(t.volume & 0x7f) })));
        evs.push((0, 1, midi(MidiMessage::Controller { controller: u7::new(10), value: u7::new(t.pan & 0x7f) })));
        for n in t.absolute_notes() {
            let key = u7::new(n.pitch & 0x7f);
            // Offs sort before ons at the same tick so repeated notes retrigger.
            evs.push((n.end(), 2, midi(MidiMessage::NoteOff { key, vel: u7::new(0) })));
            evs.push((n.start, 3, midi(MidiMessage::NoteOn { key, vel: u7::new(n.velocity.clamp(1, 127)) })));
        }
        tracks.push(finish(evs, 0));
    }

    let smf = Smf {
        header: Header::new(Format::Parallel, Timing::Metrical(u15::new(PPQ as u16))),
        tracks,
    };
    let mut out = Vec::new();
    smf.write_std(&mut out)?;
    Ok(out)
}

/// Sorts by tick then priority, converts to deltas, and appends end-of-track
/// no earlier than `min_end`.
fn finish(mut evs: Vec<(Tick, u8, TrackEventKind)>, min_end: Tick) -> Vec<TrackEvent> {
    evs.sort_by_key(|(t, p, _)| (*t, *p));
    let mut last = 0;
    let mut out: Vec<TrackEvent> = evs
        .into_iter()
        .map(|(t, _, kind)| {
            let delta = u28::new((t - last) as u32);
            last = t;
            TrackEvent { delta, kind }
        })
        .collect();
    let end = min_end.max(last);
    out.push(TrackEvent {
        delta: u28::new((end - last) as u32),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    out
}

/// Reads a MIDI file into a new project. Each (file track, channel) pair
/// becomes a project track with a single clip holding all its notes. Ticks
/// are rescaled to the project's resolution.
pub fn import(bytes: &[u8], name: &str) -> Result<Project, MidiError> {
    let smf = Smf::parse(bytes)?;
    let Timing::Metrical(tpq) = smf.header.timing else {
        return Err(MidiError::Smpte);
    };
    let scale = PPQ as f64 / tpq.as_int().max(1) as f64;
    let at = |t: u64| (t as f64 * scale).round() as Tick;

    let mut p = Project::new(name);
    p.meter.clear();
    let mut first_tempo = true;

    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut tick = 0u64;
        let mut track_name = None;
        // Per channel: program, open notes by pitch, finished notes.
        let mut program = [0u8; 16];
        let mut open: Vec<[Option<(Tick, u8)>; 128]> = vec![[None; 128]; 16];
        let mut notes: Vec<Vec<Note>> = vec![Vec::new(); 16];

        for ev in track {
            tick += ev.delta.as_int() as u64;
            let now = at(tick);
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) => {
                    track_name = Some(String::from_utf8_lossy(n).into_owned());
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    let bpm = 60_000_000.0 / us.as_int().max(1) as f64;
                    if first_tempo && now != 0 {
                        p.tempo.set(0, bpm);
                    }
                    first_tempo = false;
                    p.tempo.set(now, bpm);
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, log2, _, _)) => {
                    p.meter.retain(|m| m.tick != now);
                    p.meter.push(MeterChange { tick: now, numerator: num, denominator: 1 << log2.min(6) });
                }
                TrackEventKind::Meta(MetaMessage::KeySignature(fifths, minor)) => {
                    p.key = KeySignature { fifths, minor };
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int() as usize;
                    match message {
                        MidiMessage::ProgramChange { program: pr } => program[ch] = pr.as_int(),
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let k = key.as_int() as usize;
                            if let Some((s, v)) = open[ch][k].take() {
                                notes[ch].push(note(k as u8, v, s, now));
                            }
                            open[ch][k] = Some((now, vel.as_int()));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let k = key.as_int() as usize;
                            if let Some((s, v)) = open[ch][k].take() {
                                notes[ch].push(note(k as u8, v, s, now));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let end = at(tick);
        for ch in 0..16 {
            for (k, slot) in open[ch].iter().enumerate() {
                if let Some((s, v)) = slot {
                    notes[ch].push(note(k as u8, *v, *s, end));
                }
            }
        }
        let channels_used = notes.iter().filter(|n| !n.is_empty()).count();
        let base = track_name.unwrap_or_else(|| format!("Track {}", ti + 1));
        for ch in 0..16 {
            if notes[ch].is_empty() {
                continue;
            }
            let mut ns = std::mem::take(&mut notes[ch]);
            ns.sort_by_key(|n| (n.start, n.pitch));
            let label =
                if channels_used > 1 { format!("{base} (ch {})", ch + 1) } else { base.clone() };
            let id = p.add_track(label, program[ch]);
            let clip_id = p.alloc_id();
            let t = p.track_mut(id).unwrap();
            t.channel = ch as u8;
            let length = ns.iter().map(|n| n.end()).max().unwrap_or(0);
            t.clips.push(Clip {
                id: clip_id,
                name: "Imported".into(),
                start: 0,
                length,
                notes: ns,
                source_session: None,
            });
        }
    }
    if p.meter.is_empty() {
        p.meter.push(MeterChange { tick: 0, numerator: 4, denominator: 4 });
    }
    p.meter.sort_by_key(|m| m.tick);
    Ok(p)
}

fn note(pitch: u8, velocity: u8, start: Tick, end: Tick) -> Note {
    Note { pitch, velocity, start, duration: end.saturating_sub(start).max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_demo_notes() {
        let p = compypal_core::demo::project();
        let bytes = export(&p).unwrap();
        let back = import(&bytes, "back").unwrap();
        assert_eq!(back.tracks.len(), p.tracks.len());
        for (a, b) in p.tracks.iter().zip(&back.tracks) {
            assert_eq!(a.absolute_notes(), b.absolute_notes(), "track {}", a.name);
            assert_eq!(a.channel, b.channel);
            assert!(b.name.starts_with(&a.name));
        }
        assert_eq!(back.tempo.bpm_at(0), 100.0);
    }
}
