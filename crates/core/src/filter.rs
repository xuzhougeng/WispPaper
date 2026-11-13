//! Search, filters and sorting of the paper list, plus the values the filter
//! menus offer. Pure functions over `&[Paper]`.

use crate::model::Paper;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// The sidebar's views: each narrows the library before the filters apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// Everything except the papers marked 不相关.
    #[default]
    All,
    /// Added within the last seven days.
    Recent,
    Unread,
    Starred,
    /// No screening decision yet.
    Pending,
    /// No stored PDF.
    NoPdf,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub view: View,
    pub search: String,
    pub tag: String,
    pub year: String,
    pub screening: String,
    pub reading: String,
    pub fulltext: String,
    pub source: String,
    /// "yes" keeps preprints only, "no" leaves them out.
    pub preprint: String,
    /// Custom field id → the value a paper must hold.
    pub fields: BTreeMap<String, String>,
    /// newest, or `<key>-asc` / `<key>-desc` with key title, year, journal or authors.
    pub sort: String,
}

impl Filters {
    /// How many filters (not counting the search and the view) are set.
    pub fn active(&self) -> usize {
        [&self.tag, &self.year, &self.screening, &self.reading, &self.fulltext, &self.source, &self.preprint].iter().filter(|v| !v.is_empty()).count() + self.fields.len()
    }

    pub fn clear(&mut self) {
        *self = Filters { view: self.view, sort: std::mem::take(&mut self.sort), ..Default::default() };
    }
}

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn in_view(p: &Paper, view: View, now_ms: i64) -> bool {
    match view {
        View::All => true,
        View::Recent => crate::time::millis(&p.added_at) >= now_ms - WEEK_MS,
        View::Unread => p.reading == "unread",
        View::Starred => p.starred,
        View::Pending => p.screening == "pending" || p.screening == "uncertain",
        View::NoPdf => !matches!(p.fulltext.as_str(), "stored" | "textless"),
    }
}

/// Everything the search box matches, lowercased.
fn haystack(p: &Paper) -> String {
    let mut hay = [p.title.as_str(), &p.authors, &p.doi, &p.pmid, &p.journal, &p.abstract_text, &p.notes].join(" ");
    for tag in &p.tags {
        hay.push(' ');
        hay.push_str(tag);
    }
    for values in p.fields.values() {
        for value in values {
            hay.push(' ');
            hay.push_str(value);
        }
    }
    if let Some(year) = p.year {
        hay.push_str(&format!(" {year}"));
    }
    hay.to_lowercase()
}

pub fn matches(p: &Paper, f: &Filters, now_ms: i64) -> bool {
    // Papers marked irrelevant stay in the library only so that imports skip
    // them; they show up when asked for by name.
    if f.screening.is_empty() { if p.screening == "irrelevant" { return false } } else if p.screening != f.screening { return false }
    if !in_view(p, f.view, now_ms) { return false }
    if !f.tag.is_empty() && !p.tags.contains(&f.tag) { return false }
    if !f.year.is_empty() && p.year.map(|y| y.to_string()).as_deref() != Some(f.year.as_str()) { return false }
    if !f.reading.is_empty() && p.reading != f.reading { return false }
    if !f.fulltext.is_empty() && p.fulltext != f.fulltext { return false }
    if !f.source.is_empty() && p.source != f.source { return false }
    match f.preprint.as_str() {
        "yes" if p.publication_status != "preprint" => return false,
        "no" if p.publication_status == "preprint" => return false,
        _ => {}
    }
    for (field, wanted) in &f.fields {
        if !p.fields.get(field).is_some_and(|values| values.contains(wanted)) { return false }
    }
    says(p, &f.search)
}

/// Whether a paper holds every word of what was typed into a search box, somewhere in its record; nothing typed matches every paper.
pub fn says(p: &Paper, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() { return true }
    let hay = haystack(p);
    query.split_whitespace().all(|word| hay.contains(word))
}

/// The key and direction of a sort value; anything unknown is "newest".
pub fn sort_parts(sort: &str) -> (&str, bool) {
    match sort.rsplit_once('-') {
        Some((key, dir)) if ["title", "year", "journal", "authors"].contains(&key) => (key, dir == "desc"),
        _ => ("added", true),
    }
}

/// Clicking a column header: flip the direction when it already sorts, else its default.
pub fn toggle_sort(current: &str, key: &str) -> String {
    let (now, desc) = sort_parts(current);
    if now == key {
        format!("{key}-{}", if desc { "asc" } else { "desc" })
    } else {
        format!("{key}-{}", if key == "year" { "desc" } else { "asc" })
    }
}

/// Text order in either direction; blank values always sink to the end.
fn text_cmp(a: &str, b: &str, desc: bool) -> Ordering {
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => {
            let order = a.to_lowercase().cmp(&b.to_lowercase());
            if desc { order.reverse() } else { order }
        }
    }
}

fn compare(a: &Paper, b: &Paper, key: &str, desc: bool) -> Ordering {
    match key {
        "title" => text_cmp(&a.title, &b.title, desc),
        "journal" => text_cmp(&a.journal, &b.journal, desc),
        "authors" => text_cmp(&a.authors, &b.authors, desc),
        "year" => match (a.year, b.year) {
            (None, None) => Ordering::Equal,
            (None, _) => Ordering::Greater,
            (_, None) => Ordering::Less,
            (Some(x), Some(y)) => if desc { y.cmp(&x) } else { x.cmp(&y) },
        },
        _ => b.added_at.cmp(&a.added_at),
    }
}

/// The indices of the papers that pass the filters, in display order.
pub fn filter_papers(papers: &[Paper], f: &Filters, now_ms: i64) -> Vec<usize> {
    let (key, desc) = sort_parts(&f.sort);
    let mut found: Vec<usize> = (0..papers.len()).filter(|&i| matches(&papers[i], f, now_ms)).collect();
    // Stable: equal keys keep the library's newest-first order.
    found.sort_by(|&a, &b| compare(&papers[a], &papers[b], key, desc));
    found
}

/// How many papers each sidebar view holds.
pub fn view_count(papers: &[Paper], view: View, now_ms: i64) -> usize {
    papers.iter().filter(|p| p.screening != "irrelevant" && in_view(p, view, now_ms)).count()
}

/// Every distinct value with the number of papers holding it, most used first.
fn tally<'a>(values: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for value in values.filter(|v| !v.is_empty()) {
        *counts.entry(value).or_default() += 1;
    }
    let mut list: Vec<(String, usize)> = counts.into_iter().map(|(v, n)| (v.to_string(), n)).collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    list
}

fn active(papers: &[Paper]) -> impl Iterator<Item = &Paper> {
    papers.iter().filter(|p| p.screening != "irrelevant")
}

pub fn tag_counts(papers: &[Paper]) -> Vec<(String, usize)> {
    tally(active(papers).flat_map(|p| p.tags.iter().map(String::as_str)))
}

pub fn source_counts(papers: &[Paper]) -> Vec<(String, usize)> {
    tally(active(papers).map(|p| p.source.as_str()))
}

pub fn field_counts(papers: &[Paper], field: &str) -> Vec<(String, usize)> {
    tally(active(papers).filter_map(|p| p.fields.get(field)).flat_map(|values| values.iter().map(String::as_str)))
}

/// The years papers were published in, latest first.
pub fn years(papers: &[Paper]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
    for year in active(papers).filter_map(|p| p.year) {
        *counts.entry(year).or_default() += 1;
    }
    counts.into_iter().rev().map(|(y, n)| (y.to_string(), n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(title: &str, year: Option<i32>, added: &str) -> Paper {
        let mut p = Paper { id: title.into(), title: title.into(), year, added_at: added.into(), ..Default::default() };
        p.normalize();
        p.added_at = added.into();
        p
    }

    fn library() -> Vec<Paper> {
        let mut a = paper("Zebra fish atlas", Some(2021), "2026-01-03T00:00:00.000Z");
        a.tags = vec!["atlas".into(), "fish".into()];
        a.fields.insert("F1".into(), vec!["Danio rerio".into()]);
        let mut b = paper("apple genome", None, "2026-01-02T00:00:00.000Z");
        b.starred = true;
        b.publication_status = "preprint".into();
        let mut c = paper("Maize root cells", Some(2024), "2026-01-01T00:00:00.000Z");
        c.tags = vec!["atlas".into()];
        c.reading = "read".into();
        let mut d = paper("Off topic", Some(2020), "2026-01-04T00:00:00.000Z");
        d.screening = "irrelevant".into();
        vec![a, b, c, d]
    }

    #[test]
    fn a_search_box_asks_for_every_word() {
        let papers = library();
        assert!(says(&papers[0], "  FISH atlas ") && says(&papers[0], "danio 2021") && says(&papers[0], ""));
        assert!(!says(&papers[0], "fish maize") && !says(&papers[1], "atlas"));
    }

    fn titles(papers: &[Paper], f: &Filters) -> Vec<String> {
        filter_papers(papers, f, 0).into_iter().map(|i| papers[i].title.clone()).collect()
    }

    #[test]
    fn default_hides_irrelevant_and_sorts_newest() {
        let papers = library();
        assert_eq!(titles(&papers, &Filters::default()), ["Zebra fish atlas", "apple genome", "Maize root cells"]);
        let asked = Filters { screening: "irrelevant".into(), ..Default::default() };
        assert_eq!(titles(&papers, &asked), ["Off topic"]);
    }

    #[test]
    fn search_and_filters() {
        let papers = library();
        let f = |edit: fn(&mut Filters)| {
            let mut f = Filters::default();
            edit(&mut f);
            titles(&papers, &f)
        };
        assert_eq!(f(|f| f.search = "ATLAS fish".into()), ["Zebra fish atlas"]);
        assert_eq!(f(|f| f.search = "danio".into()), ["Zebra fish atlas"]);
        assert_eq!(f(|f| f.tag = "atlas".into()).len(), 2);
        assert_eq!(f(|f| f.year = "2024".into()), ["Maize root cells"]);
        assert_eq!(f(|f| f.preprint = "yes".into()), ["apple genome"]);
        assert_eq!(f(|f| f.preprint = "no".into()).len(), 2);
        assert_eq!(f(|f| f.view = View::Starred), ["apple genome"]);
        assert_eq!(f(|f| f.view = View::Unread).len(), 2);
        assert_eq!(f(|f| { f.fields.insert("F1".into(), "Danio rerio".into()); }), ["Zebra fish atlas"]);
        assert_eq!(f(|f| { f.fields.insert("F1".into(), "Mus".into()); }).len(), 0);
    }

    #[test]
    fn sorting_sinks_blanks() {
        let papers = library();
        let sorted = |sort: &str| titles(&papers, &Filters { sort: sort.into(), ..Default::default() });
        assert_eq!(sorted("title-asc"), ["apple genome", "Maize root cells", "Zebra fish atlas"]);
        assert_eq!(sorted("year-desc"), ["Maize root cells", "Zebra fish atlas", "apple genome"]);
        assert_eq!(sorted("year-asc"), ["Zebra fish atlas", "Maize root cells", "apple genome"]);
        assert_eq!(toggle_sort("newest", "year"), "year-desc");
        assert_eq!(toggle_sort("year-desc", "year"), "year-asc");
        assert_eq!(toggle_sort("year-asc", "title"), "title-asc");
    }

    #[test]
    fn counts() {
        let papers = library();
        assert_eq!(tag_counts(&papers), [("atlas".to_string(), 2), ("fish".to_string(), 1)]);
        assert_eq!(years(&papers), [("2024".to_string(), 1), ("2021".to_string(), 1)]);
        assert_eq!(view_count(&papers, View::All, 0), 3);
        assert_eq!(field_counts(&papers, "F1"), [("Danio rerio".to_string(), 1)]);
        let mut f = Filters { tag: "x".into(), search: "q".into(), sort: "year-asc".into(), view: View::Starred, ..Default::default() };
        assert_eq!(f.active(), 1);
        f.clear();
        assert_eq!((f.active(), f.search.as_str(), f.sort.as_str(), f.view), (0, "", "year-asc", View::Starred));
    }
}
