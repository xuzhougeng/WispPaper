//! 朗读 and 播客: a PDF read aloud, or talked over by two hosts. Each reading has one thread of its own that asks
//! DeepSeek's speech service for its pieces in turn (`wisp_net::speak`) and
//! holds the output device while there is sound to play: rodio's device
//! sink cannot leave the thread it was opened on, and closing it when the
//! reading ends leaves the device alone the rest of the time. What was
//! fetched and not yet played is a `Tape` shared with the output; dropping
//! the `Reading` stops both.

use rodio::{ChannelCount, DeviceSinkBuilder, SampleRate, Source};
use std::collections::VecDeque;
use std::num::NonZero;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use wisp_core::ai::HttpRequest;
use wisp_core::speech::{self, SAMPLE_RATE};

/// How far ahead of what is heard the sound is fetched: enough that a slow answer is not heard, little
/// enough that stopping early leaves little paid for and unplayed.
const AHEAD: usize = SAMPLE_RATE as usize * 40;

/// One request of a reading: what it marks (a page, a podcast's turn; 0 for a selection), the 1-based
/// page its text is on (0 for a podcast's), the voice, the text, and whether a pause comes before it (a new speaker).
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub mark: usize,
    pub page: usize,
    pub voice: String,
    pub text: String,
    pub pause: bool,
}

/// What a reading is of, as its button shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Aloud,
    Podcast,
}

/// What is read: the pieces in turn, as they come (a podcast's are spoken while the rest of its script
/// is still being written; an error ends the reading), and for a podcast its script, kept beside the
/// sound once all of it came.
pub struct Plan {
    pub pieces: Box<dyn Iterator<Item = Result<Piece, String>> + Send>,
    pub transcript: Option<Box<dyn FnOnce() -> String + Send>>,
}

impl Plan {
    /// Pieces known from the start, with no script.
    pub fn of(pieces: Vec<Piece>) -> Plan {
        Plan { pieces: Box::new(pieces.into_iter().map(Ok)), transcript: None }
    }
}

/// Where a piece's sound stands on the tape, and what it says.
struct Mark {
    start: usize,
    /// Where it ends, once all of it came.
    end: Option<usize>,
    mark: usize,
    page: usize,
    voice: String,
    text: String,
}

/// Samples of sound a billed character takes before any piece came whole to tell (`speech::billed`).
const SAMPLES_PER_CHARACTER: f32 = 1_700.;

/// Sound fetched and not yet played, with how much of it was played and where each piece begins.
#[derive(Default)]
struct Tape {
    samples: VecDeque<f32>,
    played: usize,
    fetched: usize,
    /// Each piece in turn, from the sample it begins at.
    marks: Vec<Mark>,
    /// Every byte fetched, while it is kept (a podcast), and its script; handed out once when all came.
    recording: Option<Vec<u8>>,
    transcript: Option<String>,
    recorded: bool,
    /// Every piece is fetched, or fetching ended with `error`.
    done: bool,
    stopped: bool,
    /// The output has given out its last sample.
    over: bool,
    error: Option<String>,
}

fn lock(tape: &Mutex<Tape>) -> MutexGuard<'_, Tape> {
    tape.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the output plays: the tape, taken in batches so the audio thread does not lock for every sample.
/// While the next piece is on its way it plays silence; it ends when the tape is done and played, or stopped.
struct Track {
    tape: Arc<Mutex<Tape>>,
    batch: VecDeque<f32>,
}

impl Iterator for Track {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if let Some(sample) = self.batch.pop_front() { return Some(sample) }
        let mut tape = lock(&self.tape);
        if tape.stopped { return None }
        let take = tape.samples.len().min(1024);
        if take == 0 {
            if tape.done {
                tape.over = true;
                return None;
            }
            return Some(0.);
        }
        self.batch.extend(tape.samples.drain(..take));
        tape.played += take;
        self.batch.pop_front()
    }
}

impl Source for Track {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        NonZero::new(1).unwrap()
    }

    fn sample_rate(&self) -> SampleRate {
        NonZero::new(SAMPLE_RATE).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// The sentence being heard: the 1-based page it is on (0 for a podcast's), the voice, and the sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub page: usize,
    pub voice: String,
    pub sentence: String,
}

/// How a reading stands, as the toolbar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The page or turn being heard; `None` for a selection, or before the first sound.
    pub mark: Option<usize>,
    /// The sentence heard, followed on the page; `None` before the first sound.
    pub heard: Option<Heard>,
    /// All of a podcast's sound came: `Reading::take_recording` has it.
    pub recorded: bool,
    /// Nothing to play yet: the first sound, or the next piece, is on its way.
    pub waiting: bool,
    /// Everything was heard, or the reading broke off (`error`).
    pub over: bool,
    pub error: Option<String>,
}

/// One reading of a file: a selection, or its pages from one on.
pub struct Reading {
    /// The file read (`PaperFile::id`): the reading belongs to the viewer showing it.
    pub resource: String,
    pub kind: Kind,
    tape: Arc<Mutex<Tape>>,
    /// A kept podcast played again: its sound's attachment (`PaperFile::id`), which the playlist goes on from.
    pub replay: Option<String>,
}

impl Reading {
    /// Play a kept podcast again from its files: the WAV, and the script beside it when there is one, so
    /// the turn heard and its words show as they did the first time. Nothing is asked of any service.
    pub fn replay(resource: String, sound: String, wav: std::path::PathBuf, script: Option<std::path::PathBuf>) -> Reading {
        let tape = Arc::new(Mutex::new(Tape::default()));
        let shared = tape.clone();
        std::thread::Builder::new().name("read-aloud".into()).spawn(move || replay(shared, wav, script)).ok();
        Reading { resource, kind: Kind::Podcast, tape, replay: Some(sound) }
    }

    /// Start reading the pieces that `plan` makes on the reading's thread (a podcast's script is written
    /// there too), each asked for with `ask` (voice, text). A podcast is fetched whole and kept; a
    /// reading only `AHEAD` of what is heard.
    pub fn start(resource: String, kind: Kind, plan: impl FnOnce() -> Result<Plan, String> + Send + 'static, ask: impl Fn(&str, &str) -> Result<HttpRequest, String> + Send + 'static) -> Reading {
        let tape = Arc::new(Mutex::new(Tape { recording: (kind == Kind::Podcast).then(Vec::new), ..Tape::default() }));
        let shared = tape.clone();
        let ahead = if kind == Kind::Podcast { usize::MAX } else { AHEAD };
        std::thread::Builder::new()
            .name("read-aloud".into())
            .spawn(move || run(shared, ahead, plan, ask))
            .ok();
        Reading { resource, kind, tape, replay: None }
    }

    pub fn status(&self) -> Status {
        let tape = lock(&self.tape);
        let current = tape.marks.iter().rev().find(|m| m.start <= tape.played).or(tape.marks.first());
        let mark = current.map(|m| m.mark).filter(|mark| *mark > 0);
        let heard = current.filter(|_| tape.played > 0).and_then(|m| heard(&tape, m));
        Status { mark, heard, recorded: tape.recorded, waiting: tape.samples.is_empty() && !tape.done, over: tape.over || tape.stopped, error: tape.error.clone() }
    }

    /// A podcast's sound (16-bit PCM bytes) and script once all of it came; `None` before, and after it was taken.
    pub fn take_recording(&self) -> Option<(Vec<u8>, String)> {
        let mut tape = lock(&self.tape);
        if !tape.recorded { return None }
        tape.recorded = false;
        Some((tape.recording.take()?, tape.transcript.take().unwrap_or_default()))
    }
}

/// The sentence of a piece being heard: the piece's sound shared among its sentences by their length.
/// A piece still coming is as long as its text is likely to sound, by the pieces that came whole.
fn heard(tape: &Tape, piece: &Mark) -> Option<Heard> {
    let length = piece.end.map(|end| end - piece.start).unwrap_or_else(|| {
        let (sound, said) = tape.marks.iter().filter_map(|m| Some((m.end? - m.start, speech::billed(&m.text)))).fold((0, 0), |(sound, said), (s, b)| (sound + s, said + b));
        let rate = if said > 0 { sound as f32 / said as f32 } else { SAMPLES_PER_CHARACTER };
        ((speech::billed(&piece.text) as f32 * rate) as usize).max(tape.fetched.saturating_sub(piece.start))
    });
    let at = tape.played.saturating_sub(piece.start) as f32 / length.max(1) as f32;
    let sentence = speech::sentence_at(&piece.text, at)?;
    Some(Heard { page: piece.page, voice: piece.voice.clone(), sentence: sentence.to_string() })
}

impl Drop for Reading {
    fn drop(&mut self) {
        lock(&self.tape).stopped = true;
    }
}

/// The reading's thread: open the output, fetch the pieces ahead of what is heard, and keep the output
/// open until the last sample is played or the reading is stopped.
fn run(tape: Arc<Mutex<Tape>>, ahead: usize, plan: impl FnOnce() -> Result<Plan, String>, ask: impl Fn(&str, &str) -> Result<HttpRequest, String>) {
    let give_up = |error: String| {
        let mut tape = lock(&tape);
        (tape.error, tape.done, tape.over) = (Some(error), true, true);
    };
    let mut plan = match plan() {
        Ok(plan) => plan,
        Err(error) => return give_up(error),
    };
    // The first piece before the output is opened: nothing to read leaves the device alone.
    let mut pieces = plan.pieces.peekable();
    match pieces.peek() {
        None => return give_up(crate::i18n::t("这里没有可朗读的文字。").to_string()),
        Some(Err(error)) => return give_up(error.clone()),
        Some(Ok(_)) => {}
    }
    if lock(&tape).stopped { return }
    let sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            sink
        }
        Err(err) => return give_up(tf!("没有可用的声音输出设备：{}", "No sound output device is available: {}", err)),
    };
    sink.mixer().add(Track { tape: tape.clone(), batch: VecDeque::new() });
    let mut whole = true;
    for (index, piece) in pieces.enumerate() {
        let piece = match piece {
            Ok(piece) => piece,
            Err(error) => {
                lock(&tape).error = Some(error);
                whole = false;
                break;
            }
        };
        // Wait while enough is fetched and not yet heard.
        loop {
            let tape = lock(&tape);
            if tape.stopped { return }
            if tape.samples.len() < ahead { break }
            drop(tape);
            std::thread::sleep(Duration::from_millis(250));
        }
        let request = match ask(&piece.voice, &piece.text) {
            Ok(request) => request,
            Err(error) => {
                lock(&tape).error = Some(error);
                whole = false;
                break;
            }
        };
        {
            let mut tape = lock(&tape);
            // A breath between two speakers, heard and kept.
            if piece.pause && index > 0 {
                tape.samples.extend(std::iter::repeat_n(0., wisp_core::podcast::PAUSE));
                tape.fetched += wisp_core::podcast::PAUSE;
                if let Some(recording) = tape.recording.as_mut() { recording.extend(std::iter::repeat_n(0u8, wisp_core::podcast::PAUSE * 2)) }
            }
            let start = tape.fetched;
            tape.marks.push(Mark { start, end: None, mark: piece.mark, page: piece.page, voice: piece.voice.clone(), text: piece.text.clone() });
        }
        let mut pcm = speech::Pcm::default();
        let result = wisp_net::speak(&request, |bytes| {
            let samples = pcm.samples(bytes);
            let mut tape = lock(&tape);
            if tape.stopped { return false }
            tape.fetched += samples.len();
            tape.samples.extend(samples);
            if let Some(recording) = tape.recording.as_mut() { recording.extend_from_slice(bytes) }
            true
        });
        if let Err(error) = result {
            lock(&tape).error = Some(error);
            whole = false;
            break;
        }
        let mut tape = lock(&tape);
        let end = tape.fetched;
        if let Some(last) = tape.marks.last_mut() { last.end = Some(end) }
    }
    {
        let mut tape = lock(&tape);
        tape.done = true;
        // A podcast that came whole is kept, even when the listener stops before its end.
        tape.recorded = whole && !tape.stopped && tape.recording.as_ref().is_some_and(|r| !r.is_empty());
        if tape.recorded { tape.transcript = plan.transcript.take().map(|write| write()) }
    }
    // The output keeps playing what is fetched; the device is let go once it is heard.
    loop {
        let tape = lock(&tape);
        if tape.stopped || tape.over { break }
        drop(tape);
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(sink);
}

/// A kept podcast's thread: read its sound and script onto the tape, all at once, and play it out.
fn replay(tape: Arc<Mutex<Tape>>, wav: std::path::PathBuf, script: Option<std::path::PathBuf>) {
    use wisp_core::podcast;
    let give_up = |error: String| {
        let mut tape = lock(&tape);
        (tape.error, tape.done, tape.over) = (Some(error), true, true);
    };
    let bytes = match std::fs::read(&wav) {
        Ok(bytes) => bytes,
        Err(error) => return give_up(tf!("读不出播客的声音文件：{}", "Could not read the podcast's sound file: {}", error)),
    };
    let samples = speech::Pcm::default().samples(podcast::pcm_of(&bytes).unwrap_or_default());
    if samples.is_empty() { return give_up(crate::i18n::t("这个播客的声音文件是空的。").to_string()) }
    // Without its script the sound still plays, only the turn heard is not known.
    let turns = script.and_then(|path| std::fs::read_to_string(path).ok()).and_then(|text| podcast::script(&text).ok()).unwrap_or_default();
    let marks = podcast::timings(&turns, samples.len()).into_iter().zip(&turns).enumerate().map(|(index, ((start, end), turn))| Mark { start, end: Some(end), mark: index + 1, page: 0, voice: podcast::HOSTS[turn.host].2.to_string(), text: turn.text.clone() }).collect();
    {
        let mut tape = lock(&tape);
        if tape.stopped { return }
        tape.fetched = samples.len();
        tape.samples = samples.into();
        (tape.marks, tape.done) = (marks, true);
    }
    let sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            sink
        }
        Err(err) => return give_up(tf!("没有可用的声音输出设备：{}", "No sound output device is available: {}", err)),
    };
    sink.mixer().add(Track { tape: tape.clone(), batch: VecDeque::new() });
    loop {
        let tape = lock(&tape);
        if tape.stopped || tape.over { break }
        drop(tape);
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(sink);
}

#[cfg(test)]
mod tests {
    use super::{Mark, Reading, Tape, Track};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_track_plays_silence_while_waiting_and_ends_with_the_tape() {
        let tape = Arc::new(Mutex::new(Tape::default()));
        let mut track = Track { tape: tape.clone(), batch: VecDeque::new() };
        assert_eq!(track.next(), Some(0.));
        tape.lock().unwrap().samples.extend([0.5, 0.25]);
        assert_eq!((track.next(), track.next()), (Some(0.5), Some(0.25)));
        assert_eq!(tape.lock().unwrap().played, 2);
        tape.lock().unwrap().done = true;
        assert_eq!(track.next(), None);
        assert!(tape.lock().unwrap().over);
    }

    #[test]
    fn the_sentence_heard_is_found_by_the_time_played() {
        let piece = |start, end, text: &str| Mark { start, end, mark: 3, page: 3, voice: "stella".into(), text: text.into() };
        let tape = Tape { fetched: 120, marks: vec![piece(0, Some(100), "Aaaa. Bbbb."), piece(100, None, "Cccc. Dddd.")], ..Tape::default() };
        let reading = Reading { resource: "R".into(), kind: super::Kind::Aloud, tape: Arc::new(Mutex::new(tape)), replay: None };
        assert_eq!(reading.status().heard, None);
        let at = |played| {
            reading.tape.lock().unwrap().played = played;
            reading.status().heard.map(|heard| heard.sentence)
        };
        assert_eq!(at(10).as_deref(), Some("Aaaa."));
        assert_eq!(at(70).as_deref(), Some("Bbbb."));
        // The piece still coming is as long as the one before for as much text.
        assert_eq!(at(130).as_deref(), Some("Cccc."));
        assert_eq!(at(170).as_deref(), Some("Dddd."));
        assert_eq!(reading.status().heard.unwrap().page, 3);
    }
}
