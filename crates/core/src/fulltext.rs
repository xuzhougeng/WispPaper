//! Page text of a stored full-text file, and the search over it that 文献库提问
//! uses to pick passages.

use std::path::Path;

/// What one paper's text may occupy; a longer text is cut and says so.
pub const MAX_TEXT_BYTES: usize = 8 << 20;

/// Why a file gave no text, in the words kept with it and shown. Each
/// says what was found and no more: a file the parsers stumble over is
/// not thereby a scan, and a scan is not thereby locked.
pub const LOCKED: &str = "这个 PDF 设了打开密码，读不了文字。去掉密码后重新添加即可。";
pub const NO_ENCODING: &str = "这个 PDF 的字体没有给出文字编码，页面看得见字，却提取不出来。可以让能看图的模型读页面。";
pub const NO_TEXT_LAYER: &str = "页面上没有文字层，多半是扫描件或图片。可以让能看图的模型读页面。";
const PARSER: &str = "两种读取方式都解析不了这个 PDF。这是解析的问题，不说明它是扫描件或加了密：在别的阅读器里能选中文字的话，文字是有的。";

/// What kind of failure a kept message tells of, for those who act on it
/// (the window, an agent): `file` (it could not be opened), `locked`,
/// `parser` (the readers could not make sense of it), `encoding` (text
/// that cannot be told from its glyphs), `no_text` (nothing but pictures),
/// `unsupported`. Messages kept by earlier builds are known too.
pub fn failure_kind(message: &str) -> &'static str {
    if message == LOCKED || message.starts_with("这个 PDF 加了密") {
        "locked"
    } else if message == NO_ENCODING {
        "encoding"
    } else if message == NO_TEXT_LAYER || message.starts_with("文件里没有可提取的文字") {
        "no_text"
    } else if message.starts_with("无法读取文件") {
        "file"
    } else if message.starts_with("不支持提取") {
        "unsupported"
    } else {
        "parser"
    }
}

/// A kept failure in a few words, for a list.
pub fn failure_brief(message: &str) -> &'static str {
    match failure_kind(message) {
        "locked" => "有打开密码",
        "encoding" => "字体没有文字编码",
        "no_text" => "页面没有文字层",
        "file" => "文件读不了",
        "unsupported" => "不支持这种文件",
        _ => "解析不了",
    }
}

/// Whether a model that sees pictures could still read the file: the
/// pages can be drawn, only their text cannot be had.
pub fn readable_by_eye(message: &str) -> bool {
    matches!(failure_kind(message), "encoding" | "no_text" | "parser")
}

/// The text of a file, page by page. A plain-text or Markdown file is one
/// page. A PDF that cannot be read is an error, never an empty text: an
/// answer should not silently rest on less than the reader thinks.
pub fn file_pages(path: &Path) -> Result<Vec<String>, String> {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let pages = match extension.as_str() {
        "txt" | "md" | "markdown" => vec![std::fs::read_to_string(path).map_err(|e| format!("无法读取文件：{e}"))?],
        "pdf" => {
            let bytes = std::fs::read(path).map_err(|e| format!("无法读取文件：{e}"))?;
            // Read off the glyphs first: that keeps the breaks the page shows. pdf-extract
            // is the second try; it panics on some files hayro reads, and reads some it does not.
            let laid_out = std::panic::catch_unwind(|| crate::pdftext::pages(bytes.clone())).unwrap_or_else(|_| Err(PARSER.into()));
            match laid_out {
                Ok(pages) if pages.iter().any(|p| !p.trim().is_empty()) => pages.into_iter().map(|p| tidy(&p)).collect(),
                // A password stops the second reader as well.
                Err(locked) if failure_kind(&locked) == "locked" => return Err(locked),
                first => {
                    let plain = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(&bytes)).ok().and_then(Result::ok);
                    match (plain, first) {
                        (Some(pages), _) => pages.into_iter().map(|p| tidy(&p)).collect(),
                        // The first read it and found no text: said below.
                        (None, Ok(pages)) => pages,
                        // What the first reader found out is the more exact of the two.
                        (None, Err(first)) if first == NO_ENCODING => return Err(first),
                        (None, Err(_)) => return Err(PARSER.into()),
                    }
                }
            }
        }
        other => return Err(format!("不支持提取 .{other} 文件的文字。")),
    };
    if pages.iter().all(|p| p.trim().is_empty()) {
        return Err(NO_TEXT_LAYER.into());
    }
    let mut total = 0;
    let mut kept = vec![];
    for page in pages {
        total += page.len();
        if total > MAX_TEXT_BYTES {
            kept.push("[后续页面超出长度上限，未提取]".to_string());
            break;
        }
        kept.push(page);
    }
    Ok(kept)
}

/// The text of a file read again, for a file whose text an earlier build
/// kept: when it cannot be read now (the file is gone), what was kept stays.
pub fn reread(path: &Path, known: Option<Result<Vec<String>, String>>) -> Result<Vec<String>, String> {
    match (file_pages(path), known) {
        (Err(_), Some(Ok(kept))) => Ok(kept),
        (pages, _) => pages,
    }
}

/// Collapse the spacing PDF extraction leaves behind; keep paragraph breaks.
fn tidy(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    let mut blank = 0;
    for line in page.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() { out.push_str(if blank > 0 { "\n\n" } else { "\n" }) }
        blank = 0;
        out.push_str(&line);
    }
    out
}

/// A text reduced to what a quotation is compared by: its letters and
/// digits, lowercase, with ligatures spelled out. Spacing, line breaks,
/// hyphens and punctuation differ between a PDF's text and a quotation of
/// it; these do not.
pub fn bare(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    out
}

/// Which glyphs are raised citation numbers, by their boxes (`[left, top,
/// right, bottom]`): digits set smaller and higher than the glyph of the
/// running text before them. After a digit they are a power, and stay. The
/// page text a quotation is taken from leaves such numbers out (`pdftext`).
fn raised_numbers(glyphs: &[String], boxes: &[[f32; 4]]) -> Vec<bool> {
    let mut raised = vec![false; glyphs.len()];
    let mut before: Option<usize> = None;
    for (index, glyph) in glyphs.iter().enumerate() {
        let said = bare(glyph);
        if said.is_empty() { continue }
        if let Some(at) = before.filter(|at| !bare(&glyphs[*at]).ends_with(|c: char| c.is_numeric())) {
            // A font's own superscript digits, or ordinary ones set small above the line they follow.
            let small = || boxes.get(index).zip(boxes.get(at)).is_some_and(|(b, line)| {
                let height = line[3] - line[1];
                b[3] - b[1] < 0.82 * height && b[3] < line[3] - 0.12 * height && b[3] > line[1]
            });
            if said.chars().all(|c| "⁰¹²³⁴⁵⁶⁷⁸⁹".contains(c)) || said.chars().all(|c| c.is_ascii_digit()) && small() {
                raised[index] = true;
                continue;
            }
        }
        before = Some(index);
    }
    raised
}

/// Where a quotation stands among the glyphs of a page, given the text and
/// the box of each glyph in drawing order: the range of glyphs it covers,
/// and whether that is the whole quotation. Raised citation numbers between
/// its words do not break it. When the whole quotation is not there, its
/// first 80, 40 or 20 characters are looked for, so a quotation that drifts
/// from the page after a while is still found, and said to be found in part.
pub fn find_quote(glyphs: &[String], boxes: &[[f32; 4]], quote: &str) -> Option<(std::ops::Range<usize>, bool)> {
    let raised = raised_numbers(glyphs, boxes);
    // Every compared character, with the glyph it came from: without the raised numbers, then with them.
    let streams = [true, false].map(|skip| {
        let mut stream: Vec<(char, usize)> = vec![];
        for (index, glyph) in glyphs.iter().enumerate() {
            if !(skip && raised[index]) { stream.extend(bare(glyph).chars().map(|c| (c, index))) }
        }
        stream
    });
    let wanted: Vec<char> = bare(quote).chars().collect();
    for length in [wanted.len(), 80, 40, 20] {
        if length > wanted.len() || length < 8 { continue }
        let part = &wanted[..length];
        for stream in &streams {
            if let Some(at) = stream.windows(length).position(|window| window.iter().map(|(c, _)| c).eq(part.iter())) {
                return Some((stream[at].1..stream[at + length - 1].1 + 1, length == wanted.len()));
            }
        }
    }
    None
}

/// Glyph boxes (`[left, top, right, bottom]`, in drawing order) merged into
/// one box per line of text, for highlighting a passage.
pub fn line_boxes(glyphs: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut lines: Vec<[f32; 4]> = vec![];
    for glyph in glyphs {
        let same_line = lines.last().is_some_and(|line| {
            let overlap = line[3].min(glyph[3]) - line[1].max(glyph[1]);
            // On the line when it shares most of its height, and does not start far back to the left.
            overlap > 0.5 * (glyph[3] - glyph[1]).min(line[3] - line[1]) && glyph[0] >= line[0] - (glyph[3] - glyph[1])
        });
        match lines.last_mut() {
            Some(line) if same_line => *line = [line[0].min(glyph[0]), line[1].min(glyph[1]), line[2].max(glyph[2]), line[3].max(glyph[3])],
            _ => lines.push(*glyph),
        }
    }
    lines
}

/// The words of a question worth matching: lowercase runs of letters and
/// digits at least two characters long; every CJK character pair counts as a
/// word, since Chinese has no spaces.
pub fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut push = |word: String| {
        if !out.contains(&word) { out.push(word) }
    };
    let lower = text.to_lowercase();
    let mut word = String::new();
    let mut cjk: Vec<char> = vec![];
    let flush_word = |word: &mut String, push: &mut dyn FnMut(String)| {
        if word.chars().count() >= 2 && !STOP_WORDS.contains(&word.as_str()) { push(std::mem::take(word)) } else { word.clear() }
    };
    for c in lower.chars().chain(std::iter::once(' ')) {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&c);
        if is_cjk {
            flush_word(&mut word, &mut push);
            cjk.push(c);
            if cjk.len() >= 2 { push(cjk[cjk.len() - 2..].iter().collect()) }
        } else {
            cjk.clear();
            if c.is_alphanumeric() { word.push(c) } else { flush_word(&mut word, &mut push) }
        }
    }
    out
}

const STOP_WORDS: [&str; 44] = [
    "the", "and", "for", "are", "was", "were", "with", "that", "this", "from", "have", "has", "what", "which", "how", "why", "who", "does", "did", "can", "could", "about", "into",
    "their", "there", "these", "those", "any", "all", "not", "but", "its", "our", "you", "your", "they", "them", "than", "then", "been", "being", "of", "in", "on",
];

/// How well a text answers the terms: each term counts once, plus a little for repeats.
pub fn score(text_lower: &str, terms: &[String]) -> f32 {
    let mut total = 0.;
    for term in terms {
        let hits = text_lower.matches(term.as_str()).count();
        if hits > 0 { total += 1. + (hits as f32).ln() * 0.3 }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_of_mixed_text() {
        assert_eq!(terms("What is the role of WOX5 in root 干细胞?"), ["is", "role", "wox5", "root", "干细", "细胞"]);
        assert!(terms("的 a").is_empty());
    }

    #[test]
    fn scoring() {
        let t = terms("root stem cells");
        assert!(score("the root of root stem cells", &t) > score("stem only", &t));
        assert_eq!(score("nothing here", &t), 0.);
    }

    #[test]
    fn text_files_and_errors() {
        let dir = std::env::temp_dir().join(format!("wisp-fulltext-{}", crate::model::new_id("T")));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("a.txt");
        std::fs::write(&txt, "hello\nworld").unwrap();
        assert_eq!(file_pages(&txt).unwrap(), ["hello\nworld"]);
        let bad = dir.join("bad.pdf");
        std::fs::write(&bad, "not a pdf").unwrap();
        // Neither reader makes sense of it: a parsing failure, which is not called a scan.
        let failure = file_pages(&bad).unwrap_err();
        assert_eq!((failure_kind(&failure), readable_by_eye(&failure)), ("parser", true));
        assert!(!failure.contains("可能是扫描件"));
        assert_eq!(failure_kind(&file_pages(&dir.join("x.docx")).unwrap_err()), "unsupported");
        assert_eq!(failure_kind(&file_pages(&dir.join("gone.pdf")).unwrap_err()), "file");
        assert_eq!((failure_kind(LOCKED), readable_by_eye(LOCKED), failure_brief(LOCKED)), ("locked", false, "有打开密码"));
        assert_eq!((failure_kind(NO_ENCODING), failure_kind(NO_TEXT_LAYER)), ("encoding", "no_text"));
        // What earlier builds kept.
        assert_eq!((failure_kind("文件里没有可提取的文字（可能是扫描件）。"), failure_kind("PDF 结构异常，无法提取文字。")), ("no_text", "parser"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn quotations_among_glyphs() {
        assert_eq!(bare("Single-cell\nRNA-seq: ﬁve ﬂoors, 10×!"), "singlecellrnaseqfivefloors10");
        // One glyph per character, as a PDF draws them; the ligature is one glyph.
        let glyphs: Vec<String> = "Libraries were prepared with 10x Genomics Chromium. The ﬁrst run failed.".chars().map(String::from).collect();
        let find_quote = |glyphs: &[String], quote: &str| find_quote(glyphs, &[], quote);
        let (found, whole) = find_quote(&glyphs, "prepared with\n10X genomics chromium").unwrap();
        assert_eq!((glyphs[found].concat().as_str(), whole), ("prepared with 10x Genomics Chromium", true));
        assert_eq!(glyphs[find_quote(&glyphs, "The first run").unwrap().0].concat(), "The ﬁrst run");
        // A quotation that leaves the page after its beginning is found by that beginning: its first 40 characters here.
        let (drifting, whole) = find_quote(&glyphs, "Libraries were prepared with 10x Genomics Chromium and then something the page never says at all").unwrap();
        assert_eq!((glyphs[drifting].concat().as_str(), whole), ("Libraries were prepared with 10x Genomics Chro", false));
        assert_eq!(find_quote(&glyphs, "nothing of the kind here"), None);
        assert_eq!(find_quote(&glyphs, "with"), None);
        assert_eq!(find_quote(&[], "prepared with 10x"), None);
    }

    #[test]
    fn raised_citation_numbers_do_not_cut_a_quotation() {
        // Issue 92: "16" is set small above the line after "challenging"; "20" and "5" are of the text.
        let page = "The assay remains challenging16, especially for the 20 samples with low input in 2019 5.";
        let glyphs: Vec<String> = page.chars().map(String::from).collect();
        let boxes: Vec<[f32; 4]> = (0..glyphs.len()).map(|i| if (29..31).contains(&i) { [i as f32 * 5., 97., i as f32 * 5. + 3., 103.] } else { [i as f32 * 5., 100., i as f32 * 5. + 5., 110.] }).collect();
        let quote = "The assay remains challenging, especially for the 20 samples with low input";
        let (found, whole) = find_quote(&glyphs, &boxes, quote).unwrap();
        assert_eq!((glyphs[found].concat().as_str(), whole), (&page[..page.find(" in 2019").unwrap()], true));
        // A quotation that has the number is found as well; without the boxes only its beginning is.
        assert!(find_quote(&glyphs, &boxes, "remains challenging 16, especially for").unwrap().1);
        assert_eq!(find_quote(&glyphs, &[], quote).map(|(found, whole)| (found.len(), whole)), Some((23, false)));
        // Numbers of the running text are compared as before: another figure is not the quotation.
        assert_eq!(find_quote(&glyphs, &boxes, "especially for the 30 samples with low input"), None);
        // The font's own superscripts need no boxes.
        let glyphs: Vec<String> = "The assay remains challenging¹⁶, especially for samples.".chars().map(String::from).collect();
        assert!(find_quote(&glyphs, &[], "The assay remains challenging, especially for samples").unwrap().1);
    }

    #[test]
    fn boxes_by_line() {
        // Two glyphs on one line, a superscript on it, then the next line starting back at the left.
        let glyphs = [[10., 100., 16., 112.], [16., 100., 22., 112.], [22., 97., 25., 106.], [10., 114., 16., 126.], [16., 114., 30., 126.]];
        assert_eq!(line_boxes(&glyphs), [[10., 97., 25., 112.], [10., 114., 30., 126.]]);
        assert!(line_boxes(&[]).is_empty());
    }

    #[test]
    fn tidy_keeps_paragraphs() {
        assert_eq!(tidy("  a   b \n c\n\n\n d "), "a b\nc\n\nd");
    }
}
