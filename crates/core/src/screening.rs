//! The person's side of 筛选: the keys that decide a paper, a round of
//! papers looked at one by one, and the suggestions confirmed together.
//! Nothing here asks a model or writes a library: the window
//! (`src/screen.rs`, `screen_single.rs`, `screen_confirm.rs`) draws these and
//! writes what the person decided.

use crate::model::{Paper, SCREENINGS};
use crate::store::Suggestion;
use std::collections::{BTreeMap, BTreeSet};

/// The four decisions in the order their buttons stand: the status, its letter, its digit.
pub const DECISIONS: [(&str, &str, &str); 4] = [("included", "y", "1"), ("excluded", "r", "2"), ("uncertain", "u", "3"), ("irrelevant", "n", "4")];

/// The status a key stands for: a letter or digit of `DECISIONS`, and `p` or `0` for taking a decision back.
pub fn status_of(key: &str) -> Option<&'static str> {
    if matches!(key, "p" | "0") { return Some("pending") }
    DECISIONS.iter().find(|(_, letter, digit)| *letter == key || *digit == key).map(|(status, _, _)| *status)
}

/// The letter on a decision's button, as it is shown.
pub fn key_of(status: &str) -> &'static str {
    match status {
        "included" => "Y",
        "excluded" => "R",
        "uncertain" => "U",
        "irrelevant" => "N",
        _ => "P",
    }
}

/// The reason kept with a decision: the assistant's when the person agrees
/// with it, the person's own when the decision stays what it was, and none
/// when the person decides otherwise (that reason is theirs to give).
pub fn reason_for(paper: &Paper, suggestion: Option<&Suggestion>, status: &str) -> String {
    if let Some(s) = suggestion.filter(|s| s.verdict == status) { return s.reason.clone() }
    if paper.screening == status { return paper.screening_reason.clone() }
    String::new()
}

/// How many papers have each status, in the order of `SCREENINGS` (待判断 first).
pub fn tally<'a>(statuses: impl Iterator<Item = &'a str>) -> [usize; 5] {
    let mut counts = [0; 5];
    for status in statuses {
        if let Some(at) = SCREENINGS.iter().position(|(key, _)| *key == status) { counts[at] += 1 }
    }
    counts
}

/// What a paper was before a decision, so the decision can be taken back.
#[derive(Clone, Debug, PartialEq)]
pub struct Before {
    pub paper_id: String,
    pub screening: String,
    pub reason: String,
    pub reviewed_at: String,
}

impl Before {
    pub fn of(paper: &Paper) -> Before {
        Before { paper_id: paper.id.clone(), screening: paper.screening.clone(), reason: paper.screening_reason.clone(), reviewed_at: paper.reviewed_at.clone() }
    }

    /// Make the paper what it was.
    pub fn restore(&self, paper: &mut Paper) {
        (paper.screening, paper.screening_reason, paper.reviewed_at) = (self.screening.clone(), self.reason.clone(), self.reviewed_at.clone());
    }
}

/// 逐篇: papers looked at one at a time. The list is fixed when the round
/// starts, so a decision neither reorders it nor takes the paper away: the
/// person can go back and decide otherwise.
#[derive(Clone, Debug, Default)]
pub struct Round {
    pub ids: Vec<String>,
    /// The paper on show; `ids.len()` is the end, where the round is summed up.
    pub at: usize,
    /// The decisions of this round, the last one last.
    made: Vec<Before>,
}

impl Round {
    /// A round over `ids`, opening on `start` when it is among them.
    pub fn new(ids: Vec<String>, start: Option<&str>) -> Round {
        let at = start.and_then(|id| ids.iter().position(|other| other == id)).unwrap_or(0);
        Round { ids, at, made: vec![] }
    }

    pub fn current(&self) -> Option<&str> {
        self.ids.get(self.at).map(String::as_str)
    }

    /// Past the last paper.
    pub fn ended(&self) -> bool {
        self.at >= self.ids.len()
    }

    pub fn go(&mut self, at: usize) {
        self.at = at.min(self.ids.len());
    }

    /// One paper on or back; on from the last one is the end.
    pub fn step(&mut self, forward: bool) {
        self.go(if forward { self.at + 1 } else { self.at.saturating_sub(1) });
    }

    /// A decision was made on a paper of the round. Made on the one on show, the next comes up.
    pub fn decided(&mut self, before: Before) {
        if self.current() == Some(before.paper_id.as_str()) { self.go(self.at + 1) }
        self.made.push(before);
    }

    pub fn can_undo(&self) -> bool {
        !self.made.is_empty()
    }

    /// Take the last decision back: what the paper was before it, with that paper on show again.
    pub fn undo(&mut self) -> Option<Before> {
        let before = self.made.pop()?;
        if let Some(at) = self.ids.iter().position(|id| *id == before.paper_id) { self.at = at }
        Some(before)
    }

    /// Papers that left the library leave the round; the paper on show stays on show.
    pub fn keep(&mut self, held: impl Fn(&str) -> bool) {
        if self.ids.iter().all(|id| held(id)) { return }
        let before = self.ids[..self.at.min(self.ids.len())].iter().filter(|id| held(id)).count();
        self.ids.retain(|id| held(id));
        self.made.retain(|made| held(&made.paper_id));
        self.at = before;
    }

    /// The first paper from the top for which `wanted` holds.
    pub fn first(&self, wanted: impl Fn(&str) -> bool) -> Option<usize> {
        self.ids.iter().position(|id| wanted(id))
    }
}

/// One paper of what a confirmation writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub paper_id: String,
    pub status: String,
    pub reason: String,
}

/// 确认: the assistant's suggestions for the waiting papers, looked over as
/// a list and written together. Nothing is written before the person says
/// so; until then a paper can be given another status or left out.
#[derive(Clone, Debug, Default)]
pub struct Confirming {
    /// The status the person chose for a paper, where it is not the suggested one.
    choice: BTreeMap<String, String>,
    /// Papers left out: they stay waiting.
    skipped: BTreeSet<String>,
    /// The suggested verdict on show; empty for all of them.
    pub part: String,
    /// The suggestions the assistant was least sure of come first.
    pub doubtful_first: bool,
    /// The paper the keys act on.
    pub cursor: Option<String>,
}

impl Confirming {
    /// What the paper becomes when confirmed: the person's choice, else the suggestion.
    pub fn status<'a>(&'a self, suggestion: &'a Suggestion) -> &'a str {
        self.choice.get(&suggestion.paper_id).map_or(suggestion.verdict.as_str(), String::as_str)
    }

    /// Whether the person chose otherwise than the assistant suggested.
    pub fn changed(&self, paper_id: &str) -> bool {
        self.choice.contains_key(paper_id)
    }

    pub fn ticked(&self, paper_id: &str) -> bool {
        !self.skipped.contains(paper_id)
    }

    /// Give a paper a status; choosing one counts it in again.
    pub fn choose(&mut self, suggestion: &Suggestion, status: &str) {
        if status == suggestion.verdict { self.choice.remove(&suggestion.paper_id); } else { self.choice.insert(suggestion.paper_id.clone(), status.to_string()); }
        self.skipped.remove(&suggestion.paper_id);
    }

    pub fn toggle(&mut self, paper_id: &str) {
        if !self.skipped.remove(paper_id) { self.skipped.insert(paper_id.to_string()); }
    }

    /// Count these papers in, or leave them all out.
    pub fn tick_all<'a>(&mut self, ids: impl Iterator<Item = &'a str>, on: bool) {
        for id in ids {
            if on { self.skipped.remove(id); } else { self.skipped.insert(id.to_string()); }
        }
    }

    /// The rows in the order they are shown: as the queue has them (by
    /// verdict, the surest first), or the least sure first.
    pub fn order<'a>(&self, mut rows: Vec<(&'a Paper, &'a Suggestion)>) -> Vec<(&'a Paper, &'a Suggestion)> {
        if !self.part.is_empty() && rows.iter().any(|(_, s)| s.verdict == self.part) { rows.retain(|(_, s)| s.verdict == self.part) }
        // Stable, so equally sure suggestions keep the queue's order. One without a figure counts as not sure at all.
        if self.doubtful_first { rows.sort_by(|a, b| a.1.confidence.unwrap_or(0.).partial_cmp(&b.1.confidence.unwrap_or(0.)).unwrap_or(std::cmp::Ordering::Equal)) }
        rows
    }

    /// What confirming writes: every ticked paper with its status and the reason kept with it.
    pub fn plan(&self, rows: &[(&Paper, &Suggestion)]) -> Vec<Line> {
        rows.iter().filter(|(paper, _)| self.ticked(&paper.id)).map(|(paper, suggestion)| {
            let status = self.status(suggestion);
            Line { paper_id: paper.id.clone(), status: status.to_string(), reason: reason_for(paper, Some(suggestion), status) }
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, screening: &str) -> Paper {
        Paper { id: id.into(), screening: screening.into(), ..Default::default() }
    }

    fn suggestion(id: &str, verdict: &str, confidence: f64) -> Suggestion {
        Suggestion { paper_id: id.into(), verdict: verdict.into(), reason: format!("because {id}"), confidence: Some(confidence), ..Default::default() }
    }

    #[test]
    fn keys() {
        assert_eq!((status_of("y"), status_of("2"), status_of("u"), status_of("4"), status_of("p"), status_of("0"), status_of("x")), (Some("included"), Some("excluded"), Some("uncertain"), Some("irrelevant"), Some("pending"), Some("pending"), None));
        // Every decision has a key that leads back to it, and is a status the library knows.
        for (status, letter, digit) in DECISIONS {
            assert_eq!((status_of(letter), status_of(digit), status_of(&key_of(status).to_lowercase())), (Some(status), Some(status), Some(status)));
            assert!(SCREENINGS.iter().any(|(key, _)| *key == status));
        }
        assert_eq!(tally(["pending", "included", "included", "irrelevant", "unknown"].into_iter()), [1, 2, 0, 0, 1]);
    }

    #[test]
    fn reasons() {
        let mut mine = paper("a", "excluded");
        mine.screening_reason = "my own".into();
        let s = suggestion("a", "included", 0.9);
        // Agreeing takes the assistant's reason, keeping one's decision keeps one's own, anything else leaves it open.
        assert_eq!((reason_for(&mine, Some(&s), "included"), reason_for(&mine, Some(&s), "excluded"), reason_for(&mine, Some(&s), "uncertain"), reason_for(&mine, None, "included")), ("because a".to_string(), "my own".to_string(), String::new(), String::new()));
    }

    #[test]
    fn round() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let mut round = Round::new(ids(&["a", "b", "c"]), Some("b"));
        assert_eq!(round.current(), Some("b"));
        // A decision on the paper on show brings the next one up; on another paper the place holds.
        round.decided(Before::of(&paper("b", "pending")));
        assert_eq!(round.current(), Some("c"));
        round.decided(Before::of(&paper("a", "pending")));
        assert_eq!(round.current(), Some("c"));
        // On from the last paper is the end, and no further; back from the first stays there.
        round.step(true);
        round.step(true);
        assert!(round.ended() && round.current().is_none() && round.at == 3);
        round.go(0);
        round.step(false);
        assert_eq!(round.at, 0);
        // Taking a decision back shows its paper again, the last decision first.
        assert_eq!(round.undo().map(|before| (before.paper_id, round.at)), Some(("a".to_string(), 0)));
        assert_eq!(round.undo().map(|before| before.screening), Some("pending".to_string()));
        assert!(round.at == 1 && !round.can_undo() && round.undo().is_none());
        // A paper that left the library leaves the round; the one on show stays on show.
        round.go(2);
        round.keep(|id| id != "a");
        assert_eq!((round.ids.clone(), round.current()), (ids(&["b", "c"]), Some("c")));
        round.keep(|id| id != "c");
        assert!(round.ended() && round.ids == ids(&["b"]));
        assert_eq!((round.first(|id| id == "b"), round.first(|id| id == "c"), Round::new(ids(&["a"]), Some("z")).at), (Some(0), None, 0));
    }

    #[test]
    fn restoring() {
        let mut p = paper("a", "pending");
        let before = Before::of(&p);
        p.decide("included", "good");
        assert!(p.screening == "included" && !p.reviewed_at.is_empty());
        before.restore(&mut p);
        assert_eq!((p.screening.as_str(), p.screening_reason.as_str(), p.reviewed_at.as_str()), ("pending", "", ""));
    }

    #[test]
    fn confirming() {
        let papers = [paper("a", "pending"), paper("b", "pending"), paper("c", "pending")];
        let suggestions = [suggestion("a", "included", 0.9), suggestion("b", "irrelevant", 0.4), suggestion("c", "irrelevant", 0.8)];
        let rows: Vec<(&Paper, &Suggestion)> = papers.iter().zip(&suggestions).collect();
        let mut confirm = Confirming::default();
        // As suggested, until the person says otherwise.
        assert_eq!(confirm.plan(&rows).iter().map(|line| (line.paper_id.as_str(), line.status.as_str(), line.reason.as_str())).collect::<Vec<_>>(), [("a", "included", "because a"), ("b", "irrelevant", "because b"), ("c", "irrelevant", "because c")]);
        // Another status leaves the reason to the person; a paper left out is not written.
        confirm.choose(&suggestions[1], "uncertain");
        confirm.toggle("c");
        assert!(confirm.changed("b") && !confirm.changed("a") && !confirm.ticked("c") && confirm.status(&suggestions[1]) == "uncertain");
        assert_eq!(confirm.plan(&rows), [Line { paper_id: "a".into(), status: "included".into(), reason: "because a".into() }, Line { paper_id: "b".into(), status: "uncertain".into(), reason: String::new() }]);
        // Choosing a status for a paper left out counts it in; the suggested one is no change.
        confirm.choose(&suggestions[2], "irrelevant");
        confirm.choose(&suggestions[1], "irrelevant");
        assert!(confirm.ticked("c") && !confirm.changed("c") && !confirm.changed("b"));
        confirm.tick_all(["a", "b"].into_iter(), false);
        assert_eq!(confirm.plan(&rows).len(), 1);
        confirm.tick_all(["a", "b"].into_iter(), true);
        assert_eq!(confirm.plan(&rows).len(), 3);
        // One verdict on show; a part nobody suggested shows everything. The least sure first, when asked.
        confirm.part = "irrelevant".into();
        assert_eq!(confirm.order(rows.clone()).iter().map(|(p, _)| p.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        confirm.part = "uncertain".into();
        confirm.doubtful_first = true;
        assert_eq!(confirm.order(rows.clone()).iter().map(|(p, _)| p.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
    }
}
