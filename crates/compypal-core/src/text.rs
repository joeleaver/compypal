//! Reading and writing music as short text, for tools and agents: pitches
//! like `C#4`, positions like `3.2.240` (bar.beat.tick, one-based like the
//! UI), and durations as note values like `1/8`, `1/4.` or `1/8t`.

use crate::gm::pitch_name;
use crate::model::{MeterChange, Note, PPQ, Project, Tick};
use crate::theory::parse_pc;

/// `C4` is 60. Accepts sharps and flats (`Eb3`, `F#5`) and plain MIDI
/// numbers (`60`).
pub fn parse_pitch(text: &str) -> Option<u8> {
    let t = text.trim();
    if let Ok(n) = t.parse::<u8>() {
        return (n <= 127).then_some(n);
    }
    let (pc, rest) = parse_pc(t)?;
    let octave: i32 = rest.trim().parse().ok()?;
    // Use the letter's own octave so B#3 is C4 and Cb4 is B3.
    let letter = t.chars().next()?.to_ascii_uppercase();
    let natural = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        _ => 11,
    };
    let shift = (pc as i32 - natural + 6).rem_euclid(12) - 6;
    let p = (octave + 1) * 12 + natural + shift;
    (0..=127).contains(&p).then_some(p as u8)
}

/// The first tick of bar `bar` (one-based), following meter changes.
pub fn bar_start(project: &Project, bar: u32) -> Tick {
    let mut tick = 0;
    for _ in 1..bar.max(1) {
        tick += project.meter_at(tick).ticks_per_bar();
    }
    tick
}

/// The one-based bar containing `tick`.
pub fn bar_of(project: &Project, tick: Tick) -> u32 {
    let mut bar = 1;
    let mut start = 0;
    loop {
        let next = start + project.meter_at(start).ticks_per_bar();
        if next > tick {
            return bar;
        }
        start = next;
        bar += 1;
    }
}

/// `3`, `3.2` or `3.2.240`: bar, beat and tick, one-based bar and beat.
/// A plain integer above 1000 is taken as an absolute tick.
pub fn parse_position(project: &Project, text: &str) -> Option<Tick> {
    let t = text.trim();
    let parts: Vec<&str> = t.split('.').collect();
    if parts.len() == 1 {
        let n: u64 = t.parse().ok()?;
        return Some(if n > 1000 { n } else { bar_start(project, n as u32) });
    }
    let bar: u32 = parts[0].parse().ok()?;
    let beat: u64 = parts[1].parse().ok()?;
    let tick: u64 = parts.get(2).map_or(Some(0), |s| s.parse().ok())?;
    let start = bar_start(project, bar);
    Some(start + beat.saturating_sub(1) * project.meter_at(start).ticks_per_beat() + tick)
}

pub fn format_position(project: &Project, tick: Tick) -> String {
    let bar = bar_of(project, tick);
    let start = bar_start(project, bar);
    let m = project.meter_at(start);
    let rel = tick - start;
    format!("{bar}.{}.{}", rel / m.ticks_per_beat() + 1, rel % m.ticks_per_beat())
}

/// `1/4` is a quarter note; `1/8.` dotted; `1/8t` triplet; `3/16` three
/// sixteenths; a bare number is ticks.
pub fn parse_duration(text: &str) -> Option<Tick> {
    let t = text.trim();
    if let Ok(n) = t.parse::<u64>() {
        return Some(n);
    }
    let (t, dotted) = t.strip_suffix('.').map_or((t, false), |s| (s, true));
    let (t, triplet) = t.strip_suffix('t').map_or((t, false), |s| (s, true));
    let (num, den) = t.split_once('/')?;
    let (num, den): (u64, u64) = (num.trim().parse().ok()?, den.trim().parse().ok()?);
    if den == 0 {
        return None;
    }
    let mut d = PPQ as u64 * 4 * num / den;
    if dotted {
        d = d * 3 / 2;
    }
    if triplet {
        d = d * 2 / 3;
    }
    Some(d)
}

/// The shortest note value that names `ticks` exactly, else plain ticks.
pub fn format_duration(ticks: Tick) -> String {
    let whole = PPQ as u64 * 4;
    for den in [1u64, 2, 4, 8, 16, 32, 64] {
        let unit = whole / den;
        if ticks.is_multiple_of(unit) {
            let n = ticks / unit;
            return if n == 1 { format!("1/{den}") } else { format!("{n}/{den}") };
        }
        if ticks * 2 == unit * 3 {
            return format!("1/{den}.");
        }
        if ticks * 3 == unit * 2 {
            return format!("1/{den}t");
        }
    }
    ticks.to_string()
}

/// One note per line: `3.2.240  E4  vel 92  1/8`. Compact enough to read a
/// few hundred notes at once.
pub fn format_notes(project: &Project, notes: &[Note]) -> String {
    notes
        .iter()
        .map(|n| {
            format!(
                "{}  {}  vel {}  {}",
                format_position(project, n.start),
                pitch_name(n.pitch),
                n.velocity,
                format_duration(n.duration)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The meter in force at a bar, for printing as `4/4`.
pub fn meter_text(m: &MeterChange) -> String {
    format!("{}/{}", m.numerator, m.denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitches() {
        assert_eq!(parse_pitch("C4"), Some(60));
        assert_eq!(parse_pitch("c#4"), Some(61));
        assert_eq!(parse_pitch("Eb3"), Some(51));
        assert_eq!(parse_pitch("B#3"), Some(60));
        assert_eq!(parse_pitch("Cb4"), Some(59));
        assert_eq!(parse_pitch("A-1"), Some(9));
        assert_eq!(parse_pitch("64"), Some(64));
        assert_eq!(parse_pitch("H2"), None);
    }

    #[test]
    fn positions_and_durations() {
        let mut p = Project::new("t");
        p.meter.push(MeterChange { tick: 3840 * 2, numerator: 3, denominator: 4 });
        assert_eq!(parse_position(&p, "1"), Some(0));
        assert_eq!(parse_position(&p, "2.3"), Some(3840 + 1920));
        assert_eq!(parse_position(&p, "4.1.120"), Some(3840 * 2 + 2880 + 120));
        assert_eq!(format_position(&p, 3840 * 2 + 2880 + 120), "4.1.120");
        assert_eq!(bar_of(&p, 3840 * 2 + 2879), 3);
        assert_eq!(parse_duration("1/8"), Some(480));
        assert_eq!(parse_duration("1/4."), Some(1440));
        assert_eq!(parse_duration("1/8t"), Some(320));
        assert_eq!(parse_duration("3/16"), Some(720));
        for d in [480, 1440, 320, 720, 3840, 7] {
            assert_eq!(parse_duration(&format_duration(d)), Some(d), "{d}");
        }
    }
}
