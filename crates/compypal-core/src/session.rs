//! Raw takes from a MIDI controller.
//!
//! A session is a faithful record of what was played, timestamped in seconds
//! from the start of recording. It is never edited: cleanup produces clips
//! from it, so the original performance is always there to go back to.

use serde::{Deserialize, Serialize};

use crate::model::{Id, MeterChange};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: Id,
    pub name: String,
    /// Unix seconds.
    pub recorded_at: u64,
    /// Click tempo during recording, if a click was running. Without one,
    /// the timing is free and tempo has to be inferred.
    pub click_bpm: Option<f64>,
    pub meter: MeterChange,
    /// Seconds from the start of the take to the first downbeat of the
    /// click. Zero when there was no count-in.
    pub downbeat_offset: f64,
    pub events: Vec<RawEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawEvent {
    /// Seconds since the take started.
    pub t: f64,
    pub channel: u8,
    pub msg: RawMsg,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RawMsg {
    NoteOn { pitch: u8, velocity: u8 },
    NoteOff { pitch: u8 },
    Cc { controller: u8, value: u8 },
    PitchBend { value: i16 },
}

impl RawMsg {
    /// Decodes a channel voice message. Returns `None` for anything this
    /// model does not keep (aftertouch, program change, system messages).
    pub fn from_midi(bytes: &[u8]) -> Option<(u8, RawMsg)> {
        let (&status, data) = bytes.split_first()?;
        let channel = status & 0x0f;
        let msg = match (status & 0xf0, data) {
            (0x90, [p, 0, ..]) => RawMsg::NoteOff { pitch: *p },
            (0x90, [p, v, ..]) => RawMsg::NoteOn { pitch: *p, velocity: *v },
            (0x80, [p, ..]) => RawMsg::NoteOff { pitch: *p },
            (0xb0, [c, v, ..]) => RawMsg::Cc { controller: *c, value: *v },
            (0xe0, [lsb, msb, ..]) => {
                RawMsg::PitchBend { value: ((*msb as i16) << 7 | *lsb as i16) - 8192 }
            }
            _ => return None,
        };
        Some((channel, msg))
    }
}

/// A note as played: real start time and held duration in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawNote {
    pub start: f64,
    /// How long the key was physically held.
    pub held: f64,
    /// How long the note sounded, including the sustain pedal.
    pub sounding: f64,
    pub pitch: u8,
    pub velocity: u8,
    pub channel: u8,
}

const SUSTAIN: u8 = 64;

impl Session {
    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.t)
    }

    /// Pairs note-ons with note-offs. A key struck again while still down
    /// ends the earlier note. Notes still held at the end of the take end
    /// there. The sustain pedal extends `sounding` but not `held`.
    pub fn notes(&self) -> Vec<RawNote> {
        let mut out: Vec<RawNote> = Vec::new();
        // Index into `out` of the note currently down, per (channel, pitch).
        let mut down: [[Option<usize>; 128]; 16] = [[None; 128]; 16];
        // Notes released while the pedal was down, waiting for pedal up.
        let mut pedalled: [Vec<usize>; 16] = Default::default();
        let mut pedal = [false; 16];

        let end = |out: &mut Vec<RawNote>, i: usize, t: f64, sounding: bool| {
            let n = &mut out[i];
            if sounding {
                n.sounding = (t - n.start).max(0.0);
            } else {
                n.held = (t - n.start).max(0.0);
                n.sounding = n.held;
            }
        };

        for e in &self.events {
            let ch = (e.channel & 0x0f) as usize;
            match e.msg {
                RawMsg::NoteOn { pitch, velocity } => {
                    let p = (pitch & 0x7f) as usize;
                    if let Some(i) = down[ch][p].take() {
                        end(&mut out, i, e.t, false);
                    }
                    // A restrike cuts off a pedalled note of the same pitch.
                    if let Some(pos) = pedalled[ch].iter().position(|&i| out[i].pitch == pitch) {
                        let i = pedalled[ch].swap_remove(pos);
                        end(&mut out, i, e.t, true);
                    }
                    down[ch][p] = Some(out.len());
                    out.push(RawNote {
                        start: e.t,
                        held: 0.0,
                        sounding: 0.0,
                        pitch,
                        velocity,
                        channel: ch as u8,
                    });
                }
                RawMsg::NoteOff { pitch } => {
                    if let Some(i) = down[ch][(pitch & 0x7f) as usize].take() {
                        end(&mut out, i, e.t, false);
                        if pedal[ch] {
                            pedalled[ch].push(i);
                        }
                    }
                }
                RawMsg::Cc { controller: SUSTAIN, value } => {
                    let on = value >= 64;
                    if pedal[ch] && !on {
                        for i in pedalled[ch].drain(..) {
                            end(&mut out, i, e.t, true);
                        }
                    }
                    pedal[ch] = on;
                }
                _ => {}
            }
        }

        let t_end = self.duration();
        for ch in 0..16 {
            for i in down[ch].iter().flatten() {
                end(&mut out, *i, t_end, false);
            }
            for &i in &pedalled[ch] {
                end(&mut out, i, t_end, true);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: f64, msg: RawMsg) -> RawEvent {
        RawEvent { t, channel: 0, msg }
    }

    fn session(events: Vec<RawEvent>) -> Session {
        Session {
            id: Id(1),
            name: "take".into(),
            recorded_at: 0,
            click_bpm: Some(120.0),
            meter: MeterChange { tick: 0, numerator: 4, denominator: 4 },
            downbeat_offset: 0.0,
            events,
        }
    }

    #[test]
    fn decodes_midi_bytes() {
        assert_eq!(RawMsg::from_midi(&[0x93, 60, 0]), Some((3, RawMsg::NoteOff { pitch: 60 })));
        assert_eq!(
            RawMsg::from_midi(&[0xe0, 0, 64]),
            Some((0, RawMsg::PitchBend { value: 0 }))
        );
        assert_eq!(RawMsg::from_midi(&[0xf8]), None);
    }

    #[test]
    fn sustain_extends_sounding_not_held() {
        let s = session(vec![
            ev(0.0, RawMsg::Cc { controller: 64, value: 127 }),
            ev(0.0, RawMsg::NoteOn { pitch: 60, velocity: 80 }),
            ev(0.5, RawMsg::NoteOff { pitch: 60 }),
            ev(2.0, RawMsg::Cc { controller: 64, value: 0 }),
        ]);
        let n = s.notes();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].held, 0.5);
        assert_eq!(n[0].sounding, 2.0);
    }

    #[test]
    fn restrike_ends_previous_note() {
        let s = session(vec![
            ev(0.0, RawMsg::NoteOn { pitch: 60, velocity: 80 }),
            ev(1.0, RawMsg::NoteOn { pitch: 60, velocity: 70 }),
            ev(1.5, RawMsg::NoteOff { pitch: 60 }),
        ]);
        let n = s.notes();
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].held, 1.0);
        assert_eq!(n[1].held, 0.5);
    }
}
