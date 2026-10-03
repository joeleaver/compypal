//! Turning a project into timed MIDI messages. Pure: no audio here, so it
//! can be tested and rebuilt on every edit while the music keeps playing.

use compypal_core::{DRUM_CHANNEL, Note, Project, Tick};

/// A MIDI message at a time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed {
    pub t: f64,
    pub msg: [u8; 3],
}

impl Timed {
    pub fn is_note_on(&self) -> bool {
        self.msg[0] & 0xf0 == 0x90 && self.msg[2] > 0
    }

    pub fn is_note(&self) -> bool {
        matches!(self.msg[0] & 0xf0, 0x80 | 0x90)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schedule {
    /// Sorted by time; at equal times, note-offs come before note-ons.
    pub events: Vec<Timed>,
    /// Where playback ends, or loops back to the start.
    pub length: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScheduleOptions {
    pub metronome: bool,
    /// Keep going (clicking, if the metronome is on) to at least here,
    /// past the last note: room to record into.
    pub until: Tick,
}

/// GM percussion: high and low wood block for the click.
const CLICK_ACCENT: u8 = 76;
const CLICK: u8 = 77;

pub fn build(project: &Project, opts: ScheduleOptions) -> Schedule {
    let secs = |tick: Tick| project.tempo.tick_to_seconds(tick as f64);
    let soloing = project.tracks.iter().any(|t| t.solo);
    let mut events = Vec::new();

    for t in &project.tracks {
        let ch = t.channel & 0x0f;
        if !t.is_drums() {
            events.push(Timed { t: 0.0, msg: [0xc0 | ch, t.program & 0x7f, 0] });
        }
        events.push(Timed { t: 0.0, msg: [0xb0 | ch, 7, t.volume & 0x7f] });
        events.push(Timed { t: 0.0, msg: [0xb0 | ch, 10, t.pan & 0x7f] });
        if t.muted || (soloing && !t.solo) {
            continue;
        }
        push_notes(&mut events, &t.absolute_notes(), ch, secs);
    }

    // Round the end up to a whole bar, so a loop comes round in time.
    let end = project.end_tick().max(opts.until);
    let mut bar_end = 0;
    while bar_end < end.max(1) {
        bar_end += project.meter_at(bar_end).ticks_per_bar();
    }

    if opts.metronome {
        let mut tick = 0;
        while tick < bar_end {
            let m = project.meter_at(tick);
            for beat in 0..m.numerator as Tick {
                let at = tick + beat * m.ticks_per_beat();
                let (key, vel) = if beat == 0 { (CLICK_ACCENT, 110) } else { (CLICK, 80) };
                let t0 = secs(at);
                events.push(Timed { t: t0, msg: [0x90 | DRUM_CHANNEL, key, vel] });
                events.push(Timed { t: t0 + 0.05, msg: [0x80 | DRUM_CHANNEL, key, 0] });
            }
            tick += m.ticks_per_bar();
        }
    }

    sort(&mut events);
    Schedule { events, length: secs(bar_end) }
}

/// What plays while recording from `from` seconds: `beats` count-in clicks
/// of `beat` seconds each, then the song from that point. Time zero is the
/// first count-in click; the song's `from` lands at `beats * beat`.
pub fn with_count_in(song: &Schedule, from: f64, beats: u8, beat: f64) -> Schedule {
    let lead = beats as f64 * beat;
    let mut events: Vec<Timed> = song
        .events
        .iter()
        .filter(|e| !e.is_note() && e.t <= from)
        .map(|e| Timed { t: 0.0, ..*e })
        .collect();
    for i in 0..beats {
        let (key, vel) = if i == 0 { (CLICK_ACCENT, 110) } else { (CLICK, 80) };
        let t = i as f64 * beat;
        events.push(Timed { t, msg: [0x90 | DRUM_CHANNEL, key, vel] });
        events.push(Timed { t: t + 0.05, msg: [0x80 | DRUM_CHANNEL, key, 0] });
    }
    events.extend(song.events.iter().filter(|e| e.t >= from).map(|e| Timed { t: e.t - from + lead, ..*e }));
    sort(&mut events);
    Schedule { events, length: (song.length - from).max(0.0) + lead }
}

/// A one-off phrase to hear right now: notes for one instrument, starting
/// at zero.
pub fn audition(notes: &[Note], channel: u8, program: u8, project: &Project) -> Schedule {
    let Some(first) = notes.iter().map(|n| n.start).min() else {
        return Schedule::default();
    };
    let origin = project.tempo.tick_to_seconds(first as f64);
    let secs = |tick: Tick| project.tempo.tick_to_seconds(tick as f64) - origin;
    let ch = channel & 0x0f;
    let mut events = Vec::new();
    if ch != DRUM_CHANNEL {
        events.push(Timed { t: 0.0, msg: [0xc0 | ch, program & 0x7f, 0] });
    }
    push_notes(&mut events, notes, ch, secs);
    sort(&mut events);
    let length = events.last().map_or(0.0, |e| e.t);
    Schedule { events, length }
}

fn push_notes(events: &mut Vec<Timed>, notes: &[Note], ch: u8, secs: impl Fn(Tick) -> f64) {
    for n in notes {
        let key = n.pitch & 0x7f;
        events.push(Timed { t: secs(n.start), msg: [0x90 | ch, key, n.velocity.clamp(1, 127)] });
        events.push(Timed { t: secs(n.end()), msg: [0x80 | ch, key, 0] });
    }
}

fn sort(events: &mut [Timed]) {
    // Offs before ons at the same instant, so a repeated note retriggers.
    events.sort_by(|a, b| a.t.total_cmp(&b.t).then(a.is_note_on().cmp(&b.is_note_on())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use compypal_core::demo;

    #[test]
    fn demo_schedule() {
        let p = demo::project();
        let s = build(&p, ScheduleOptions::default());
        // Four bars at 100 BPM.
        assert!((s.length - 9.6).abs() < 1e-9);
        let ons = s.events.iter().filter(|e| e.is_note_on()).count();
        let notes: usize = p.tracks.iter().map(|t| t.absolute_notes().len()).sum();
        assert_eq!(ons, notes);
        assert!(s.events.windows(2).all(|w| w[0].t <= w[1].t));
    }

    #[test]
    fn count_in_then_the_song_from_the_cursor() {
        let p = demo::project();
        let song = build(&p, ScheduleOptions::default());
        // From bar 3 (4.8s at 100 BPM), four 0.6s beats of count-in.
        let rec = with_count_in(&song, 4.8, 4, 0.6);
        assert!((rec.length - (9.6 - 4.8 + 2.4)).abs() < 1e-9);
        let first_note = rec.events.iter().find(|e| e.is_note_on() && e.msg[0] & 0x0f != DRUM_CHANNEL).unwrap();
        assert!((first_note.t - 2.4).abs() < 0.05, "{first_note:?}");
        let clicks = rec.events.iter().filter(|e| e.is_note_on() && e.t < 2.4 && e.msg[0] & 0x0f == DRUM_CHANNEL).count();
        assert_eq!(clicks, 4);
    }

    #[test]
    fn mute_solo_and_metronome() {
        let mut p = demo::project();
        p.tracks[1].solo = true;
        let s = build(&p, ScheduleOptions { metronome: true, ..Default::default() });
        let channels: std::collections::BTreeSet<u8> =
            s.events.iter().filter(|e| e.is_note_on()).map(|e| e.msg[0] & 0x0f).collect();
        // The soloed bass, plus the click on the drum channel.
        assert_eq!(channels.into_iter().collect::<Vec<_>>(), [p.tracks[1].channel, DRUM_CHANNEL]);
        let clicks = s.events.iter().filter(|e| e.is_note_on() && matches!(e.msg[1], CLICK | CLICK_ACCENT)).count();
        assert_eq!(clicks, 16);
    }
}
