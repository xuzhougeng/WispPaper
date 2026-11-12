//! A batch translates one field of a fixed selection. Identical texts share
//! a request, but progress counts papers, including those without that field.

use crate::{ai, model::Paper, store::translation_key};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Abstract,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self { Self::Title => "标题", Self::Abstract => "摘要" }
    }

    fn text(self, paper: &Paper) -> &str {
        match self { Self::Title => &paper.title, Self::Abstract => &paper.abstract_text }
    }
}

#[derive(Clone)]
pub struct Job {
    pub key: String,
    pub text: String,
    pub papers: usize,
}

pub struct Batch {
    pub field: Field,
    pub jobs: VecDeque<Job>,
    /// The first job is already in flight (possibly started by the detail panel).
    pub active: bool,
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
        let mut batch = Self { field, jobs: VecDeque::new(), active: false, total: 0, translated: 0, cached: 0, skipped: 0, failed: 0, stopped: false, error: String::new() };
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

    pub fn done(&self) -> usize { self.translated + self.cached + self.skipped + self.failed }
    pub fn running(&self) -> bool { !self.jobs.is_empty() }

    /// Stop after the request already sent. Its answer is still kept.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.jobs.truncate(usize::from(self.active));
    }

    pub fn finish(&mut self, result: Result<(), String>, cached: bool) {
        let Some(job) = self.jobs.pop_front() else { return };
        self.active = false;
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

    #[test]
    fn shared_texts_count_each_paper_and_failures_do_not_drop_the_rest() {
        let papers = [paper("1", " Same ", ""), paper("2", "Same", ""), paper("3", "Fails", ""), paper("4", "Works", "")];
        let mut batch = Batch::new(&papers, Field::Title, "");
        assert_eq!(batch.jobs.len(), 3);
        batch.finish(Ok(()), true);
        batch.finish(Err("Service unavailable".into()), false);
        assert!(batch.running());
        batch.finish(Ok(()), false);
        assert_eq!((batch.done(), batch.cached, batch.failed, batch.translated), (4, 2, 1, 1));
        assert!(!batch.running());
    }

    #[test]
    fn stopping_keeps_only_the_inflight_result_and_retry_can_replan() {
        let papers = [paper("1", "First", ""), paper("2", "Second", "")];
        let mut batch = Batch::new(&papers, Field::Title, "");
        batch.active = true;
        batch.stop();
        assert!(batch.running());
        assert_eq!(batch.jobs.len(), 1);
        batch.finish(Ok(()), false);
        assert!(batch.stopped && !batch.running());
        assert_eq!((batch.done(), batch.total), (1, 2));
        let mut retry = Batch::new(&papers, Field::Title, "");
        retry.finish(Ok(()), true);
        assert_eq!(retry.jobs[0].text, "Second");
        retry.stop();
        assert!(!retry.running());
    }
}
