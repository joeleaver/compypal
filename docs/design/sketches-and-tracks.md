# Playing, sketches and tracks: a plan

Status: proposal, for review. Nothing here is built yet.

compypal grew three ways to capture playing (Rec, Free, Listen), two kinds
of input (MIDI, audio) and two places raw playing lives (the journal and
each song). Each made sense when it was added; together they are hard to
explain. This note lists what a player wants to do, what the app does
today, and a smaller model that covers the same ground, then the decisions
it needs and a plan in phases.

## Who

One player-composer at a desk: a MIDI keyboard, sometimes a guitar or a
voice into an interface, an agent alongside. Ideas come while noodling,
not on cue. Songs are built from those ideas, then cleaned up and arranged
with the agent.

## User stories

### Capturing

- **C1** As a player, I noodle without deciding to record, and later find
  the good bit. Nothing I play is lost.
- **C2** I hum, sing or play guitar an idea into the mic and get it as
  notes I can build on.
- **C3** I record a part over the song, to the click: a bass line under
  bars 9–16 of what's there.
- **C4** I play with no click, and the song's grid fits my playing (or my
  playing fits the grid of a song that already has one).
- **C5** I play the same part several times and keep the best, or the best
  bars of each.
- **C6** I can always hear what I actually played (the recording, or the
  untidied timing), not only the cleaned-up notes.

### Finding

- **F1** "The thing in A minor I played yesterday evening": by time, key,
  chords or name, in the app or by asking the agent.
- **F2** At a glance, which ideas are in this song and which aren't.

### Building the song

- **B1** I put an idea into the song: on a new track or a chosen one, at
  the end or at a bar.
- **B2** I see each part's alternatives where they are, pick one per bar
  range, and nothing is lost when I switch.
- **B3** The agent cleans up, re-harmonises and arranges what I played,
  and the original stays underneath, recoverable.
- **B4** A track is a part of the band (Bass, Keys, Voice): I set its
  instrument, and can mute, solo and balance it.

### Talking about it

- **T1** The app, the agent and I use the same few words for the same
  things.

## What there is today

| Thing | Where it lives | What the UI calls it | What the agent calls it |
|---|---|---|---|
| Everything played while Listen is on (MIDI, and audio from the chosen input) | `journal/` in the data folder, across all songs | sketch (Sketches tab) | sketch (`list_sketches`, `get_sketch`, `hear_sketch`) |
| A Rec or Free recording | inside the song file (`Project::sessions`) | take ("Take 3", "Show raw take"), also a sketch in the Sketches tab | session (`get_session`) |
| A sketch added to the song | the song (a session with `own_tempo`) | the track it lands on is often named after it ("Jam 09:13") | session |
| What is heard on a track | clips on the track | not named; orange in the arranger when recorded | clips, notes |
| Music laid over music | muted clips, `muted_by` | "Takes" lanes above the roll; hatched in the arranger; "Use this one" | `use_sketch` |

Where the confusion comes from:

1. **Three capture buttons that overlap.** Rec records MIDI onto the
   selected track at the cursor, with a count-in. Free (a toggle next to
   Rec) records MIDI with no click and fits the grid afterwards. Listen
   captures everything, MIDI and audio, into the journal and never touches
   the song. Audio can only come in through Listen.
2. **Four words for raw playing.** Sketch, take, jam and session all appear
   somewhere (UI, track names, agent tools). CLAUDE.md already says
   "sketch" should be the only one in the UI.
3. **Two homes.** A Rec take lives in the song; a Listen sketch lives in
   the journal. The Sketches tab merges them, and a take recorded while
   listening is in both, de-duplicated by matching.
4. **Audio is only ever notes.** A guitar or voice is transcribed (strums,
   one line, or Basic Pitch) and the song plays the transcription. The
   recording is kept and can be heard from the Sketches tab, but never in
   the song.
5. **Alternatives are powerful and hidden.** Laying music over music keeps
   what was there as an alternative, per bar range, but you only see it in
   the Takes lanes of the selected track or as hatching.
6. **Tracks shown twice.** In Arrange, the sidebar's track list and the
   arranger's row labels are the same list; the instrument picker is only
   in the sidebar; there's no mute, solo or volume anywhere.

## A smaller model

Three nouns, used everywhere (UI, agent tools, CLAUDE.md):

- **Sketch**: something you played, as you played it. MIDI or audio,
  immutable, with a name from its music ("Am F C G loop"), a time, a
  tempo and key guess, and for audio, its readings as notes. Every sketch
  is in one library (the journal), however it was captured.
- **Part**: music in the song, on a track, over a bar range. A part made
  from a sketch remembers it ("from: Am F C G loop, 14 Oct 21:40"), so the
  original can be heard or restored (C6, B3). Today's clips are parts.
- **Track**: a line in the band with an instrument. Its parts play;
  versions under them wait.

And one relation:

- **Versions**: when a part is laid over another (recorded over, a sketch
  added, the agent's rewrite), the old one becomes a version of those bars.
  One version per bar range is heard. This replaces "takes" in the UI
  (B2, C5).

### Capturing in this model

- **Listen** stays as is: on, everything you play becomes sketches (C1,
  C2). Nothing goes into the song by itself.
- **Rec** puts a new part on the selected track at the cursor, and also
  files the playing as a sketch in the library, whether or not Listen is
  on. Recording again over the same bars adds a version (C3, C5).
- **Free** stops being its own button: Rec with **Click off** records in
  free time (no count-in, nothing playing, the pulse found afterwards), as
  Free does now (C4). One button, one switch that already means "do I
  hear the click".
- **Audio** follows the selected input: with an audio input chosen in
  Settings, Rec records it too, as an audio sketch, and the part is its
  transcription (C2). Whether the song can play the recording itself is
  decision 1 below.

### Where things are seen

- **Sketches tab** is the library: every sketch, by day, with filters
  All / In this song / Not in the song / Hidden (F1, F2). "Add to song"
  (B1) as now.
- **Arrange** is the song: track headers (name, instrument, mute, solo,
  level) replace the duplicate sidebar list there (B4). Versions show as a
  count on a part ("3 versions"), opened in place.
- **Edit** is one part's notes, with the versions lanes above (renamed
  from Takes) and "Show as played" (renamed from "Show raw take").
- The **agent** gets the same words: `get_session` becomes part of
  `get_sketch` (a sketch in this song), `use_sketch` becomes
  `use_version`, and names like "Jam 09:13" become the sketch's own name.

## Decisions to make

1. **Can a song play audio?** Either (a) audio is always transcribed into
   notes and the recording is only for reference, as today, or (b) a track
   can be an audio track that plays a sketch's recording (a real vocal or
   guitar in the mix, and in exports). (b) means audio in the engine's
   schedule, in `render`, and in the song file or beside it.
2. **Are song files self-contained?** Either (a) a song embeds a copy of
   each sketch it uses (as sessions are embedded now), so the file can be
   moved or shared, or (b) it refers to the library by id, which is
   smaller but tied to this machine's journal. Recommended: (a), keyed by
   sketch id so the library and the song agree on identity.
3. **Merge Free into Rec + Click off?** Recommended: yes.
4. **Versions per bar range** (as now), or whole parts only? Recommended:
   bar ranges; it's what comping needs (C5).
5. **Mute, solo, level per track**: in scope for this work, or after the
   synth (roadmap 5)?
6. **Listen on by default** for new installs? It's off now because a mic
   hears the room; MIDI-only listening could default on.

## Plan

Each phase leaves the app working and is one commit or a few.

1. **Words** (small). UI strings: "Takes" lanes → "Versions", "Show raw
   take" → "Show as played", generated track names from the sketch name
   instead of "Jam hh:mm". Agent: `get_sketch` answers for a song's
   sketches too, with `get_session` kept as an alias for a while. CLAUDE.md
   vocabulary section. (T1)
2. **One library.** Rec writes its playing to the journal like Listen
   does, so every sketch has one id; a song's sessions are keyed by sketch
   id (decision 2); the Sketches tab drops its matching of takes to journal
   sketches and gains "In this song". (C1, F1, F2)
3. **One Rec.** Free folds into Rec with Click off (decision 3); repeated
   Rec over the same bars adds versions; Rec records the chosen audio input
   as an audio sketch with its transcription as the part. (C2–C5)
4. **Track headers.** In Arrange, headers with name, instrument, mute,
   solo, level (decision 5); the sidebar shows tracks only in Edit; parts
   show their version count. (B2, B4)
5. **Audio tracks**, if decision 1 is (b): the engine and `render` play
   recordings, Rec on an audio track keeps the audio as the part.

Not in this plan: the synth (roadmap 5), mixing beyond a level per track,
and editing audio itself (cutting a recording other than by bars).
