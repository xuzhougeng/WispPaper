//! A batch translates one field of a fixed selection. Identical texts share
//! a request, but progress counts papers, including those without that field.
//! A paper's full text is a batch of its own (`passages`, `Batch::texts`):
//! one text per paragraph, counted in paragraphs, each kept under its own
//! `translation_key`, so a run stopped halfway goes on from what is kept.

use crate::{ai, model::Paper, pdftext, store::translation_key, structure};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Abstract,
    /// The paragraphs of one paper's full text (`Batch::texts`): no field of a record.
    Body,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self { Self::Title => "标题", Self::Abstract => "摘要", Self::Body => "全文" }
    }

    fn text(self, paper: &Paper) -> &str {
        match self { Self::Title => &paper.title, Self::Abstract => &paper.abstract_text, Self::Body => "" }
    }
}

/// A paragraph or a heading of a paper as one page shows it: what is
/// translated, and where the translation is set in its place.
#[derive(Clone, Debug, PartialEq)]
pub struct Passage {
    /// The 1-based page it is on.
    pub page: usize,
    /// The box it stands in: `[left, top, right, bottom]`, points from the page's top left.
    pub rect: [f32; 4],
    /// The size it is set in.
    pub size: f32,
    pub text: String,
    pub heading: bool,
}

/// What a reference list is headed with, as `reference_list` compares it.
const REFERENCE_LISTS: [&str; 16] = ["references", "reference", "literature cited", "bibliography", "literatur", "literaturverzeichnis", "références", "bibliographie", "referencias", "bibliografía", "referências", "bibliografia", "参考文献", "參考文獻", "引用文献", "참고문헌"];

/// Whether a heading opens the list of references: `References`, `7. REFERENCES`, `参考文献`.
fn reference_list(heading: &str) -> bool {
    let name: String = heading.to_lowercase().chars().map(|c| if c.is_alphabetic() { c } else { ' ' }).collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    REFERENCE_LISTS.iter().any(|list| name == *list || name.strip_suffix(list).is_some_and(|before| before.ends_with(' ')))
}

/// Running text, as against the letters of a figure (`B C D E`, `batlow`,
/// `1.5 0.5`): five words of three letters or more, or a dozen Chinese,
/// Japanese or Korean characters.
fn wordy(text: &str) -> bool {
    text.split_whitespace().filter(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 3).count() >= 5 || text.chars().filter(|c| structure::is_cjk(*c)).count() >= 12
}

/// What of a paper's pages is translated, page by page in the order each
/// is drawn: the paragraphs and headings as the page shows them
/// (`pdftext::page_paragraphs`, given the glyphs of every page), each a
/// request of its own, so only what is worth one. Left out are the list of
/// references, which reads the same in any language (it ends where a part
/// of the paper begins again: the methods after the references, Nature's
/// way), what comes back on three pages or more (running headers and
/// footers), the words standing in figures (`wordy`), and a short line that
/// is neither the name of a part of the paper nor set apart (bold, or
/// larger than the text) over a paragraph: a label of a figure.
pub fn passages(pages: &[Vec<pdftext::Mark>]) -> Vec<Passage> {
    let paragraphs: Vec<(usize, pdftext::Paragraph)> = pages.iter().enumerate().flat_map(|(index, marks)| pdftext::page_paragraphs(marks).into_iter().map(move |paragraph| (index + 1, paragraph))).collect();
    // A paragraph with its digits blanked, and the pages it is on.
    let shape = |text: &str| text.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().to_lowercase();
    let mut seen = HashMap::<String, HashSet<usize>>::new();
    for (page, paragraph) in &paragraphs { seen.entry(shape(&paragraph.text)).or_default().insert(*page); }
    // The size most of the text is set in.
    let mut sizes = HashMap::<i32, usize>::new();
    for (_, paragraph) in &paragraphs { *sizes.entry((paragraph.size * 2.).round() as i32).or_default() += paragraph.text.len() }
    let body = sizes.into_iter().max_by_key(|(_, bytes)| *bytes).map_or(10., |(size, _)| size as f32 / 2.);
    let prose = |at: usize| paragraphs.get(at).is_some_and(|(_, paragraph)| paragraph.lines >= 2 && wordy(&paragraph.text));
    let mut listing = false;
    let mut out = vec![];
    for (index, (page, paragraph)) in paragraphs.iter().enumerate() {
        let text = paragraph.text.trim();
        let short = paragraph.lines <= 2 && text.chars().count() <= 120 && !text.ends_with(['.', '。', ',', '，', ';', '；']);
        let part = if short { structure::canonical(text) } else { None };
        if short && reference_list(text) { listing = true } else if part.is_some_and(|part| part != structure::SKIP) { listing = false }
        if listing || seen[&shape(&paragraph.text)].len() >= 3 || text.chars().filter(|c| c.is_alphabetic()).count() < 2 { continue }
        let apart = paragraph.bold || paragraph.size > body * 1.12;
        let heading = short && (part.is_some() || (apart && (prose(index + 1) || prose(index + 2))));
        if heading || wordy(text) { out.push(Passage { page: *page, rect: paragraph.rect, size: paragraph.size, text: text.to_string(), heading }) }
    }
    out
}

/// How wide a text runs when set at size 1: a Chinese, Japanese or Korean
/// character takes a square, a letter about half of one.
fn ems(text: &str) -> f32 {
    text.chars().map(|c| if structure::is_cjk(c) || ('\u{3000}'..='\u{303f}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c) { 1. } else { 0.55 }).sum()
}

/// How far apart the lines of a translation are set, by its size.
pub const LEADING: f32 = 1.3;

/// The size a translation is set in so that it stands in the box of the
/// paragraph it replaces (`width` by `height`, in points): the size of the
/// paragraph where it fits, else the largest that does, and never under
/// `SMALLEST`, where it is cut off instead of being too small to read.
pub fn fit(text: &str, width: f32, height: f32, size: f32) -> f32 {
    let ems = ems(text);
    let mut at = size.max(SMALLEST);
    while at > SMALLEST {
        // Lines break at words, so none is filled to its end.
        let lines = (ems * at * 1.05 / width.max(1.)).ceil().max(1.);
        // A line of the page is given a little more than its letters take.
        if lines * at * LEADING <= height.max(size * LEADING) + at * 0.3 { break }
        at -= 0.25;
    }
    at.max(SMALLEST)
}

/// The smallest size a translation is set in, in points.
pub const SMALLEST: f32 = 4.5;

#[derive(Clone)]
pub struct Job {
    pub key: String,
    pub text: String,
    pub papers: usize,
}

pub struct Batch {
    pub field: Field,
    /// Waiting to be sent, in the order of the selection.
    pub jobs: VecDeque<Job>,
    /// In flight: sent by this batch, or already asked for by the detail panel.
    pub working: Vec<Job>,
    pub total: usize,
    pub translated: usize,
    pub cached: usize,
    pub skipped: usize,
    pub failed: usize,
    pub stopped: bool,
    pub error: String,
}

impl Batch {
    pub fn new<'a>(papers: impl IntoIterator<Item = &'a Paper>, field: Field, prompt: &str) -> Self {
        let mut batch = Self { field, jobs: VecDeque::new(), working: vec![], total: 0, translated: 0, cached: 0, skipped: 0, failed: 0, stopped: false, error: String::new() };
        let mut ids = HashSet::new();
        let mut positions = HashMap::<String, usize>::new();
        for paper in papers {
            if !ids.insert(&paper.id) { continue }
            batch.total += 1;
            let text = field.text(paper).trim();
            if text.is_empty() { batch.skipped += 1; continue }
            let key = translation_key(text, ai::translation_prompt(prompt));
            if let Some(&at) = positions.get(&key) {
                batch.jobs[at].papers += 1;
            } else {
                positions.insert(key.clone(), batch.jobs.len());
                batch.jobs.push_back(Job { key, text: text.to_string(), papers: 1 });
            }
        }
        batch
    }

    /// A batch of texts in the order given, each counted once: the
    /// paragraphs of a full text. Those said twice share a request.
    pub fn texts<'a>(texts: impl IntoIterator<Item = &'a str>, prompt: &str) -> Self {
        let mut batch = Self { field: Field::Body, jobs: VecDeque::new(), working: vec![], total: 0, translated: 0, cached: 0, skipped: 0, failed: 0, stopped: false, error: String::new() };
        let mut positions = HashMap::<String, usize>::new();
        for text in texts {
            batch.total += 1;
            let text = text.trim();
            if text.is_empty() { batch.skipped += 1; continue }
            let key = translation_key(text, ai::translation_prompt(prompt));
            if let Some(&at) = positions.get(&key) {
                batch.jobs[at].papers += 1;
            } else {
                positions.insert(key.clone(), batch.jobs.len());
                batch.jobs.push_back(Job { key, text: text.to_string(), papers: 1 });
            }
        }
        batch
    }

    pub fn done(&self) -> usize { self.translated + self.cached + self.skipped + self.failed }
    pub fn running(&self) -> bool { !self.jobs.is_empty() || !self.working.is_empty() }

    /// The next text to ask for, counted as in flight until `finish`.
    pub fn take(&mut self) -> Option<Job> {
        let job = self.jobs.pop_front()?;
        self.working.push(job.clone());
        Some(job)
    }

    /// Send nothing more. The answers to the requests already sent are still kept.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.jobs.clear();
    }

    pub fn finish(&mut self, key: &str, result: Result<(), String>, cached: bool) {
        let Some(at) = self.working.iter().position(|job| job.key == key) else { return };
        let job = self.working.remove(at);
        match result {
            Ok(()) if cached => self.cached += job.papers,
            Ok(()) => self.translated += job.papers,
            Err(error) => {
                self.failed += job.papers;
                self.error = error;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, title: &str, abstract_text: &str) -> Paper {
        Paper { id: id.into(), title: title.into(), abstract_text: abstract_text.into(), ..Default::default() }
    }

    #[test]
    fn only_the_requested_field_of_the_selection_is_translated() {
        let papers = [paper("1", " First title ", "One abstract"), paper("2", "Second title", "  "), paper("3", "Not selected", "Not selected")];
        let titles = Batch::new([&papers[0], &papers[1], &papers[0]], Field::Title, "");
        assert_eq!((titles.total, titles.skipped, titles.jobs.len()), (2, 0, 2));
        assert_eq!(titles.jobs.iter().map(|job| job.text.as_str()).collect::<Vec<_>>(), ["First title", "Second title"]);
        let abstracts = Batch::new([&papers[0], &papers[1]], Field::Abstract, "");
        assert_eq!((abstracts.total, abstracts.skipped, abstracts.jobs.len()), (2, 1, 1));
        assert_eq!(abstracts.jobs[0].text, "One abstract");
        assert_eq!(abstracts.jobs[0].key, translation_key("One abstract", ai::TRANSLATION_PROMPT));
        let other = Batch::new([&papers[0]], Field::Abstract, "Translate to French");
        assert_ne!(abstracts.jobs[0].key, other.jobs[0].key);
    }

    /// A line of text set on a page: one mark a letter, 5 points wide.
    fn line(text: &str, x: f32, y: f32, size: f32, bold: bool) -> Vec<pdftext::Mark> {
        text.chars().enumerate().map(|(i, c)| pdftext::Mark { text: c.to_string(), x0: x + i as f32 * 5., x1: x + (i + 1) as f32 * 5., y, size, bold }).collect()
    }

    #[test]
    fn a_full_text_is_the_paragraphs_of_its_pages_without_figures_and_references() {
        let sentence = "Roots grow more slowly in compacted soil because ethylene stays.";
        let footer = |page: usize| line(&format!("JOURNAL OF ROOTS | (2020) 11:5444 | www.example.org/roots {page}"), 50., 800., 7., false);
        let paragraph = |y: f32| [line(sentence, 50., y, 10., false), line(sentence, 50., y + 12., 10., false), line("It does so here.", 50., y + 24., 10., false)].concat();
        let pages = [
            [line("A study of roots in compacted soil", 50., 60., 18., true), line("Introduction", 50., 100., 10., true), paragraph(120.), line("B C D E F G H I", 300., 300., 6., false), line("batlow", 300., 340., 6., false), footer(1),
                line("References", 50., 500., 10., true), line("1. Smith J. Roots and soil in the field. Nature 1, 1-2 (2020).", 50., 520., 8., false)].concat(),
            [line("3. Wang Q. More on roots and soil in pots. Science 6, 7-8 (2022).", 50., 60., 8., false), line("Methods", 50., 100., 10., true), paragraph(120.), footer(2)].concat(),
            [line("根系在紧实土壤中生长变慢，因为乙烯无法离开组织。", 50., 60., 10., false), footer(3)].concat(),
        ];
        let found = passages(&pages);
        let body = format!("{sentence} {sentence} It does so here.");
        let texts: Vec<&str> = found.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["A study of roots in compacted soil", "Introduction", body.as_str(), "Methods", body.as_str(), "根系在紧实土壤中生长变慢，因为乙烯无法离开组织。"]);
        assert_eq!(found.iter().map(|p| (p.page, p.heading)).collect::<Vec<_>>(), [(1, true), (1, true), (1, false), (2, true), (2, false), (3, false)]);
        // Each stands where the page sets it: three lines of ten points from the first's capitals to the last's descenders.
        assert_eq!(found[2].rect, [50., 112., 50. + 5. * sentence.len() as f32, 146.5]);
        assert!(reference_list("7. REFERENCES") && reference_list("参考文献") && !reference_list("Cross-references in the text"));
        // Each paragraph is a text of its own; the two that say the same share one request.
        let mut batch = Batch::texts(texts.iter().copied().chain(["  "]), "");
        assert_eq!((batch.field, batch.total, batch.skipped, batch.jobs.len()), (Field::Body, 7, 1, 5));
        assert_eq!(batch.jobs[2].papers, 2);
        let job = batch.take().unwrap();
        assert_eq!(job.key, translation_key("A study of roots in compacted soil", ai::TRANSLATION_PROMPT));
        batch.finish(&job.key, Ok(()), true);
        assert_eq!((batch.done(), batch.cached), (2, 1));
    }

    #[test]
    fn a_translation_is_set_to_stand_in_the_paragraphs_box() {
        // Chinese says it in less room: the paragraph's own size.
        assert_eq!(fit(&"根".repeat(60), 240., 60., 10.), 10.);
        // More than the box holds at that size: smaller, until it stands in it.
        let size = fit(&"根".repeat(200), 240., 60., 10.);
        assert!(size < 10. && size > SMALLEST, "{size}");
        assert!((200. * size * 1.05 / 240.).ceil() * size * LEADING <= 60. + size * 0.3);
        // A heading's one line keeps its size; what cannot fit is cut off, not set unreadably small.
        assert_eq!(fit("方法", 60., 12.5, 10.), 10.);
        assert_eq!(fit(&"根".repeat(5000), 240., 60., 10.), SMALLEST);
    }

    #[test]
    fn shared_texts_count_each_paper_and_failures_do_not_drop_the_rest() {
        let papers = [paper("1", " Same ", ""), paper("2", "Same", ""), paper("3", "Fails", ""), paper("4", "Works", "")];
        let mut batch = Batch::new(&papers, Field::Title, "");
        assert_eq!(batch.jobs.len(), 3);
        let jobs: Vec<Job> = std::iter::from_fn(|| batch.take()).collect();
        assert_eq!((jobs.len(), batch.working.len()), (3, 3));
        // Answers come back in any order.
        batch.finish(&jobs[1].key, Err("Service unavailable".into()), false);
        batch.finish(&jobs[0].key, Ok(()), true);
        assert!(batch.running());
        batch.finish(&jobs[1].key, Ok(()), false);
        assert_eq!(batch.failed, 1);
        batch.finish(&jobs[2].key, Ok(()), false);
        assert_eq!((batch.done(), batch.cached, batch.failed, batch.translated), (4, 2, 1, 1));
        assert!(!batch.running());
    }

    #[test]
    fn stopping_keeps_only_the_inflight_results_and_retry_can_replan() {
        let papers = [paper("1", "First", ""), paper("2", "Second", ""), paper("3", "Third", "")];
        let mut batch = Batch::new(&papers, Field::Title, "");
        let (first, second) = (batch.take().unwrap(), batch.take().unwrap());
        batch.stop();
        assert!(batch.running());
        assert_eq!((batch.jobs.len(), batch.working.len()), (0, 2));
        assert!(batch.take().is_none());
        batch.finish(&second.key, Ok(()), false);
        assert!(batch.running());
        batch.finish(&first.key, Ok(()), false);
        assert!(batch.stopped && !batch.running());
        assert_eq!((batch.done(), batch.total), (2, 3));
        let mut retry = Batch::new(&papers[..2], Field::Title, "");
        let first = retry.take().unwrap();
        retry.finish(&first.key, Ok(()), true);
        assert_eq!(retry.jobs[0].text, "Second");
        retry.stop();
        assert!(!retry.running());
    }
}
