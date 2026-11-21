//! 朗读: a PDF read aloud. Each reading has one thread of its own that asks
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

/// Sound fetched and not yet played, with how much of it was played and where each page begins.
#[derive(Default)]
struct Tape {
    samples: VecDeque<f32>,
    played: usize,
    fetched: usize,
    /// The sample each piece begins at, with its page (0 for a selection).
    marks: Vec<(usize, usize)>,
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

/// How a reading stands, as the toolbar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The page being heard; `None` for a selection.
    pub page: Option<usize>,
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
    tape: Arc<Mutex<Tape>>,
}

impl Reading {
    /// Start reading the pieces (page, text) that `pieces` makes on the reading's thread, each asked for
    /// with `ask`. The pages of a selection are 0.
    pub fn start(resource: String, pieces: impl FnOnce() -> Result<Vec<(usize, String)>, String> + Send + 'static, ask: impl Fn(&str) -> Result<HttpRequest, String> + Send + 'static) -> Reading {
        let tape = Arc::new(Mutex::new(Tape::default()));
        let shared = tape.clone();
        std::thread::Builder::new()
            .name("read-aloud".into())
            .spawn(move || run(shared, pieces, ask))
            .ok();
        Reading { resource, tape }
    }

    pub fn status(&self) -> Status {
        let tape = lock(&self.tape);
        let page = tape.marks.iter().rev().find(|(start, _)| *start <= tape.played).or(tape.marks.first()).map(|(_, page)| *page).filter(|page| *page > 0);
        Status { page, waiting: tape.samples.is_empty() && !tape.done, over: tape.over || tape.stopped, error: tape.error.clone() }
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        lock(&self.tape).stopped = true;
    }
}

/// The reading's thread: open the output, fetch the pieces ahead of what is heard, and keep the output
/// open until the last sample is played or the reading is stopped.
fn run(tape: Arc<Mutex<Tape>>, pieces: impl FnOnce() -> Result<Vec<(usize, String)>, String>, ask: impl Fn(&str) -> Result<HttpRequest, String>) {
    let give_up = |error: String| {
        let mut tape = lock(&tape);
        (tape.error, tape.done, tape.over) = (Some(error), true, true);
    };
    let pieces = match pieces() {
        Ok(pieces) if pieces.is_empty() => return give_up(crate::i18n::t("这里没有可朗读的文字。").to_string()),
        Ok(pieces) => pieces,
        Err(error) => return give_up(error),
    };
    let sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            sink
        }
        Err(err) => return give_up(tf!("没有可用的声音输出设备：{}", "No sound output device is available: {}", err)),
    };
    sink.mixer().add(Track { tape: tape.clone(), batch: VecDeque::new() });
    for (page, text) in pieces {
        // Wait while enough is fetched and not yet heard.
        loop {
            let tape = lock(&tape);
            if tape.stopped { return }
            if tape.samples.len() < AHEAD { break }
            drop(tape);
            std::thread::sleep(Duration::from_millis(250));
        }
        let request = match ask(&text) {
            Ok(request) => request,
            Err(error) => {
                lock(&tape).error = Some(error);
                break;
            }
        };
        { let mut tape = lock(&tape); let at = tape.fetched; tape.marks.push((at, page)); }
        let mut pcm = speech::Pcm::default();
        let result = wisp_net::speak(&request, |bytes| {
            let samples = pcm.samples(bytes);
            let mut tape = lock(&tape);
            if tape.stopped { return false }
            tape.fetched += samples.len();
            tape.samples.extend(samples);
            true
        });
        if let Err(error) = result {
            lock(&tape).error = Some(error);
            break;
        }
    }
    lock(&tape).done = true;
    // The output keeps playing what is fetched; the device is let go once it is heard.
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
    use super::{Tape, Track};
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
}
