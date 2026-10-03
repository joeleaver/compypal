//! The realtime side: runs inside the audio callback. It owns the synth,
//! takes commands without blocking, and renders sample-accurately by
//! splitting each buffer at event times.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;

use crate::schedule::{Schedule, Timed};
use crate::synth::Synth;

pub enum Cmd {
    /// Start the transport at `from` seconds.
    Play { schedule: Arc<Schedule>, from: f64, looping: bool },
    /// Swap in an edited schedule without stopping.
    Update(Arc<Schedule>),
    Stop,
    SetLooping(bool),
    /// Play a phrase now, alongside (or instead of) the transport.
    Audition(Arc<Schedule>),
    /// A message straight to the synth: live input, clicking a key.
    Midi([u8; 3]),
    Panic,
    SetSynth(Box<dyn Synth>),
}

/// What the UI can read without talking to the audio thread.
#[derive(Default)]
pub struct Shared {
    /// Transport position in seconds, as f64 bits.
    position: AtomicU64,
    playing: AtomicBool,
}

impl Shared {
    pub fn position(&self) -> Option<f64> {
        self.playing
            .load(Ordering::Relaxed)
            .then(|| f64::from_bits(self.position.load(Ordering::Relaxed)))
    }
}

/// One stream of scheduled events and where it has got to.
struct Run {
    schedule: Arc<Schedule>,
    pos: f64,
    next: usize,
    looping: bool,
    /// Notes this run has started and not yet stopped, to release on stop.
    sounding: Vec<[u8; 2]>,
}

impl Run {
    fn new(schedule: Arc<Schedule>, from: f64, looping: bool) -> Self {
        let next = schedule.events.partition_point(|e| e.t < from);
        Self { schedule, pos: from, next, looping, sounding: Vec::new() }
    }

    fn next_time(&self) -> f64 {
        match self.schedule.events.get(self.next) {
            Some(e) => e.t.min(self.schedule.length),
            None => self.schedule.length,
        }
    }

    fn release(&mut self, synth: &mut dyn Synth) {
        for [status, key] in self.sounding.drain(..) {
            synth.midi(0x80 | (status & 0x0f), key, 0);
        }
    }

    fn fire(&mut self, e: &Timed, synth: &mut dyn Synth) {
        let [status, key, _] = e.msg;
        if e.is_note_on() {
            self.sounding.push([status, key]);
        } else if e.is_note()
            && let Some(i) = self.sounding.iter().position(|s| s[0] & 0x0f == status & 0x0f && s[1] == key)
        {
            self.sounding.swap_remove(i);
        }
        synth.midi(e.msg[0], e.msg[1], e.msg[2]);
    }

    /// Fires every event due at or before `pos`. Returns false when the run
    /// has finished.
    fn fire_due(&mut self, synth: &mut dyn Synth) -> bool {
        while let Some(e) = self.schedule.events.get(self.next) {
            if e.t > self.pos + 1e-9 {
                break;
            }
            let e = *e;
            self.next += 1;
            self.fire(&e, synth);
        }
        if self.pos + 1e-9 >= self.schedule.length {
            self.release(synth);
            if !self.looping {
                return false;
            }
            self.pos -= self.schedule.length;
            self.next = 0;
            return self.fire_due(synth);
        }
        true
    }
}

/// Re-sends program, volume and pan changes from before `from`, so starting
/// mid-song sounds like playing up to there.
fn chase(schedule: &Schedule, from: f64, synth: &mut dyn Synth) {
    for e in schedule.events.iter().take_while(|e| e.t <= from) {
        if !e.is_note() {
            synth.midi(e.msg[0], e.msg[1], e.msg[2]);
        }
    }
}

pub struct Player {
    synth: Box<dyn Synth>,
    rate: f64,
    transport: Option<Run>,
    audition: Option<Run>,
    cmds: Receiver<Cmd>,
    shared: Arc<Shared>,
}

impl Player {
    pub fn new(synth: Box<dyn Synth>, rate: u32, cmds: Receiver<Cmd>, shared: Arc<Shared>) -> Self {
        Self { synth, rate: rate as f64, transport: None, audition: None, cmds, shared }
    }

    fn handle(&mut self, cmd: Cmd) {
        let synth = self.synth.as_mut();
        match cmd {
            Cmd::Play { schedule, from, looping } => {
                if let Some(mut r) = self.transport.take() {
                    r.release(synth);
                }
                chase(&schedule, from, synth);
                self.transport = Some(Run::new(schedule, from, looping));
            }
            Cmd::Update(schedule) => {
                if let Some(r) = self.transport.as_mut() {
                    r.release(synth);
                    let pos = if r.looping && schedule.length > 0.0 { r.pos % schedule.length } else { r.pos };
                    let looping = r.looping;
                    chase(&schedule, pos, synth);
                    *r = Run::new(schedule, pos, looping);
                }
            }
            Cmd::Stop => {
                if let Some(mut r) = self.transport.take() {
                    r.release(synth);
                }
            }
            Cmd::SetLooping(on) => {
                if let Some(r) = self.transport.as_mut() {
                    r.looping = on;
                }
            }
            Cmd::Audition(schedule) => {
                if let Some(mut r) = self.audition.take() {
                    r.release(synth);
                }
                self.audition = Some(Run::new(schedule, 0.0, false));
            }
            Cmd::Midi([a, b, c]) => synth.midi(a, b, c),
            Cmd::Panic => {
                self.transport = None;
                self.audition = None;
                synth.panic();
            }
            Cmd::SetSynth(mut new) => {
                // Carry the channel setup over by replaying it.
                if let Some(r) = &self.transport {
                    chase(&r.schedule, r.pos, new.as_mut());
                }
                self.synth.panic();
                self.synth = new;
                if let Some(r) = self.transport.as_mut() {
                    r.sounding.clear();
                }
            }
        }
    }

    /// Fills `left` and `right` with the next frames.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        while let Ok(cmd) = self.cmds.try_recv() {
            self.handle(cmd);
        }
        left.fill(0.0);
        right.fill(0.0);

        let mut done = 0;
        while done < left.len() {
            for run in [&mut self.transport, &mut self.audition] {
                if let Some(r) = run
                    && !r.fire_due(self.synth.as_mut())
                {
                    *run = None;
                }
            }
            // Render up to the next event of either run, sample-accurately.
            let mut frames = left.len() - done;
            for r in [&self.transport, &self.audition].into_iter().flatten() {
                let until = ((r.next_time() - r.pos) * self.rate).ceil().max(1.0) as usize;
                frames = frames.min(until);
            }
            self.synth.render(&mut left[done..done + frames], &mut right[done..done + frames]);
            let secs = frames as f64 / self.rate;
            for r in [&mut self.transport, &mut self.audition].into_iter().flatten() {
                r.pos += secs;
            }
            done += frames;
        }

        match &self.transport {
            Some(r) => {
                self.shared.position.store(r.pos.to_bits(), Ordering::Relaxed);
                self.shared.playing.store(true, Ordering::Relaxed);
            }
            None => self.shared.playing.store(false, Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::channel;

    /// What the spy was told, and at which frame.
    type Log = Arc<Mutex<Vec<(usize, [u8; 3])>>>;

    /// Records what it was told and when, in frames.
    struct Spy {
        log: Log,
        frames: usize,
    }

    impl Synth for Spy {
        fn midi(&mut self, a: u8, b: u8, c: u8) {
            self.log.lock().unwrap().push((self.frames, [a, b, c]));
        }
        fn panic(&mut self) {}
        fn render(&mut self, left: &mut [f32], _: &mut [f32]) {
            self.frames += left.len();
        }
        fn name(&self) -> String {
            "spy".into()
        }
    }

    fn player() -> (Player, std::sync::mpsc::Sender<Cmd>, Log, Arc<Shared>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let shared = Arc::new(Shared::default());
        let p = Player::new(Box::new(Spy { log: log.clone(), frames: 0 }), 1000, rx, shared.clone());
        (p, tx, log, shared)
    }

    fn sched(events: &[(f64, [u8; 3])], length: f64) -> Arc<Schedule> {
        Arc::new(Schedule { events: events.iter().map(|&(t, msg)| Timed { t, msg }).collect(), length })
    }

    #[test]
    fn events_land_on_the_right_frame() {
        let (mut p, tx, log, shared) = player();
        tx.send(Cmd::Play {
            schedule: sched(&[(0.0105, [0x90, 60, 100]), (0.5, [0x80, 60, 0])], 1.0),
            from: 0.0,
            looping: false,
        })
        .unwrap();
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        for _ in 0..3 {
            p.process(&mut l, &mut r);
        }
        let log = log.lock().unwrap();
        // At 1000 Hz, 10.5 ms is between frames 10 and 11.
        assert_eq!(log[0], (11, [0x90, 60, 100]));
        assert_eq!(log[1], (500, [0x80, 60, 0]));
        assert!((shared.position().unwrap() - 0.768).abs() < 1e-9);
    }

    #[test]
    fn stop_releases_held_notes_and_loop_wraps() {
        let (mut p, tx, log, _) = player();
        tx.send(Cmd::Play { schedule: sched(&[(0.0, [0x91, 64, 90])], 0.1), from: 0.0, looping: true }).unwrap();
        let (mut l, mut r) = (vec![0.0; 250], vec![0.0; 250]);
        p.process(&mut l, &mut r);
        tx.send(Cmd::Stop).unwrap();
        p.process(&mut l, &mut r);
        let log = log.lock().unwrap();
        let ons = log.iter().filter(|(_, m)| m[0] == 0x91).count();
        assert_eq!(ons, 3, "0.1s loop over 0.25s plays three times: {log:?}");
        assert_eq!(log.last().unwrap().1, [0x81, 64, 0]);
    }
}
