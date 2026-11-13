//! BibTeX and CSV writers.

use crate::model::{FieldDef, Paper, clean_strings};
use std::collections::HashMap;

/// The papers as `@article` entries. Titles are brace-protected so BibTeX
/// styles keep their capitalisation; authors stored as "A; B" become
/// "A and B". Keys are author-year-word and get a letter suffix on collision.
/// Tags and custom field values go into `keywords`.
pub fn bibtex(papers: &[&Paper]) -> String {
    let mut used: HashMap<String, usize> = HashMap::new();
    let keys: Vec<String> = papers
        .iter()
        .map(|p| {
            let base = bib_key(p);
            let n = used.entry(base.clone()).or_default();
            *n += 1;
            if *n > 1 { format!("{base}{}", (b'a' + (*n as u8 - 2).min(25)) as char) } else { base }
        })
        .collect();
    bibtex_keyed(&keys.iter().map(String::as_str).zip(papers.iter().copied()).collect::<Vec<_>>())
}

/// The same with the keys given: those a manuscript cites the papers by (`cite::bibtex`).
pub fn bibtex_keyed(papers: &[(&str, &Paper)]) -> String {
    let mut out = String::new();
    for &(key, p) in papers {
        let year = p.year.map(|y| y.to_string()).unwrap_or_default();
        let mut fields: Vec<(&str, String)> = vec![
            ("title", format!("{{{}}}", bib_escape(&p.title))),
            ("author", bib_escape(&bib_authors(&p.authors))),
            ("journal", bib_escape(&p.journal)),
            ("year", year),
            ("doi", bib_escape(&p.doi)),
        ];
        if !p.doi.is_empty() {
            fields.push(("url", format!("https://doi.org/{}", bib_escape(&p.doi))));
        } else if !p.url.is_empty() {
            fields.push(("url", bib_escape(&p.url)));
        }
        if !p.pmid.is_empty() { fields.push(("pmid", bib_escape(&p.pmid))) }
        if p.publication_status == "preprint" { fields.push(("note", "Preprint".into())) }
        let keywords = clean_strings(p.tags.iter().chain(p.fields.values().flatten()));
        if !keywords.is_empty() { fields.push(("keywords", bib_escape(&keywords.join(", ")))) }
        if !p.abstract_text.is_empty() {
            fields.push(("abstract", bib_escape(&p.abstract_text.split_whitespace().collect::<Vec<_>>().join(" "))));
        }
        out.push_str(&format!("@article{{{key},\n"));
        for (name, value) in fields {
            if value.is_empty() || value == "{}" { continue }
            out.push_str(&format!("  {name} = {{{value}}},\n"));
        }
        out.push_str("}\n\n");
    }
    out
}

fn ascii_key(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// "surname2024word": the first author, the year and the first title word
/// longer than three letters, ASCII only.
fn bib_key(p: &Paper) -> String {
    let mut surname = crate::model::author_names(&p.authors).first().map(|(family, _)| ascii_key(family)).unwrap_or_default();
    if surname.is_empty() { surname = "anon".into() }
    let word = p.title.split_whitespace().map(ascii_key).find(|w| w.len() > 3).unwrap_or_default();
    format!("{surname}{}{word}", p.year.map(|y| y.to_string()).unwrap_or_default()).to_lowercase()
}

/// "A; B" → "A and B". A comma-separated list is split only when every piece
/// holds several words, so "Smith, Alice" stays one name while
/// "Alice Smith, Bob Jones" becomes two.
fn bib_authors(authors: &str) -> String {
    let mut sep = ';';
    if !authors.contains(';') && !authors.contains(" and ") && authors.contains(',') {
        sep = ',';
        if authors.split(',').any(|piece| piece.split_whitespace().count() < 2) { sep = ';' }
    }
    // `Bono M` would be read by BibTeX as a given name and the family name M: written `Bono, M`.
    let names = authors.split(sep).map(str::trim).filter(|n| !n.is_empty()).collect::<Vec<_>>().join("; ");
    crate::model::family_first(&names).split("; ").collect::<Vec<_>>().join(" and ")
}

/// Protect the characters BibTeX and LaTeX treat specially.
fn bib_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.trim().chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '&' | '%' | '$' | '#' | '_' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            _ => out.push(c),
        }
    }
    out
}

/// RFC 4180 CSV with CRLF line ends and a UTF-8 BOM, so spreadsheets open it
/// correctly. A cell starting with a formula character gets an apostrophe in
/// front so no spreadsheet evaluates it.
pub fn csv_table(header: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::from("\u{FEFF}");
    let mut write = |row: &[String], guard: bool| {
        let cells: Vec<String> = row
            .iter()
            .map(|cell| {
                let cell = if guard && cell.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{cell}") } else { cell.clone() };
                if cell.contains([',', '"', '\n', '\r']) { format!("\"{}\"", cell.replace('"', "\"\"")) } else { cell }
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    };
    write(header, false);
    for row in rows {
        write(row, true);
    }
    out
}

/// One row per paper; multi-valued cells are joined by "; ". Each custom
/// field is a column named after it.
pub fn papers_csv(papers: &[&Paper], fields: &[FieldDef]) -> String {
    let mut header: Vec<String> = ["id", "title", "authors", "journal", "year", "doi", "pmid", "url", "publication_status", "screening", "screening_reason", "reading", "fulltext", "starred", "tags"].map(String::from).to_vec();
    header.extend(fields.iter().map(|f| f.name.clone()));
    header.extend(["notes", "abstract", "source", "added_at"].map(String::from));
    let rows: Vec<Vec<String>> = papers
        .iter()
        .map(|p| {
            let mut row = vec![
                p.id.clone(), p.title.clone(), p.authors.clone(), p.journal.clone(), p.year.map(|y| y.to_string()).unwrap_or_default(),
                p.doi.clone(), p.pmid.clone(), p.url.clone(), p.publication_status.clone(), p.screening.clone(), p.screening_reason.clone(),
                p.reading.clone(), p.fulltext.clone(), if p.starred { "yes".into() } else { String::new() }, p.tags.join("; "),
            ];
            row.extend(fields.iter().map(|f| p.fields.get(&f.id).map(|v| v.join("; ")).unwrap_or_default()));
            row.extend([p.notes.clone(), p.abstract_text.clone(), p.source.clone(), p.added_at.clone()]);
            row
        })
        .collect();
    csv_table(&header, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldKind;

    fn paper() -> Paper {
        Paper {
            id: "P-1".into(),
            title: "Single-cell atlas of Arabidopsis & rice roots".into(),
            authors: "Zhang, Wei; O'Brien, Ann".into(),
            journal: "Nature Plants".into(),
            year: Some(2024),
            doi: "10.1000/abc_1".into(),
            tags: vec!["root".into()],
            fields: [("F1".to_string(), vec!["Arabidopsis thaliana".to_string()])].into(),
            ..Default::default()
        }
    }

    #[test]
    fn bibtex_entry() {
        let p = paper();
        let text = bibtex(&[&p, &p]);
        assert!(text.starts_with("@article{zhang2024singlecell,\n  title = {{Single-cell atlas of Arabidopsis \\& rice roots}},\n  author = {Zhang, Wei and O'Brien, Ann},\n  journal = {Nature Plants},\n  year = {2024},\n  doi = {10.1000/abc\\_1},\n  url = {https://doi.org/10.1000/abc\\_1},\n  keywords = {root, Arabidopsis thaliana},\n}\n"), "{text}");
        assert!(text.contains("@article{zhang2024singlecella,"));
    }

    #[test]
    fn author_splitting() {
        assert_eq!(bib_authors("Smith, Alice"), "Smith, Alice");
        assert_eq!(bib_authors("Alice Smith, Bob Jones"), "Alice Smith and Bob Jones");
        assert_eq!(bib_authors("A B; C D"), "A B and C D");
        assert_eq!(bib_authors("Bono M; de Melo AV"), "Bono, M and de Melo, AV");
    }

    #[test]
    fn csv_guards_formulas_and_quotes() {
        let mut p = paper();
        p.title = "=SUM(A1), \"quoted\"".into();
        let fields = [FieldDef { id: "F1".into(), name: "物种".into(), kind: FieldKind::Tags, ..Default::default() }];
        let text = papers_csv(&[&p], &fields);
        assert!(text.starts_with("\u{FEFF}id,title,"));
        assert!(text.contains(",物种,notes,"));
        assert!(text.contains("\"'=SUM(A1), \"\"quoted\"\"\""));
        assert!(text.contains(",Arabidopsis thaliana,"));
        assert!(text.ends_with("\r\n"));
    }
}
