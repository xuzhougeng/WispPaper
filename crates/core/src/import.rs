//! BibTeX and RIS readers: what reference managers and publisher sites export.

use crate::model::{Paper, clean_strings, normalize_doi, split_list};

/// Read a file by its content: RIS records start with `TY  -`, anything
/// holding an `@entry{` is BibTeX.
pub fn parse_references(text: &str, source: &str) -> Result<Vec<Paper>, String> {
    let text = text.trim_start_matches('\u{FEFF}');
    let mut papers = if text.lines().any(|l| l.trim_start().starts_with("TY  -")) {
        parse_ris(text)
    } else if text.contains('@') {
        parse_bibtex(text)
    } else {
        vec![]
    };
    if papers.is_empty() { return Err("没有在文件中找到文献条目（支持 BibTeX 和 RIS）。".into()) }
    for paper in &mut papers {
        paper.source = source.to_string();
    }
    Ok(papers)
}

// ---- BibTeX

/// Every entry with a title. `@comment`, `@string` and `@preamble` are skipped.
pub fn parse_bibtex(text: &str) -> Vec<Paper> {
    let chars: Vec<char> = text.chars().collect();
    let mut papers = vec![];
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < chars.len() && chars[j].is_alphabetic() { j += 1 }
        let kind: String = chars[start..j].iter().collect::<String>().to_lowercase();
        while j < chars.len() && chars[j].is_whitespace() { j += 1 }
        if j >= chars.len() || (chars[j] != '{' && chars[j] != '(') || kind.is_empty() {
            i = j.max(i + 1);
            continue;
        }
        let close = if chars[j] == '{' { '}' } else { ')' };
        // The entry body runs to the matching close, braces inside nest.
        let body_start = j + 1;
        let mut depth = 0i32;
        let mut k = body_start;
        while k < chars.len() {
            match chars[k] {
                '{' => depth += 1,
                '}' if depth > 0 => depth -= 1,
                c if c == close && depth == 0 => break,
                _ => {}
            }
            k += 1;
        }
        let body: String = chars[body_start..k.min(chars.len())].iter().collect();
        i = k + 1;
        if ["comment", "string", "preamble"].contains(&kind.as_str()) { continue }
        if let Some(paper) = bib_entry(&kind, &body) { papers.push(paper) }
    }
    papers
}

fn bib_entry(kind: &str, body: &str) -> Option<Paper> {
    let (_key, rest) = body.split_once(',')?;
    let fields = bib_fields(rest);
    let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let title = get("title");
    if title.is_empty() { return None }
    let mut paper = Paper {
        title,
        authors: get("author").split(" and ").map(str::trim).filter(|a| !a.is_empty() && *a != "others").collect::<Vec<_>>().join("; "),
        journal: [get("journal"), get("journaltitle"), get("booktitle"), get("publisher")].into_iter().find(|v| !v.is_empty()).unwrap_or_default(),
        year: [get("year"), get("date")].iter().find_map(|v| first_year(v)),
        doi: normalize_doi(&get("doi")),
        pmid: get("pmid"),
        url: get("url"),
        abstract_text: get("abstract"),
        tags: split_list(&get("keywords")),
        notes: get("annote"),
        ..Default::default()
    };
    let archive = format!("{} {} {}", get("archiveprefix"), get("eprinttype"), paper.journal).to_lowercase();
    if kind == "unpublished" || get("note").to_lowercase().contains("preprint") || ["arxiv", "biorxiv", "medrxiv"].iter().any(|a| archive.contains(a)) {
        paper.publication_status = "preprint".into();
    } else if ["article", "inproceedings", "book", "incollection"].contains(&kind) {
        paper.publication_status = "published".into();
    }
    Some(paper)
}

/// `name = {value}`, `name = "value"` or `name = 2024`, comma separated.
fn bib_fields(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut fields = vec![];
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') { i += 1 }
        let start = i;
        while i < chars.len() && chars[i] != '=' && chars[i] != ',' { i += 1 }
        if i >= chars.len() || chars[i] != '=' { continue }
        let name: String = chars[start..i].iter().collect::<String>().trim().to_lowercase();
        i += 1;
        // A value is one or more pieces joined by `#`.
        let mut value = String::new();
        loop {
            while i < chars.len() && chars[i].is_whitespace() { i += 1 }
            if i >= chars.len() { break }
            match chars[i] {
                '{' => {
                    let mut depth = 0;
                    let begin = i + 1;
                    while i < chars.len() {
                        match chars[i] {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 { break }
                            }
                            '\\' => i += 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    value.extend(&chars[begin.min(chars.len())..i.min(chars.len())]);
                    i += 1;
                }
                '"' => {
                    let begin = i + 1;
                    i += 1;
                    let mut depth = 0;
                    while i < chars.len() && !(chars[i] == '"' && depth == 0) {
                        match chars[i] {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            '\\' => i += 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    value.extend(&chars[begin.min(chars.len())..i.min(chars.len())]);
                    i += 1;
                }
                _ => {
                    let begin = i;
                    while i < chars.len() && chars[i] != ',' && chars[i] != '#' { i += 1 }
                    value.push_str(chars[begin..i].iter().collect::<String>().trim());
                }
            }
            while i < chars.len() && chars[i].is_whitespace() { i += 1 }
            if i < chars.len() && chars[i] == '#' { i += 1 } else { break }
        }
        if !name.is_empty() { fields.push((name, untex(&value))) }
    }
    fields
}

/// Plain text out of a BibTeX value: braces dropped, the common escapes and accents undone.
fn untex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '}' => {}
            '~' => out.push(' '),
            '\\' if i + 1 < chars.len() => {
                i += 1;
                let c = chars[i];
                if c.is_alphabetic() {
                    let begin = i;
                    while i + 1 < chars.len() && chars[i + 1].is_alphabetic() { i += 1 }
                    let command: String = chars[begin..=i].iter().collect();
                    match command.as_str() {
                        "textbackslash" => out.push('\\'),
                        "textasciitilde" => out.push('~'),
                        "textasciicircum" => out.push('^'),
                        "ss" => out.push('ß'),
                        "o" => out.push('ø'),
                        "O" => out.push('Ø'),
                        "ae" => out.push('æ'),
                        "AE" => out.push('Æ'),
                        "l" => out.push('ł'),
                        "L" => out.push('Ł'),
                        "i" => out.push('i'),
                        // \c{c}, \v{s}, \emph{…}: keep what the command wraps.
                        _ => {}
                    }
                    if i + 1 < chars.len() && chars[i + 1] == ' ' { i += 1 }
                } else if "'`^\"~".contains(c) {
                    // An accent applies to the next letter, possibly braced.
                    let mut j = i + 1;
                    while j < chars.len() && chars[j] == '{' { j += 1 }
                    if j < chars.len() {
                        out.push(accent(c, chars[j]));
                        i = j;
                    }
                } else {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn accent(mark: char, letter: char) -> char {
    const TABLE: [(char, &str, &str); 5] = [
        ('\'', "aeiouyAEIOUYcnszCNSZ", "áéíóúýÁÉÍÓÚÝćńśźĆŃŚŹ"),
        ('`', "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ('^', "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ('"', "aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
        ('~', "anoANO", "ãñõÃÑÕ"),
    ];
    for (m, plain, accented) in TABLE {
        if m == mark && let Some(at) = plain.chars().position(|c| c == letter) {
            return accented.chars().nth(at).unwrap_or(letter);
        }
    }
    letter
}

fn first_year(value: &str) -> Option<i32> {
    let digits: Vec<char> = value.chars().collect();
    digits.windows(4).find(|w| w.iter().all(char::is_ascii_digit)).and_then(|w| w.iter().collect::<String>().parse().ok()).filter(|y| (1800..=2200).contains(y))
}

// ---- RIS

/// Records between `TY  -` and `ER  -`.
pub fn parse_ris(text: &str) -> Vec<Paper> {
    let mut papers = vec![];
    let mut current: Option<Paper> = None;
    let mut authors: Vec<String> = vec![];
    let mut last_tag = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        let tagged = line.len() >= 5 && line.is_char_boundary(2) && line.is_char_boundary(5) && &line[2..5] == "  -" && line[..2].chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let (tag, value) = if tagged {
            last_tag = line[..2].to_string();
            (last_tag.clone(), line[5..].trim().to_string())
        } else if current.is_some() && !line.trim().is_empty() {
            // An untagged line continues the previous field (long abstracts).
            (last_tag.clone(), line.trim().to_string())
        } else {
            continue;
        };
        if tag == "TY" {
            let status = if value == "UNPB" { "preprint" } else if ["JOUR", "CONF", "CPAPER", "BOOK", "CHAP"].contains(&value.as_str()) { "published" } else { "" };
            current = Some(Paper { publication_status: status.to_string(), ..Default::default() });
            authors.clear();
            continue;
        }
        let Some(paper) = current.as_mut() else { continue };
        let append = |target: &mut String, value: &str| {
            if !target.is_empty() { target.push(' ') }
            target.push_str(value);
        };
        match tag.as_str() {
            "TI" | "T1" => append(&mut paper.title, &value),
            "AU" | "A1" => authors.push(value),
            "JO" | "JF" | "T2" | "J2" if paper.journal.is_empty() || !tagged => {
                if tagged { paper.journal = value } else { append(&mut paper.journal, &value) }
            }
            "PY" | "Y1" | "DA" if paper.year.is_none() => paper.year = first_year(&value),
            "DO" => paper.doi = normalize_doi(&value),
            "AB" | "N2" => append(&mut paper.abstract_text, &value),
            "KW" => paper.tags.push(value),
            "UR" if paper.url.is_empty() => paper.url = value,
            "N1" => append(&mut paper.notes, &value),
            "AN" if value.to_lowercase().starts_with("pmid") || (paper.pmid.is_empty() && value.bytes().all(|b| b.is_ascii_digit())) => paper.pmid = crate::model::normalize_pmid(&value),
            "ER" => {
                let mut done = current.take().unwrap();
                done.authors = authors.join("; ");
                done.tags = clean_strings(&done.tags);
                // "doi:10.…" sometimes hides in the URL or notes.
                if done.doi.is_empty() && let Some(at) = done.url.find("doi.org/") {
                    done.doi = normalize_doi(&done.url[at..]);
                }
                if !done.title.is_empty() { papers.push(done) }
            }
            _ => {}
        }
    }
    papers
}

/// The PDFs under a folder, at any depth, each with the folders between the
/// one picked and the file as its tags: `Root/植物/光合/a.pdf` is tagged
/// 「植物」 and 「光合」, a PDF lying in `Root` itself none. The picked folder's
/// own name is left out, being as often 「下载」 as a topic. Hidden files and
/// folders are skipped, and a linked folder is not followed (it may lead back
/// up). Sorted by path, so a folder is imported in the order it shows.
pub fn folder_pdfs(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<String>)> {
    fn walk(dir: &std::path::Path, tags: &[String], out: &mut Vec<(std::path::PathBuf, Vec<String>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') { continue }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                let mut inner = tags.to_vec();
                inner.push(name);
                walk(&path, &clean_strings(inner), out);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) && (kind.is_file() || path.is_file()) {
                out.push((path, tags.to_vec()));
            }
        }
    }
    let mut out = vec![];
    walk(root, &[], &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_levels_become_tags() {
        let root = std::env::temp_dir().join(format!("wisp-folder-{}", crate::model::new_id("T")));
        for dir in ["植物/光合", "植物/.cache", ".hidden", "空的", "Methods/Methods"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in ["top.PDF", "notes.txt", "植物/a.pdf", "植物/光合/b.pdf", "植物/光合/c.txt", "植物/.cache/d.pdf", ".hidden/e.pdf", "Methods/Methods/f.pdf", "植物/.g.pdf"] {
            std::fs::write(root.join(file), b"%PDF-1.4").unwrap();
        }
        let found: Vec<(String, Vec<String>)> = folder_pdfs(&root).into_iter().map(|(path, tags)| (path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), tags)).collect();
        let tags = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(found, vec![
            ("Methods/Methods/f.pdf".to_string(), tags(&["Methods"])),
            ("top.PDF".to_string(), tags(&[])),
            ("植物/a.pdf".to_string(), tags(&["植物"])),
            ("植物/光合/b.pdf".to_string(), tags(&["植物", "光合"])),
        ]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bibtex_entries() {
        let text = r#"
% a comment
@string{np = "Nature Plants"}
@article{zhang2024, title = {{Single-cell} atlas of {A}rabidopsis \& rice},
  author = "Zhang, Wei and M{\"u}ller, J\'an and others",
  journal = {Nature Plants}, year = 2024, doi = {https://doi.org/10.1000/ABC_1},
  keywords = {root, atlas; root},
  abstract = {Line one
     line two.}
}
@misc{pre, title="A preprint", author={Li, Na}, eprinttype={arXiv}, date={2023-05-01}}
@article{untitled, author={Nobody}}
"#;
        let papers = parse_bibtex(text);
        assert_eq!(papers.len(), 2);
        let p = &papers[0];
        assert_eq!(p.title, "Single-cell atlas of Arabidopsis & rice");
        assert_eq!(p.authors, "Zhang, Wei; Müller, Ján");
        assert_eq!((p.journal.as_str(), p.year, p.doi.as_str()), ("Nature Plants", Some(2024), "10.1000/abc_1"));
        assert_eq!(p.tags, ["root", "atlas"]);
        assert_eq!(p.abstract_text, "Line one line two.");
        assert_eq!(p.publication_status, "published");
        assert_eq!((papers[1].publication_status.as_str(), papers[1].year), ("preprint", Some(2023)));
    }

    #[test]
    fn export_then_import() {
        let mut original = Paper { title: "Cost of 100% {braces} & more_stuff".into(), authors: "Smith, Alice; Jones, Bob".into(), journal: "J. Tests".into(), year: Some(2020), doi: "10.1000/x_y".into(), ..Default::default() };
        original.normalize();
        let read = parse_bibtex(&crate::export::bibtex(&[&original]));
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].title.as_str(), read[0].authors.as_str(), read[0].doi.as_str()), ("Cost of 100% {braces} & more_stuff", "Smith, Alice; Jones, Bob", "10.1000/x_y"));
    }

    #[test]
    fn ris_records() {
        let text = "TY  - JOUR\nTI  - Root cell atlas\nAU  - Zhang, Wei\nAU  - Li, Na\nJO  - Plant Cell\nPY  - 2022/03/01\nDO  - 10.1000/ris\nAB  - First line\nsecond line\nKW  - root\nKW  - atlas\nER  - \n\nTY  - UNPB\nT1  - Draft\nER  - \n";
        let papers = parse_references(text, "RIS 导入").unwrap();
        assert_eq!(papers.len(), 2);
        assert_eq!(papers[0].authors, "Zhang, Wei; Li, Na");
        assert_eq!((papers[0].year, papers[0].doi.as_str(), papers[0].abstract_text.as_str()), (Some(2022), "10.1000/ris", "First line second line"));
        assert_eq!(papers[0].tags, ["root", "atlas"]);
        assert_eq!(papers[1].publication_status, "preprint");
        assert_eq!(papers[1].source, "RIS 导入");
        assert!(parse_references("just words", "x").is_err());
    }
}
