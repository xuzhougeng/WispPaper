//! A paper's full text put in order for writing: its pages cut into
//! paragraphs and headings (`blocks`), every paragraph given the part of the
//! paper it belongs to (`assign` by rule, or a model's reading of the
//! outline: `outline_prompt` / `parse_outline`), the paragraphs cut into
//! sentences (`sentences`), and the sentences of one part ranked against
//! what a person is writing (`rank`). 写作 shows those as candidates: write
//! an introduction, read how the library's introductions put it.

use crate::fulltext::bare;
use std::collections::{HashMap, HashSet};

/// The parts a paper is cut into, as stored and as shown. `other` is body
/// text whose part is not known; what is left out (front matter, captions,
/// references) has no key here and is not kept.
pub const SECTIONS: [(&str, &str); 6] = [("abstract", "摘要"), ("introduction", "导言"), ("methods", "方法"), ("results", "结果"), ("discussion", "讨论"), ("other", "其他")];

/// The label of a block that is not kept.
pub const SKIP: &str = "skip";

/// Bumped when `blocks`, `assign` or `sentences` change what they make of
/// the same pages, so structures made by rule are made again.
pub const VERSION: i32 = 2;

/// A paragraph or a heading of a paper, in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// The 1-based page it starts on.
    pub page: usize,
    pub text: String,
    /// A short line standing alone: a section title, or something that looks like one.
    pub heading: bool,
}

/// A sentence of a paper, with where it stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sentence {
    /// A key of `SECTIONS`.
    pub section: String,
    /// The heading it stands under; empty when there is none.
    pub heading: String,
    pub page: usize,
    /// Its place among the sentences of this part of the paper: 0 opens the part.
    pub position: usize,
    pub text: String,
    /// The language of the paper's text, a key of `LANGUAGES` (`language`); empty when it could not be told.
    pub language: String,
}

/// A line with its digits blanked: what a running header looks like from page to page.
fn shape(line: &str) -> String {
    line.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().to_lowercase()
}

/// The lines that come back on page after page: running headers and
/// footers, journal names, download notices.
fn furniture(pages: &[String]) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for page in pages {
        let shapes: HashSet<String> = page.lines().map(str::trim).filter(|l| !l.is_empty() && l.chars().count() <= 120).map(shape).collect();
        for shape in shapes {
            *seen.entry(shape).or_default() += 1;
        }
    }
    let enough = (pages.len() / 4).max(3);
    seen.into_iter().filter(|(_, count)| *count >= enough).map(|(shape, _)| shape).collect()
}

/// What a paper's own spelling says about the words broken at a line's
/// end: the compounds it writes with a hyphen inside a line, and the words
/// it writes whole (the pieces of broken words left out).
struct Lexicon {
    hyphenated: HashSet<String>,
    words: HashSet<String>,
    /// English, where two words of the paper set apart by a hyphen are a compound; German breaks its own compounds anywhere.
    english: bool,
}

/// Words that make a closed compound with what follows them (`through-` /
/// `out`, `bio-` / `technologies`): broken after one, the word is joined.
const CLOSED_PREFIXES: [&str; 26] = ["bio", "pre", "pro", "proto", "re", "un", "in", "dis", "through", "with", "over", "under", "out", "sub", "super", "inter", "intra", "trans", "micro", "macro", "photo", "phyto", "hydro", "auto", "mis", "counter"];

fn lexicon(pages: &[String]) -> Lexicon {
    let (mut hyphenated, mut words) = (HashSet::new(), HashSet::new());
    for page in pages {
        let lines: Vec<&str> = page.lines().map(str::trim).collect();
        for (at, line) in lines.iter().enumerate() {
            let mut tokens: Vec<&str> = line.split_whitespace().collect();
            // The two pieces of a word broken here are not words of the paper.
            if line.ends_with('-') { tokens.pop(); }
            if at > 0 && lines[at - 1].ends_with('-') && !tokens.is_empty() { tokens.remove(0); }
            for token in tokens {
                let token = token.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
                if token.contains('-') && token.chars().filter(|c| c.is_alphabetic()).count() >= 4 { hyphenated.insert(token.clone()); }
                words.extend(token.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string));
            }
        }
    }
    Lexicon { hyphenated, words, english: matches!(language(pages), "en" | "") }
}

impl Lexicon {
    /// Whether `head-` at a line's end and `tail` at the next line's start
    /// are two words joined by a hyphen (`pathogen-associated`,
    /// `PBS3-mediated`) rather than one word broken in two (`protoplast-` / `ing`).
    fn keeps_hyphen(&self, head: &str, tail: &str) -> bool {
        let lower = |text: &str| text.to_lowercase();
        let last = head.rsplit('-').next().unwrap_or(head);
        if self.hyphenated.contains(&lower(&format!("{head}-{tail}"))) { return true }
        if self.words.contains(&lower(&format!("{last}{tail}"))) { return false }
        // A gene, a protein or a number: `PBS3-`, `EDS1-`, `10-` / `fold`.
        if last.chars().any(|c| c.is_ascii_digit()) || last.chars().filter(|c| c.is_uppercase()).count() >= 2 { return true }
        if CLOSED_PREFIXES.contains(&lower(last).as_str()) { return false }
        // Two words the paper writes on their own: `pathogen` and `associated`.
        self.english && last.chars().count() >= 3 && tail.chars().count() >= 3 && self.words.contains(&lower(last)) && self.words.contains(&lower(tail))
    }
}

/// The citation numbers a superscript leaves at the start of the next
/// line (`stresses` / `1,2. Roots have`): how many bytes of the line they take.
fn leading_citation(line: &str) -> usize {
    let mut end = 0;
    let mut digits = 0;
    for (at, c) in line.char_indices() {
        match c {
            '0'..='9' => digits += 1,
            ',' | '-' | '–' if digits > 0 => digits = 0,
            '.' | ',' | ';' | ':' | ')' if digits > 0 => {
                end = at;
                break;
            }
            _ => return 0,
        }
        if digits > 3 { return 0 }
    }
    // Only before a punctuation mark that ends the clause: `3.5 cm` and `47,000 cells` stay.
    let rest = &line[end..];
    if end > 0 && rest.len() > 1 && rest[1..].starts_with(' ') { end } else { 0 }
}

/// A letter of a script written without spaces between its words: Chinese, Japanese.
fn unspaced(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}' | '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' | '々' | '〇')
}

/// A letter of Chinese, Japanese or Korean: what a search finds by pairs of letters.
pub fn is_cjk(c: char) -> bool {
    unspaced(c) || matches!(c, '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}')
}

/// The punctuation of Chinese and Japanese, full-width forms included.
fn cjk_mark(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ff0f}' | '\u{ff1a}'..='\u{ff20}' | '\u{ff3b}'..='\u{ff40}' | '\u{ff5b}'..='\u{ff65}' | '“' | '”' | '‘' | '’' | '…' | '—')
}

/// How long a text reads, in letters of a language that writes spaces: a
/// Chinese, Japanese or Korean letter says about as much as three of them.
pub fn width(text: &str) -> usize {
    text.chars().map(|c| if is_cjk(c) { 3 } else { 1 }).sum()
}

/// How many words a text has; in Chinese and Japanese, which do not space
/// them, a word is about two letters.
fn word_count(text: &str) -> usize {
    text.split_whitespace().map(|token| {
        let letters = token.chars().filter(|c| unspaced(*c)).count();
        if letters == 0 { 1 } else { letters.div_ceil(2) }
    }).sum()
}

/// Where a word broken at a line's end ends and where the next line goes on:
/// `head` is the word before the hyphen (with its own hyphens), `tail` the
/// letters after it.
fn broken_word<'a>(before: &'a str, after: &'a str) -> (&'a str, &'a str) {
    let body = before.strip_suffix('-').unwrap_or(before);
    let start = body.rfind(|c: char| !(c.is_alphanumeric() || c == '-')).map_or(0, |at| at + body[at..].chars().next().map_or(1, char::len_utf8));
    let end = after.find(|c: char| !c.is_alphanumeric()).unwrap_or(after.len());
    (&body[start..], &after[..end])
}

/// Text that goes on after `out` at a line's or a page's end: a hyphen
/// kept or dropped, a space between words, none between Chinese letters.
fn join_text(out: &mut String, next: &str, lexicon: &Lexicon) {
    let first = next.chars().next();
    if out.ends_with('-') && out.len() > 1 && first.is_some_and(char::is_alphanumeric) && out[..out.len() - 1].chars().last().is_some_and(char::is_alphanumeric) {
        let (head, tail) = broken_word(out, next);
        // A hyphen before a capital or a digit, or before `and`, is never a word broken.
        let split = first.is_some_and(char::is_lowercase) && !lexicon.keeps_hyphen(head, tail);
        if matches!(tail, "and" | "or" | "to") {
            out.push(' ');
        } else if split {
            out.pop();
        }
        out.push_str(next);
        return;
    }
    let last = out.chars().last();
    let close = |c: Option<char>| c.is_some_and(|c| unspaced(c) || cjk_mark(c));
    if !out.is_empty() && !(close(last) && close(first)) { out.push(' ') }
    out.push_str(next);
}

/// Lines of one paragraph joined into running text.
fn join_lines(lines: &[&str], lexicon: &Lexicon) -> String {
    let mut out = String::new();
    for line in lines {
        let mut line = *line;
        let last_letter = out.chars().last().is_some_and(char::is_alphabetic);
        if last_letter {
            let skip = leading_citation(line);
            if skip > 0 {
                line = &line[skip..];
                out.push_str(line);
                continue;
            }
        }
        join_text(&mut out, line, lexicon);
    }
    out
}

/// Whether a block of these lines looks like a title standing alone.
fn looks_like_heading(lines: &[&str], text: &str) -> bool {
    let count = width(text);
    if lines.len() > 2 || !(3..=110).contains(&count) { return false }
    let Some(first) = text.chars().next() else { return false };
    // A capital, a number, or a letter of a script without capitals.
    if !(first.is_ascii_digit() || (first.is_alphabetic() && !first.is_lowercase())) { return false }
    if text.ends_with(['.', ',', ';', ':', '?', '。', '，', '；', '：', '？', '、']) && canonical(text).is_none() { return false }
    let letters: usize = text.chars().filter(|c| c.is_alphabetic()).map(|c| if is_cjk(c) { 3 } else { 1 }).sum();
    letters * 10 >= count * 6 && !is_caption(text)
}

/// A figure or table legend.
fn is_caption(text: &str) -> bool {
    let lower = text.to_lowercase();
    let latin = ["fig.", "fig ", "figure ", "table ", "extended data", "supplementary fig", "supplemental fig", "scheme ", "abb.", "abbildung ", "tabelle ", "tabla ", "tableau ", "figura ", "tabela "].iter().any(|start| {
        lower.strip_prefix(start).is_some_and(|rest| rest.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit() || c == 's' || c == 'f' || c == 't'))
    });
    // 图 1、表 2、図 3、그림 4.
    latin || ["图", "表", "図", "附图", "附表", "그림", "표"].iter().any(|start| lower.strip_prefix(start).is_some_and(|rest| rest.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit() || c.eq_ignore_ascii_case(&'s'))))
}

/// The small print of a first or last page: licence, copyright, correspondence, dates.
fn is_notice(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["creative commons", "licensed under", "© the author", "the author(s)", "all rights reserved", "correspondence:", "*correspondence", "received:", "accepted:", "published online", "publisher's note", "publisher’s note", "doi.org/", "open access this article", "these authors contributed equally", "handling editor", "edited by", "reviewed by", "peer review", "ª the author", "收稿日期", "通信作者", "通讯作者", "出版日期", "基金项目", "作者简介", "責任著者", "교신저자"]
        .iter()
        .any(|mark| lower.contains(mark))
        || ["published by ", "an imprint of "].iter().any(|start| lower.starts_with(start))
}

/// The names of parts that some layouts run into the paragraph they head
/// (`Introduction Single-cell RNA sequencing has…`), longest first.
const RUN_IN: [&str; 23] = [
    "Materials and methods", "Materials and Methods", "MATERIALS AND METHODS", "Results and discussion", "Results and Discussion", "Introduction", "INTRODUCTION", "Background", "BACKGROUND", "Conclusions", "CONCLUSIONS", "Conclusion", "CONCLUSION", "Discussion", "DISCUSSION",
    "References", "REFERENCES", "Abstract", "ABSTRACT", "Results", "RESULTS", "Methods", "METHODS",
];

/// A paragraph that opens with the name of a part and goes straight on
/// with a new sentence: the name, and the paragraph without it.
fn run_in(text: &str) -> Option<(&str, &str)> {
    let name = RUN_IN.iter().find(|name| text.starts_with(**name))?;
    let rest = text[name.len()..].strip_prefix(' ')?.trim_start();
    (rest.len() >= 40 && rest.chars().next().is_some_and(char::is_uppercase)).then_some((*name, rest))
}

/// A paragraph that opens with the name of a part set off by brackets or a
/// colon (`【摘要】目的…`, `[Abstract] Objective`, `要旨：…`): the name, and what follows it.
fn run_in_marked(text: &str) -> Option<(&str, &str)> {
    let body = text.trim_start_matches(['【', '[', '〔', '［']);
    let end = body.find(['】', ']', '〕', '］', '：', ':'])?;
    let name = body[..end].trim();
    let part = canonical(name)?;
    // Without brackets only the names of what is not body text: `方法：` may open a paragraph of a structured abstract.
    if name.chars().count() > 20 || !(body.len() < text.len() || (name.chars().any(is_cjk) && matches!(part, "abstract" | SKIP))) { return None }
    Some((name, body[end..].trim_start_matches(['】', ']', '〕', '］', '：', ':']).trim()))
}

/// Running text: long enough, and it says something to its end.
fn is_prose(block: &Block) -> bool {
    !block.heading && width(&block.text) >= 200 && (block.text.contains(". ") || block.text.contains(['。', '．']))
}

/// The paragraphs and headings of a paper's pages, in reading order, with
/// running headers left out, the words broken at line ends put together
/// and a paragraph that runs over a page joined again.
pub fn blocks(pages: &[String]) -> Vec<Block> {
    let furniture = furniture(pages);
    let lexicon = lexicon(pages);
    let mut out: Vec<Block> = vec![];
    for (index, page) in pages.iter().enumerate() {
        let mut lines: Vec<&str> = vec![];
        for line in page.lines().chain([""]) {
            let line = line.trim();
            let is_number = !line.is_empty() && line.chars().all(|c| c.is_ascii_digit());
            if !line.is_empty() && !is_number && !furniture.contains(&shape(line)) {
                lines.push(line);
                continue;
            }
            // A header in the middle of a paragraph does not end it; a blank line does.
            if !line.is_empty() || lines.is_empty() { continue }
            let text = join_lines(&lines, &lexicon);
            let heading = looks_like_heading(&lines, &text);
            lines.clear();
            // What starts in small letters carries on the paragraph left open before the page or column ended.
            let first = text.chars().next();
            let continues = !heading && first.is_some_and(char::is_lowercase);
            let closed = |text: &str| text.ends_with(['.', '!', '?', ':', '”', '"', ')', '。', '！', '？', '：', '）', '」']);
            // The last paragraph of running text: a notice is passed over, and so is a finished legend at the foot of
            // an earlier page; on this page what follows a legend is more of the legend (its second column).
            let open = out.iter().rposition(|b| (is_prose(b) || (!b.heading && width(&b.text) >= 80)) && !is_notice(&b.text) && !(is_caption(&b.text) && closed(&b.text) && b.page <= index)).filter(|at| out.len() - at <= 12 && !closed(&out[*at].text));
            // In Chinese or Japanese a paragraph cut by the page goes on without a mark: a letter after a letter.
            let continues = continues || (!heading && first.is_some_and(unspaced) && open.is_some_and(|at| out[at].text.chars().last().is_some_and(|c| unspaced(c) || c == '，' || c == '、')));
            match open.filter(|_| continues) {
                Some(at) => {
                    join_text(&mut out[at].text, &text, &lexicon);
                }
                None => match run_in(&text).filter(|_| !heading).or_else(|| run_in_marked(&text)) {
                    Some((name, rest)) => {
                        out.push(Block { page: index + 1, text: name.into(), heading: true });
                        if !rest.is_empty() { out.push(Block { page: index + 1, text: rest.into(), heading: false }) }
                    }
                    None => out.push(Block { page: index + 1, text, heading }),
                },
            }
        }
    }
    out
}

/// A Chinese numeral, as in `一、引言`.
fn is_numeral(c: char) -> bool {
    "一二三四五六七八九十".contains(c)
}

/// The part of a paper a heading names: a key of `SECTIONS`, or `SKIP` for
/// the back matter. `None` for a heading that is the paper's own.
pub fn canonical(heading: &str) -> Option<&'static str> {
    // `2.1 Materials and Methods`, `II. RESULTS`, `R E S U L T S`, `STAR★METHODS`.
    let mut name: String = heading.replace('ﬁ', "fi").replace('ﬂ', "fl").to_lowercase().chars().map(|c| if c.is_alphabetic() || c == '&' { c } else { ' ' }).collect();
    if name.split_whitespace().count() >= 4 && name.split_whitespace().all(|word| word.chars().count() == 1) { name = name.replace(' ', "") }
    // `引 言`, `緒　言`, `서 론`: a title spaced out letter by letter.
    let spaced = name.split_whitespace().filter(|word| !word.chars().all(is_numeral)).collect::<Vec<_>>();
    if spaced.len() >= 2 && spaced.iter().all(|word| word.chars().count() == 1 && word.chars().all(is_cjk)) { name = spaced.concat() }
    let words: Vec<&str> = name.split_whitespace().skip_while(|word| ["i", "ii", "iii", "iv", "v", "vi"].contains(word) || word.chars().all(is_numeral)).collect();
    let name = words.join(" ").replace(" & ", " and ");
    const NAMES: [(&str, &[&str]); 6] = [
        ("abstract", &["abstract", "summary", "in brief", "highlights", "zusammenfassung", "kurzfassung", "résumé", "resumen", "resumo", "riassunto", "摘要", "提要", "要旨", "要約", "抄録", "초록", "요약"]),
        ("introduction", &[
            "introduction", "background", "main", "einleitung", "einführung", "hintergrund", "introducción", "introdução", "introduzione", "antecedentes", "引言", "前言", "绪论", "緒論", "导言", "研究背景", "背景", "はじめに", "緒言", "序論", "서론", "서 론", "배경",
        ]),
        ("methods", &["methods", "method", "materials and methods", "material and methods", "methods and materials", "experimental procedures", "experimental section", "online methods", "star methods", "method details", "methodology", "materials and method", "experimental model and subject details", "quantification and statistical analysis",
            "material und methoden", "methoden", "methodik", "matériel et méthodes", "matériels et méthodes", "méthodes", "materiales y métodos", "material y métodos", "métodos", "metodología", "material e métodos", "materiais e métodos", "metodologia", "materiali e metodi", "metodi",
            "材料与方法", "材料和方法", "材料與方法", "资料与方法", "对象与方法", "研究方法", "实验方法", "试验方法", "方法", "材料", "材料と方法", "対象と方法", "재료 및 방법", "대상 및 방법", "연구 방법", "방법"]),
        ("results", &[
            "results", "result", "results and discussion", "results and discussions", "findings", "ergebnisse", "ergebnisse und diskussion", "résultats", "résultats et discussion", "resultados", "resultados y discusión", "resultados e discussão", "risultati",
            "结果", "結果", "结果与分析", "结果和分析", "结果与讨论", "结果和讨论", "实验结果", "研究结果", "結果と考察", "결과", "연구 결과",
        ]),
        ("discussion", &["discussion", "discussions", "conclusion", "conclusions", "concluding remarks", "discussion and conclusion", "discussion and conclusions", "conclusions and perspectives", "conclusion and perspectives", "conclusions and future perspectives", "conclusion and future perspectives", "summary and outlook", "perspectives", "outlook", "limitations of the study",
            "diskussion", "schlussfolgerung", "schlussfolgerungen", "fazit", "ausblick", "conclusions et perspectives", "discusión", "conclusiones", "discussão", "conclusão", "conclusões", "considerações finais", "discussione", "conclusioni",
            "讨论", "討論", "结论", "結論", "讨论与结论", "结论与讨论", "结语", "結語", "小结", "展望", "结论与展望", "总结与展望", "总结", "考察", "まとめ", "おわりに", "고찰", "논의", "결론", "고찰 및 결론"]),
        (SKIP, &[
            "references", "reference", "literature cited", "bibliography", "acknowledgements", "acknowledgments", "acknowledgement", "acknowledgment", "funding", "author contributions", "authors contributions", "contributions", "data availability",
            "data availability statement", "code availability", "data and code availability", "availability of data and materials", "competing interests", "conflict of interest", "conflicts of interest", "declaration of interests", "declarations", "ethics declarations",
            "supplementary information", "supplemental information", "supplementary material", "supplementary materials", "supporting information", "supplementary data", "additional information", "abbreviations", "reporting summary", "online content", "accession numbers",
            "key resources table", "resource availability", "author information", "publisher s note", "keywords", "key words", "generative ai statement",
            "palabras clave", "palavras chave", "schlüsselwörter", "mots clés", "parole chiave", "danksagung", "literatur", "literaturverzeichnis", "remerciements", "références", "bibliographie", "agradecimientos", "referencias", "bibliografía", "agradecimentos",
            "referências", "bibliografia", "ringraziamenti", "interessenkonflikt", "conflicto de intereses", "conflito de interesses", "financiamiento", "financiamento", "förderung",
            "参考文献", "參考文獻", "致谢", "致謝", "关键词", "關鍵詞", "关键字", "基金项目", "作者简介", "作者贡献", "利益冲突", "附录", "中图分类号", "文献标志码", "文献标识码", "引用文献", "文献", "谢辞", "謝辞", "キーワード",
            "참고문헌", "감사의 글", "사사", "주요 검색어", "중심 단어", "이해관계", "key words",
        ]),
    ];
    NAMES.iter().find(|(_, names)| names.contains(&name.as_str())).map(|(key, _)| *key)
}

/// The part of the paper each block belongs to, told by rule: a heading
/// that names a part starts it. In a paper that never says "Introduction"
/// (Nature's way), the running text before the first heading of its own is
/// the introduction, and what follows it the results. `abstract_text` is
/// the abstract of the paper's record: the paragraph that repeats it is the
/// abstract. Front matter, legends and back matter are `SKIP`.
pub fn assign(blocks: &[Block], abstract_text: &str) -> Vec<&'static str> {
    assign_sure(blocks, abstract_text).0
}

/// `assign`, and for each block whether the rules are sure of it: it
/// stands under a heading that names its part, or is plainly not body
/// text. The rest they only guess at (text before any such heading, a
/// paper's own headings in a review), and a model may know better.
pub fn assign_sure(blocks: &[Block], abstract_text: &str) -> (Vec<&'static str>, Vec<bool>) {
    let abstract_start: String = bare(abstract_text).chars().take(60).collect();
    let named_introduction = blocks.iter().any(|b| b.heading && canonical(&b.text) == Some("introduction"));
    let named_results = blocks.iter().any(|b| b.heading && canonical(&b.text) == Some("results"));
    // A review has neither methods nor results: its own headings are its body.
    let review = !named_results && !blocks.iter().any(|b| b.heading && canonical(&b.text) == Some("methods"));
    // Before the first part: front matter, unless the paper has no "Introduction" to wait for.
    let mut section = SKIP;
    let (mut implicit, mut guessed) = (false, true);
    let (mut labels, mut sure) = (vec![], vec![]);
    // Under a heading "Abstract": the part it interrupts (a side column's abstract) and the room it has left.
    let mut interrupted: Option<(&'static str, bool, usize)> = None;
    // Where the introduction taken for one without a heading starts.
    let mut implicit_from = 0;
    for block in blocks {
        if let Some((before, was_guessed, room)) = interrupted {
            // An abstract is a paragraph or a few: what goes on past that, or past a heading of the paper's own, is the text again.
            let past = room == 0 || (block.heading && canonical(&block.text).is_none());
            if past { (section, guessed, interrupted) = (before, was_guessed, None) }
        }
        if block.heading {
            match canonical(&block.text) {
                Some("abstract") => {
                    // What was taken for an introduction before an abstract was front matter (an English address after a Chinese abstract).
                    if implicit && section == "introduction" {
                        for label in &mut labels[implicit_from..] {
                            if *label == "introduction" { *label = SKIP }
                        }
                        (section, implicit) = (SKIP, false);
                    }
                    interrupted = Some((if section == "abstract" { SKIP } else { section }, guessed, ABSTRACT_WIDTH));
                    (section, guessed) = ("abstract", false);
                }
                Some(part) => (section, implicit, guessed, interrupted) = (part, false, false, None),
                None if section == "introduction" && review => (section, guessed) = ("other", true),
                // The first heading of the paper's own after an unnamed introduction opens the results.
                None if implicit && section == "introduction" && !named_results => section = "results",
                None => {}
            }
            labels.push(SKIP);
            sure.push(true);
            continue;
        }
        if is_caption(&block.text) || is_notice(&block.text) {
            labels.push(SKIP);
            sure.push(true);
            continue;
        }
        if abstract_start.chars().count() >= 40 && bare(&block.text).contains(&abstract_start) {
            labels.push("abstract");
            sure.push(true);
            continue;
        }
        // Running text in sentences (an address with a full stop in it is not).
        if section == SKIP && !named_introduction && !implicit && is_prose(block) && labels.iter().all(|l| *l == SKIP || *l == "abstract") && !block.text.contains('@') && split_sentences(&block.text).iter().filter(|s| usable(s)).count() >= 2 {
            (section, implicit, guessed, implicit_from) = ("introduction", true, true, labels.len());
        }
        if let Some((_, _, room)) = interrupted.as_mut() { *room = room.saturating_sub(width(&block.text)) }
        labels.push(if section == "abstract" && !is_prose(block) { SKIP } else { section });
        sure.push(!guessed);
    }
    (labels, sure)
}

/// How long an abstract under its own heading runs at most, in `width`: a
/// structured one of 300 words, and some.
const ABSTRACT_WIDTH: usize = 2600;

/// How many paragraphs of running text the rules only guessed at: with a
/// few, a model's reading of the outline is worth asking for.
pub fn open_paragraphs(blocks: &[Block], sure: &[bool]) -> usize {
    blocks.iter().zip(sure).filter(|(block, sure)| !**sure && is_prose(block)).count()
}

/// From this many guessed paragraphs on, a full text is one for a model to
/// look at: fewer are the front matter of a paper whose parts are named.
pub const OPEN_ENOUGH: usize = 4;

/// How many paragraphs of running text a text has (`is_prose`).
pub fn prose_paragraphs(blocks: &[Block]) -> usize {
    blocks.iter().filter(|block| is_prose(block)).count()
}

/// Whether a model's reading of the outline is worth asking for a text
/// whose rules guessed `open` of its `paragraphs`: many guessed, or a good
/// share of a short piece (a commentary of three paragraphs, none under a
/// heading). `paragraphs` 0 is a structure made before it was counted.
pub fn worth_asking(open: usize, paragraphs: usize) -> bool {
    open >= OPEN_ENOUGH || (open >= 2 && paragraphs > 0 && open * 3 >= paragraphs)
}

/// The rules' labels where they are sure, the model's elsewhere.
pub fn merge(rules: &[&'static str], sure: &[bool], model: &[&'static str]) -> Vec<&'static str> {
    rules.iter().zip(sure).zip(model).map(|((rule, sure), model)| if *sure { *rule } else { *model }).collect()
}

/// What a model is told when it is asked to sort a paper's blocks into parts.
pub const OUTLINE_SYSTEM: &str = "你在整理一篇学术论文的版面提取结果。提取结果按阅读顺序分成了编号的块：标题行（H）和正文段落（P），每块只给出开头。你的任务是判断每一块属于论文的哪个部分。\n\
部分只有这几种：abstract（摘要）、introduction（导言，含背景）、methods（材料与方法）、results（结果）、discussion（讨论，含结论）、other（属于正文但不属于以上任何一种，例如综述的各个主题章节）。\n\
不属于正文的块不要列出：题目、作者、单位、通讯信息、收稿日期、页眉页脚、图注和表注、表格内容、致谢、基金、作者贡献、数据可用性、参考文献、补充材料说明。\n\
注意：摘要常常没有标题，是题目和作者之后、正文之前概述全文的那一段，它是 abstract 而不是 introduction。有的期刊（如 Nature）导言没有标题，直接跟在摘要后面；结果部分常由论文自己的小标题组成；方法可能排在参考文献之后。小标题下的段落归入它所在的部分。\n\
只输出结果，每行一个部分，写出属于它的块编号范围，例如：\nintroduction: 6-11\nresults: 12-58, 61-64\nmethods: 80-112\n不要解释。";

/// How much of a block the model sees.
const OUTLINE_CHARS: usize = 110;
/// The blocks one request lists; a longer paper is cut there and says so.
pub const OUTLINE_BLOCKS: usize = 900;

/// The blocks as a numbered outline for the model: `12 p3 H Results`.
pub fn outline_prompt(title: &str, blocks: &[Block]) -> String {
    let mut out = format!("论文题目：{title}\n\n");
    for (index, block) in blocks.iter().take(OUTLINE_BLOCKS).enumerate() {
        let count = block.text.chars().count();
        let start: String = block.text.chars().take(OUTLINE_CHARS).collect();
        out.push_str(&format!("{index} p{} {} {start}{}\n", block.page, if block.heading { "H" } else { "P" }, if count > OUTLINE_CHARS { format!("…（{count} 字符）") } else { String::new() }));
    }
    if blocks.len() > OUTLINE_BLOCKS { out.push_str("（后面的块没有列出）\n") }
    out
}

/// The model's answer to `outline_prompt`, as a label for each of the
/// `count` blocks. A block it did not list is left out; headings never
/// carry sentences. An answer that names no block at all is an error.
pub fn parse_outline(reply: &str, blocks: &[Block], abstract_text: &str) -> Result<Vec<&'static str>, String> {
    let abstract_start: String = bare(abstract_text).chars().take(60).collect();
    let mut labels = vec![SKIP; blocks.len()];
    let mut named = 0;
    for line in reply.split(['\n', ';', '；']) {
        let line = line.trim().trim_start_matches(['-', '*', ' ']).to_lowercase();
        let Some((key, _)) = SECTIONS.iter().find(|(key, name)| line.starts_with(key) || line.starts_with(name)) else { continue };
        let ranges = line.split_once([':', '：']).map_or("", |(_, ranges)| ranges);
        for range in ranges.split([',', '，', '、']) {
            let mut ends = range.split(['-', '–', '~', '—']).map(|n| n.trim().parse::<usize>());
            let (Some(Ok(from)), to) = (ends.next(), ends.next()) else { continue };
            let to = match to {
                Some(Ok(to)) => to,
                Some(Err(_)) => continue,
                None => from,
            };
            for index in from..=to.min(blocks.len().saturating_sub(1)) {
                if index < blocks.len() && !blocks[index].heading {
                    labels[index] = key;
                    named += 1;
                }
            }
        }
    }
    if named == 0 { return Err("模型的回答里没有可用的分节结果。".into()) }
    // A legend stays out whatever part its neighbours are in, and the abstract is the one the record has.
    for (label, block) in labels.iter_mut().zip(blocks) {
        if is_caption(&block.text) { *label = SKIP }
        if *label != SKIP && abstract_start.chars().count() >= 40 && bare(&block.text).contains(&abstract_start) { *label = "abstract" }
    }
    Ok(labels)
}

/// The words a full stop after which does not end a sentence; besides
/// English, those of German, French, Spanish, Portuguese and Italian papers.
pub(crate) const ABBREVIATIONS: &[&str] = &[
    "al", "fig", "figs", "e.g", "i.e", "vs", "cf", "ca", "approx", "ref", "refs", "eq", "eqs", "no", "nos", "dr", "prof", "sp", "spp", "var", "supp", "suppl", "tab", "st", "inc", "ltd", "co", "ed", "eds", "vol",
    "abb", "vgl", "bzw", "z.b", "d.h", "u.a", "usw", "bspw", "ggf", "inkl", "nr", "evtl", "sog", "jh", "mio", "mrd", "s.o", "s.u", "u.ä", "z.t", "env", "ex", "p.ex", "éd", "ej", "p.ej", "aprox", "pág", "págs", "núm", "sr", "sra", "dra", "pp", "tabla", "ecc",
];

/// A text cut into sentences. A full stop after an abbreviation, an
/// initial or inside a number does not end one.
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut start = 0;
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        // Chinese and Japanese end a sentence with a mark of their own, and need no space after it.
        let full_stop = matches!(c, '。' | '！' | '？') || (c == '．' && !(at > 0 && chars[at - 1].is_ascii_digit() && chars.get(at + 1).is_some_and(char::is_ascii_digit)));
        if matches!(c, '.' | '!' | '?') || full_stop {
            // Closing marks belong to the sentence they close.
            let mut end = at + 1;
            while end < chars.len() && matches!(chars[end], ')' | ']' | '"' | '”' | '’' | '\'' | '）' | '」' | '』' | '】') { end += 1 }
            let after_space = full_stop || (end < chars.len() && chars[end].is_whitespace());
            let next = chars[end..].iter().copied().find(|c| !c.is_whitespace());
            // A capital, a number, or a letter of a script without capitals (Korean ends with `. ` too).
            let starts_new = full_stop || next.is_some_and(|n| n.is_ascii_digit() || (n.is_alphabetic() && !n.is_lowercase()) || matches!(n, '(' | '[' | '“' | '"' | '（' | '「' | '【'));
            let word: String = chars[start..at].iter().rev().take_while(|c| !c.is_whitespace() && **c != '(').collect::<Vec<_>>().into_iter().rev().collect();
            let word = word.to_lowercase();
            let abbreviation = c == '.' && (ABBREVIATIONS.contains(&word.trim_start_matches(['[', '“', '"'])) || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic)) || word.ends_with("et al"));
            if (end == chars.len() || (after_space && starts_new)) && !abbreviation {
                let sentence: String = chars[start..end].iter().collect();
                let sentence = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
                if !sentence.is_empty() { out.push(sentence) }
                start = end;
                at = end;
                continue;
            }
        }
        at += 1;
    }
    let rest: String = chars[start..].iter().collect();
    let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    if !rest.is_empty() { out.push(rest) }
    out
}

/// Whether a sentence is one to show a writer: whole, in words, of a
/// length a sentence has. Table rows, addresses and fragments are not.
pub fn usable(sentence: &str) -> bool {
    // Lengths in `width`: a sentence of Chinese, Japanese or Korean is as long by what it says.
    let count = width(sentence);
    if !(40..=600).contains(&count) || word_count(sentence) < 6 { return false }
    let letters = width(&sentence.chars().filter(|c| c.is_alphabetic()).collect::<String>());
    let first = sentence.chars().next().unwrap_or(' ');
    // It starts as a sentence does: a capital, a number, or a letter of a script without capitals.
    let starts = first.is_ascii_digit() || (first.is_alphabetic() && !first.is_lowercase());
    letters * 10 >= count * 7 && starts && sentence.ends_with(['.', '?', '!', ')', '”', '"', '。', '！', '？', '）', '」', '』', '．']) && !sentence.contains('@') && !apparatus(sentence)
}

/// Whether a sentence is a table's or a figure's apparatus rather than the
/// paper's prose: a note under a table (`Note:`, `Abbreviations:`, `注：`),
/// or the cells of a table read as one line, which run to many bare numbers
/// (`Stele 0.73 Cortex 0.68 …`). Numbers in prose come with their units and
/// punctuation, so they are not counted. Also checked when candidates are
/// offered, so sentences kept by earlier builds are left out without sorting the texts again.
pub fn apparatus(sentence: &str) -> bool {
    let opening = sentence.trim_start().to_lowercase();
    if ["note:", "notes:", "note.", "abbreviations:", "abbreviation:", "source:", "sources:", "注：", "注:"].iter().any(|start| opening.starts_with(start)) { return true }
    let tokens: Vec<&str> = sentence.split_whitespace().collect();
    let bare = tokens.iter().filter(|token| token.chars().any(|c| c.is_ascii_digit()) && token.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '–')).count();
    bare >= 5 && bare * 10 >= tokens.len()
}

/// The sentences of the blocks that are kept, with their part, the
/// heading they stand under and their place in the part. The abstract is
/// not among them: a paper's record has it.
pub fn sentences(blocks: &[Block], labels: &[&str]) -> Vec<Sentence> {
    let mut out = vec![];
    let mut heading = String::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for (block, label) in blocks.iter().zip(labels) {
        if block.heading {
            heading = if canonical(&block.text).is_some() { String::new() } else { block.text.clone() };
            continue;
        }
        if *label == SKIP || *label == "abstract" { continue }
        for text in split_sentences(&block.text).into_iter().filter(|s| usable(s)) {
            let position = positions.entry(label).or_default();
            out.push(Sentence { section: label.to_string(), heading: heading.clone(), page: block.page, position: *position, text, language: String::new() });
            *position += 1;
        }
    }
    out
}

/// What the rules make of a paper's pages.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ByRule {
    /// Sorted into parts, each with the text's language.
    pub sentences: Vec<Sentence>,
    /// How many paragraphs the rules only guessed at (`open_paragraphs`).
    pub open: usize,
    /// How many paragraphs of running text there are (`prose_paragraphs`).
    pub paragraphs: usize,
    /// `language`.
    pub language: &'static str,
}

/// The sentences of a paper's pages, sorted into parts by rule.
pub fn by_rule(pages: &[String], abstract_text: &str) -> ByRule {
    let blocks = blocks(pages);
    let (labels, sure) = assign_sure(&blocks, abstract_text);
    let language = body_language(&blocks, &labels).unwrap_or_else(|| language(pages));
    let mut sentences = sentences(&blocks, &labels);
    for sentence in &mut sentences {
        sentence.language = language.to_string();
    }
    ByRule { sentences, open: open_paragraphs(&blocks, &sure), paragraphs: prose_paragraphs(&blocks), language }
}

/// The languages a full text is told to be in, as kept and as shown.
pub const LANGUAGES: [(&str, &str); 10] = [("en", "英文"), ("zh", "中文"), ("ja", "日文"), ("ko", "韩文"), ("de", "德文"), ("fr", "法文"), ("es", "西班牙文"), ("pt", "葡萄牙文"), ("it", "意大利文"), ("ru", "俄文")];

/// The commonest words of each language written in Latin letters, those it
/// shares with no other of them where it can.
const COMMON_WORDS: [(&str, &[&str]); 6] = [
    ("en", &["the", "and", "of", "to", "is", "that", "with", "for", "are", "was", "were", "this"]),
    ("de", &["der", "die", "und", "den", "das", "ist", "nicht", "mit", "von", "zu", "werden", "sich", "auf", "eine"]),
    ("fr", &["les", "des", "et", "est", "une", "dans", "pour", "sur", "du", "au", "aux", "sont", "le"]),
    ("es", &["el", "los", "las", "del", "y", "por", "una", "para", "con", "se", "al", "como", "más"]),
    ("pt", &["os", "das", "dos", "não", "uma", "para", "com", "em", "do", "da", "ao", "como", "são"]),
    ("it", &["il", "della", "che", "di", "per", "gli", "delle", "sono", "nel", "dei", "alla", "anche"]),
];

/// The language of a paper's running text, the paragraphs kept for its
/// parts: an English abstract, references and legends of a Chinese paper
/// do not count. `None` when too little was kept to tell.
pub fn body_language(blocks: &[Block], labels: &[&str]) -> Option<&'static str> {
    let body: Vec<String> = blocks.iter().zip(labels).filter(|(block, label)| !block.heading && **label != SKIP && **label != "abstract").map(|(block, _)| block.text.clone()).collect();
    if body.iter().map(|text| width(text)).sum::<usize>() < 2000 { return None }
    Some(language(&body)).filter(|language| !language.is_empty())
}

/// The language a text is written in, a key of `LANGUAGES`, told cheaply:
/// by its script (Chinese, Japanese by its kana, Korean, Russian), and in
/// Latin letters by its commonest words. Empty when there is too little to tell.
/// The language of a stored text is `Sentence::language` and `StructureInfo::language`.
pub fn language(pages: &[String]) -> &'static str {
    let (mut han, mut kana, mut hangul, mut cyrillic, mut latin) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for c in pages.iter().flat_map(|page| page.chars()) {
        match c {
            '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' => kana += 1,
            '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}' => hangul += 1,
            '\u{0400}'..='\u{04ff}' => cyrillic += 1,
            c if unspaced(c) => han += 1,
            c if c.is_alphabetic() => latin += 1,
            _ => {}
        }
    }
    // The references and an abstract in English are in every such paper: a letter of these scripts weighs three.
    let cjk = han + kana + hangul;
    if cjk >= 50 && cjk * 3 >= latin {
        return if hangul * 2 >= han + kana { "ko" } else if kana * 10 >= cjk { "ja" } else { "zh" };
    }
    if cyrillic >= 200 && cyrillic >= latin { return "ru" }
    let mut counts = [0usize; COMMON_WORDS.len()];
    for word in pages.iter().flat_map(|page| page.split(|c: char| !c.is_alphabetic())).filter(|word| !word.is_empty() && word.len() <= 6).take(200_000) {
        let word = word.to_lowercase();
        for (count, (_, common)) in counts.iter_mut().zip(COMMON_WORDS) {
            if common.contains(&word.as_str()) { *count += 1 }
        }
    }
    let (best, most) = counts.iter().enumerate().max_by_key(|(_, count)| **count).map(|(at, count)| (at, *count)).unwrap_or_default();
    if most < 20 { return "" }
    COMMON_WORDS[best].0
}

/// What a person is writing right now, for finding sentences like it:
/// the paragraph the caret is in, up to the caret; when the caret stands
/// at the paragraph's start, the paragraph's beginning.
pub fn writing_at(text: &str, caret: usize) -> String {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    let start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let end = text[caret..].find('\n').map_or(text.len(), |at| caret + at);
    let written = text[start..caret].trim();
    if written.is_empty() {
        text[start..end].trim().chars().take(WRITING_CHARS).collect()
    } else {
        // The last sentences count: the start of a long paragraph was written a while ago.
        let skip = written.chars().count().saturating_sub(WRITING_CHARS);
        written.chars().skip(skip).collect()
    }
}

const WRITING_CHARS: usize = 320;

/// Whether a text is (partly) in Chinese: the library's sentences are not,
/// so it is put into English search words before it is looked for.
pub fn in_chinese(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// What a model is told to turn something written in Chinese into search words.
pub const KEYWORDS_SYSTEM: &str = "用户正在写英文学术论文，下面是他想表达的意思（中文或中英混杂）。把它改写成用来检索相似英文句子的关键词：给出 6 到 12 个英文单词或短语，覆盖其中的概念、对象和论述方式（例如 remains unclear、in contrast、we hypothesized）。术语用该领域论文里通行的写法。只输出关键词，用空格分隔，不要解释。";

const STOP_WORDS: [&str; 70] = [
    "the", "and", "for", "are", "was", "were", "with", "that", "this", "from", "have", "has", "had", "which", "what", "how", "why", "who", "does", "did", "can", "could", "about", "into", "their", "there", "these", "those", "any", "all", "not", "but", "its", "our", "you",
    "your", "they", "them", "than", "then", "been", "being", "of", "in", "on", "to", "is", "as", "by", "be", "we", "it", "an", "at", "or", "also", "such", "may", "more", "most", "between", "both", "however", "here", "using", "used", "use", "study", "show", "shown",
];

/// Words as common in the other languages of `COMMON_WORDS` as `the` is in
/// English, and no word of English.
const OTHER_STOP_WORDS: [&str; 52] = [
    "der", "das", "und", "den", "dem", "ist", "nicht", "mit", "von", "zu", "werden", "wird", "sich", "auf", "eine", "einer", "eines", "für", "auch", "les", "des", "et", "est", "une", "dans", "pour", "sur", "du", "aux", "sont", "le", "la", "el", "los", "las", "del", "por", "una",
    "para", "con", "se", "al", "como", "os", "dos", "das", "não", "em", "da", "il", "della", "che",
];

/// A word reduced to what its forms share: `regulates`, `regulated` and
/// `regulating` are one word to a search.
fn stem(word: &str) -> String {
    let count = word.chars().count();
    if !word.is_ascii() { return word.to_string() }
    for (suffix, least, put) in [("ies", 5, "y"), ("sses", 6, "ss"), ("ions", 7, ""), ("ion", 6, ""), ("ing", 6, ""), ("ed", 5, ""), ("es", 5, ""), ("s", 4, "")] {
        if count >= least && word.ends_with(suffix) && !(suffix == "s" && (word.ends_with("ss") || word.ends_with("us") || word.ends_with("is"))) {
            let mut stem = format!("{}{put}", &word[..word.len() - suffix.len()]);
            // `regulat(e)`: the forms with and without the final e meet.
            if stem.ends_with('e') && stem.len() > 4 { stem.pop(); }
            return stem;
        }
    }
    if count > 4 && word.ends_with('e') { word[..word.len() - 1].to_string() } else { word.to_string() }
}

/// The words a sentence is found by: lowercase, without the commonest
/// ones, each reduced to its stem. Chinese, Japanese and Korean are found
/// by each pair of letters that stand side by side (`钙离子` gives `钙离`,
/// `离子`): there is no space to tell their words by, and a pair is most often a word or a part of one.
pub fn words(text: &str) -> Vec<String> {
    let mut out = vec![];
    for piece in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = piece.chars().collect();
        let mut at = 0;
        while at < chars.len() {
            let cjk = is_cjk(chars[at]);
            let end = chars[at..].iter().position(|c| is_cjk(*c) != cjk).map_or(chars.len(), |n| at + n);
            if cjk {
                out.extend(chars[at..end].windows(2).map(|pair| pair.iter().collect::<String>()));
            } else {
                let word: String = chars[at..end].iter().collect();
                if word.chars().count() >= 2 && !STOP_WORDS.contains(&word.as_str()) && !OTHER_STOP_WORDS.contains(&word.as_str()) { out.push(stem(&word)) }
            }
            at = end;
        }
    }
    out
}

/// The sentences one paper may place among the candidates: a paper that
/// says it all would otherwise fill the list.
const PER_PAPER: usize = 3;

/// The sentences that say most of what `query` says, the best first:
/// `(index into texts, score)`. Words rare among the sentences count for
/// more, a long sentence gets no credit for its length (BM25). `owners`
/// names the paper of each sentence, so no paper takes more than a few
/// places. An empty query finds nothing.
pub fn rank(texts: &[&str], owners: &[&str], query: &str, limit: usize) -> Vec<(usize, f32)> {
    let wanted: Vec<String> = {
        let mut seen = HashSet::new();
        words(query).into_iter().filter(|word| seen.insert(word.clone())).collect()
    };
    if wanted.is_empty() || texts.is_empty() { return vec![] }
    let bags: Vec<Vec<String>> = texts.iter().map(|text| words(text)).collect();
    let average = bags.iter().map(Vec::len).sum::<usize>() as f32 / bags.len() as f32;
    let mut scored: Vec<(usize, f32)> = vec![];
    let weights: Vec<f32> = wanted
        .iter()
        .map(|word| {
            let holders = bags.iter().filter(|bag| bag.contains(word)).count() as f32;
            (1. + (bags.len() as f32 - holders + 0.5) / (holders + 0.5)).ln()
        })
        .collect();
    for (index, bag) in bags.iter().enumerate() {
        let mut score = 0.;
        for (word, weight) in wanted.iter().zip(&weights) {
            let hits = bag.iter().filter(|w| *w == word).count() as f32;
            if hits > 0. { score += weight * hits * 2.2 / (hits + 1.2 * (0.25 + 0.75 * bag.len() as f32 / average.max(1.))) }
        }
        if score > 0. { scored.push((index, score)) }
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut taken: HashMap<&str, usize> = HashMap::new();
    scored
        .into_iter()
        .filter(|(index, _)| {
            let count = taken.entry(owners.get(*index).copied().unwrap_or("")).or_default();
            *count += 1;
            *count <= PER_PAPER
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(text: &str) -> String {
        text.to_string()
    }

    const INTRO: &str = "Crops such as rice thrive in arable soils that show natural heterogeneity. The heterogeneous nature of soils is characterized by uneven distributions of nutrients, water and organic content. However, our understanding of how roots respond to complex soil environments at a cellular level remains limited.";
    const RESULT: &str = "Protoplasts of rice primary roots were adopted to generate a high-quality reference dataset. Using the 10X Genomics platform, we profiled more than 47,000 root cells across ten sets of seedlings. All datasets underwent processing with the same pipeline (Fig. 1a,b).";

    #[test]
    fn blocks_of_pages() {
        let pages = [
            page("Journal of Roots 12 (2024) 100\n\nArticle\n\nA study of roots\n\nThe root is long and it is known that roots have evolved diverse strate-\ngies to tackle the variability in soil condi-\ntions\n1,2. However, single-\ncell work on the single-cell level is rare and the understanding of\n47,000 cells remains"),
            page("Journal of Roots 12 (2024) 101\n\nlimited to this day.\n\nResults\n\nFig. 1 | A legend of the first figure, long enough.\n\n101"),
            page("Journal of Roots 12 (2024) 102\n\nDiscussion More text here, after a name of a part run into it.\n\nResults from elsewhere stay."),
            page("Journal of Roots 12 (2024) 103\n\nThe end."),
        ];
        let blocks = blocks(&pages);
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Article",
                "A study of roots",
                "The root is long and it is known that roots have evolved diverse strategies to tackle the variability in soil conditions. However, single-cell work on the single-cell level is rare and the understanding of 47,000 cells remains limited to this day.",
                "Results",
                "Fig. 1 | A legend of the first figure, long enough.",
                "Discussion",
                "More text here, after a name of a part run into it.",
                "Results from elsewhere stay.",
                "The end."
            ]
        );
        assert_eq!(blocks.iter().map(|b| b.heading).collect::<Vec<_>>(), [true, true, false, true, false, true, false, false, false]);
        assert_eq!((blocks[2].page, blocks[3].page), (1, 2));
    }

    #[test]
    fn headings_that_name_a_part() {
        for (heading, part) in [("1. Introduction", "introduction"), ("MATERIALS AND METHODS", "methods"), ("2.1 Materials & Methods", "methods"), ("R E S U L T S", "results"), ("STAR★METHODS", "methods"), ("Results and Discussion", "results"), ("IV. CONCLUSIONS", "discussion"), ("References", SKIP), ("Acknowledgements", SKIP)] {
            assert_eq!(canonical(heading), Some(part), "{heading}");
        }
        assert_eq!(canonical("Rice root scRNA-seq and spatial transcriptomic atlas"), None);
        assert_eq!(canonical("Methods for the future of results"), None);
    }

    fn block(text: &str) -> Block {
        Block { page: 1, text: text.into(), heading: !text.contains(". ") && text.len() < 100 }
    }

    #[test]
    fn parts_by_rule() {
        // A paper that names its parts.
        let named = [block("A study"), block(INTRO), block("Introduction"), block(INTRO), block("Results"), block("A heading of its own"), block(RESULT), block("Fig. 1 | Legend. With two sentences."), block("Discussion"), block(INTRO), block("References"), block(INTRO), block("Methods"), block(RESULT)];
        assert_eq!(assign(&named, ""), [SKIP, SKIP, SKIP, "introduction", SKIP, SKIP, "results", SKIP, SKIP, "discussion", SKIP, SKIP, SKIP, "methods"]);
        // One that does not: the abstract is known by the record, the text after it is the introduction until the first heading.
        let summary = "Land plants thrive in soils showing vastly different properties and environmental stresses. Root systems can adapt to contrasting soil conditions, yet how their responses are programmed remains unclear. We showed major expression changes in outer root cell types.";
        let nature = [block("Article"), block(summary), block(INTRO), block(INTRO), block("Rice root atlas"), block(RESULT), block("Discussion"), block(INTRO), block("Methods"), block(RESULT)];
        assert_eq!(assign(&nature, &summary.replace("thrive in", "thrive  in\n")), [SKIP, "abstract", "introduction", "introduction", SKIP, "results", SKIP, "discussion", SKIP, "methods"]);
        // The rules are sure of what stands under a named heading, and guess at the rest.
        let (labels, sure) = assign_sure(&nature, summary);
        assert_eq!(sure, [true, true, false, false, true, false, true, true, true, true]);
        assert_eq!(open_paragraphs(&nature, &sure), 3);
        assert_eq!(merge(&labels, &sure, &["other"; 10]), [SKIP, "abstract", "other", "other", SKIP, "other", SKIP, "discussion", SKIP, "methods"]);
        assert_eq!(open_paragraphs(&named, &assign_sure(&named, "").1), 1);
        let made = sentences(&nature, &assign(&nature, summary));
        assert_eq!(made.iter().filter(|s| s.section == "introduction").count(), 6);
        assert_eq!(made.iter().find(|s| s.section == "results").map(|s| (s.heading.as_str(), s.position)), Some(("Rice root atlas", 0)));
        assert!(made.iter().all(|s| s.section != "abstract"));
        assert_eq!(made.iter().filter(|s| s.section == "methods").map(|s| s.position).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn parts_by_a_model() {
        let blocks = [block("Title"), block(INTRO), block("Atlas"), block(RESULT), block("Fig. 2 | Legend. Two sentences."), block(RESULT)];
        let prompt = outline_prompt("A study", &blocks);
        assert!(prompt.contains("0 p1 H Title\n") && prompt.contains("1 p1 P Crops such as rice") && prompt.contains("字符）"));
        let labels = parse_outline("好的：\nintroduction: 1\n- Results: 2-9\n方法：\nnonsense: 0", &blocks, "").unwrap();
        assert_eq!(labels, [SKIP, "introduction", SKIP, "results", SKIP, "results"]);
        assert_eq!(parse_outline("导言：1；结果：3", &blocks, "").unwrap()[1], "introduction");
        assert_eq!(parse_outline("introduction: 1", &blocks, INTRO).unwrap()[1], "abstract");
        assert!(parse_outline("I cannot tell.", &blocks, "").is_err());
    }

    #[test]
    fn sentences_of_a_text() {
        let text = "Roots adapt to soil (Smith et al. 2019; Fig. 1a). A. thaliana grows fast, e.g. in 3.5 days! Is it so? \"Yes.\" It was shown by Zhu et al. (2021) that it is.";
        assert_eq!(split_sentences(text), ["Roots adapt to soil (Smith et al. 2019; Fig. 1a).", "A. thaliana grows fast, e.g. in 3.5 days!", "Is it so?", "\"Yes.\"", "It was shown by Zhu et al. (2021) that it is."]);
        assert!(usable("However, our understanding of how roots respond to soil remains limited."));
        for bad in ["Too short.", "1Department of Biology, Duke University, Durham, NC, USA. x@duke.edu is the address.", "however, a fragment that starts in the middle of things goes on.", "A sentence that never comes to an end and so is likely a heading or a row of a table", "12.5 33.1 45.6 78.9 11.2 0.05 0.01 ns ns 14.2 15.5 16.7 18.9."] {
            assert!(!usable(bad), "{bad}");
        }
    }

    #[test]
    fn apparatus_is_not_prose() {
        assert!(apparatus("Note: Cell-type numbers are approximate where the original paper did not report exact counts."));
        assert!(apparatus("Xylem-pore-pericycle 0.57 Late xylem-pore-pericycle (XPP) 0.49 Phloem-pore-pericycle 0.39 Lateral root primordium 0.55 Stele 0.73 Cortex 0.68 Endodermis 0.69 The correlation analysis identifies non-hair cells as the primary match."));
        assert!(apparatus("注：细胞类型数目为近似值。"));
        // Numbers of prose come with units and punctuation; a year or two is no table.
        assert!(!apparatus("The correlation was 0.57 (P < 0.01), higher than the 0.49 reported in 2019 for 12 samples."));
        assert!(!apparatus("Notably, the stele showed the highest correlation with cluster 3."));
        assert!(!apparatus("We profiled 47,000 cells from 10 roots at 3, 5, 7 and 9 days after germination."));
    }

    fn texts(pages: &[String]) -> Vec<String> {
        blocks(pages).into_iter().map(|b| b.text).collect()
    }

    #[test]
    fn words_broken_at_line_ends() {
        // PBS3 protects EDS1 (Molecular Plant): two compounds the paper writes on no line whole, one word broken in two.
        let pbs3 = page(
            "During evolution, both plants and animals independently developed the ability to\nrecognize conserved molecules in pathogens called pathogen-\nassociated molecular patterns (PAMPs) (Jones and Dangl, 2006).\nburst, and callose deposition to restrict pathogen growth\nassociated with the induction of localized programmed cell death.\n\nThis study\nunravels an EDS1-dependent mechanism underlying PBS3-\nmediated plant immunity. Further studies are required to eluci-\ndate how PTI is mediated by several pattern recognition receptors. Primers were EDS1-qRTFor/EDS1-\nqRTRev and the title says Proteasome-\nMediated Degradation.",
        );
        let text = texts(&[pbs3]).join("\n");
        for kept in ["called pathogen-associated molecular", "underlying PBS3-mediated plant", "required to elucidate how", "EDS1-qRTFor/EDS1-qRTRev", "Proteasome-Mediated Degradation"] {
            assert!(text.contains(kept), "{kept}: {text}");
        }
        // A rice root atlas (Nature), the citrus greening review, a single-cell review: words broken in two, `through-` / `out` a closed compound.
        let others = page(
            "Table 1 and Supplementary Data 1). To mitigate the effect of protoplast-\ning on gene expression, we identified protoplasting-induced genes by sorting.\nIt is challenging for biologists to fully understand the pathogen-\nesis of citrus HLB, and pathogenesis is slow. The atlas reveals gene-expression complexity through-\nout root developmental stages. Repeated pre- and\npost-treatment.",
        );
        let text = texts(&[others]).join("\n");
        for kept in ["effect of protoplasting on gene", "understand the pathogenesis of citrus", "complexity throughout root", "pre- and post-treatment"] {
            assert!(text.contains(kept), "{kept}: {text}");
        }
        // German (Journal of Health Monitoring) breaks its compounds anywhere: both halves being words says nothing there.
        let german = page(
            "Extreme Wetterereignisse stellen in der öffentlichen Wahr-\nnehmung eine der greifbarsten Auswirkungen des Klima-\nwandels dar. Einzelne Ereignisse, die Katastrophen auslö-\nsen, werden häufig mit dem Klimawandel erklärt. Das ist\naus wissenschaftlicher Perspektive schwierig, weil sich mit\ndem Klimawandel zwar die Wahrscheinlichkeit für das Auf-\ntreten von Extremen verändert, dies aber noch keine hin-\n\nreichende Erklärung für das jeweilige Einzelereignis dar-\nstellt (Infobox). Jedoch deutet vieles darauf hin, dass die Ereignisse zu den Vektor-assoziierten Krankheiten und der Hitze hin führen, die ein reichende Zahl ist.",
        );
        let text = texts(&[german]).join("\n");
        assert!(text.contains("noch keine hinreichende Erklärung für das jeweilige Einzelereignis darstellt") && text.contains("öffentlichen Wahrnehmung"), "{text}");
    }

    #[test]
    fn a_short_piece_continued_over_a_page() {
        // Weedy rice evolution (Molecular Plant, Editor's Highlights): a paragraph runs past a legend and the journal's notices to page 2.
        let pages = [
            page("Weedy Rice Evolution: De-domestication\nor Semi-domestication?\n\nWeeds, especially close relatives of crops, pose severe threats\nto agricultural production worldwide. Thus, understanding the origin and evolution of weedy rice is so urgently needed for developing efficient weed management strategies in agriculture. Interestingly, in this issue, Sun et al. (2019) reported a novel finding.\n\nIn short, Sun et al. (2019) have provided multiple lines of\nmutually supportive evidence showing that WRAH originated from early Japonica ancestor through semi-domestication and underwent parallel evolution. While weedy rice\nis troublesome as an ‘‘evil’’ in most rice production areas\n\nFigure 1. Co-occurrence of Weedy and Cultivated Rice in the\nPaddy Field in Northeast China.\n\nPublished by the Molecular Plant Shanghai Editorial Office in association with\nCell Press, an imprint of Elsevier Inc., on behalf of CSPB and IPPE, SIBS, CAS.\n\nMolecular Plant 12, 613–614, May 2019 ª The Author 2019. 613"),
            page("worldwide, the genomics-based identification of genes/QTLs\nassociated with its stress tolerance and fast growth could facilitate the\nelimination of its evil properties for crop improvement.\n\nXiao Yang*\n\n*Correspondence: Xiao Yang (yangxiao@sibs.ac.cn)\n\nREFERENCES\n\nLi, L.F., Li, Y.L., Jia, Y.L., Caicedo, A.L., and Olsen, K.M. (2017)."),
        ];
        let blocks = blocks(&pages);
        let (labels, sure) = assign_sure(&blocks, "");
        let made = sentences(&blocks, &labels);
        assert!(made.iter().any(|s| s.text.starts_with("While weedy rice is troublesome as an ‘‘evil’’ in most rice production areas worldwide, the genomics-based")), "{made:#?}");
        assert!(made.iter().all(|s| s.section == "introduction" && !s.text.starts_with("Published")));
        // Every paragraph a guess, and few of them: worth a model's look, though fewer than `OPEN_ENOUGH`.
        let (open, paragraphs) = (open_paragraphs(&blocks, &sure), prose_paragraphs(&blocks));
        assert_eq!((open, paragraphs), (2, 2));
        assert!(worth_asking(open, paragraphs) && !worth_asking(open, 40) && worth_asking(5, 100) && !worth_asking(3, 0));
    }

    #[test]
    fn an_abstract_in_a_side_column() {
        // Journal of Health Monitoring: the abstract stands in a column beside the introduction and comes after it in reading order.
        let einleitung = "Extreme Wetterereignisse stellen in der öffentlichen Wahrnehmung eine der greifbarsten Auswirkungen des Klimawandels dar. Einzelne Ereignisse, die Katastrophen auslösen, werden häufig mit dem Klimawandel erklärt. Das ist aus wissenschaftlicher Perspektive schwierig.";
        let zusammenfassung = "Hintergrund: Extremwetterereignisse stellen eine der greifbarsten Auswirkungen des anthropogenen Klimawandels dar. Methode: Es wurde eine Literaturrecherche durchgeführt. Ergebnisse: Die gesundheitlichen Folgen sind vielfältig und reichen weit.";
        let weiter = "Im Folgenden werden beide Sichtweisen verknüpft. Zunächst wird die klimawandelbedingte Änderung der Frequenz potenziell gesundheitsgefährdender Extremwetterereignisse beschrieben. Es ist zu erwarten, dass Frequenz und Magnitude zunehmen.";
        let title = |text: &str| Block { page: 1, text: text.into(), heading: true };
        let blocks = [title("1. Einleitung"), block(einleitung), title("Abstract"), block(zusammenfassung), title("Korrekturhinweis: Eine aktuellere Version des Artikels"), block(weiter), title("2.1 Risikokaskaden – konzeptionelle Grundlagen"), block(weiter)];
        // After it the text goes on where it was: a review whose own headings are its body.
        assert_eq!(assign(&blocks, ""), [SKIP, "introduction", SKIP, "abstract", SKIP, "other", SKIP, "other"]);
        // A heading the paper names ends it too.
        let paper = [title("Introduction"), block(einleitung), title("Abstract"), block(zusammenfassung), block(weiter), title("Results"), block(weiter)];
        assert_eq!(assign(&paper, ""), [SKIP, "introduction", SKIP, "abstract", "abstract", SKIP, "results"]);
        // An abstract with no heading of the paper's own after it ends when it has run long enough.
        let part = zusammenfassung.repeat(3);
        let long = [title("Abstract"), block(&part), block(&part), block(&part), block(&part), block(weiter)];
        assert_eq!(assign(&long, "")[1..], ["abstract", "abstract", "abstract", "abstract", "introduction"]);
        assert_eq!(canonical("1. Einleitung"), Some("introduction"));
    }

    #[test]
    fn a_paper_in_chinese() {
        // 四川大学学报（医学版）: a Chinese abstract run into its name, an English one, keywords, and an introduction without a title.
        let pages = [
            page("CAMK2A通过激活ERK信号通路调控口腔鳞癌细胞\n\n紫杉醇耐药性*\n\n口腔疾病防治全国重点实验室 口腔疾病国家临床医学研究中心 四川大学华西口腔医院 （成都 610041）\n\n【摘要】 目的 研究钙离子信号通路调控口腔鳞癌（oral squamous cell carcinoma, OSCC）细胞紫杉醇耐药生存的分\n子机制。方法 通过紫杉醇低剂量持续诱导且剂量递增法诱导构建口腔鳞癌紫杉醇耐药细胞模型；使用Cell Counting\nKit-8（CCK-8）和成像流式评估耐药模型的稳定性。\n【关键词】 口腔鳞癌 钙离子信号通路 紫杉醇耐药 CAMK2a 靶点\n\nWang Junfeng , Chen Hongli, Chen Jiao, Feng Yun, Yuan Yao, Zhang Ping △. State Key Laboratory of Oral\n\nDiseases and National Clinical Research Center for Oral Diseases, West China Hospital of Stomatology, Sichuan University,\nChengdu 610041, China. This study was supported by the Sichuan Provincial Science and Technology Program (No. 2023NSFSC0555), Basic and Applied Basic Research Project.\n\n[Abstract] Objective\n\nTo investigate the molecular mechanisms by which calcium signaling pathways regulate the\npaclitaxel resistance and the survival of oral squamous cell carcinoma (OSCC) cells. Methods A paclitaxel-resistant\nOSCC cell model was established through continuous low-dose paclitaxel induction with dose escalation.\n\n[Key words]\n\nOral squamous cell carcinoma Calcium signaling pathway Paclitaxel resistance"),
            page("口腔鳞状细胞癌（oral squamous cell carcinoma,\nOSCC）是头颈部常见的恶性肿瘤之一，起源于口腔黏膜\n上皮。2022年全球新发病例约389 485例[1-2]。目前治疗手\n段多样，但手术联合化疗仍是主要方案。然而，随着化疗\n疗程延长，多数患者逐渐产生耐药，导致预后不良。因\n此，深入探讨OSCC耐药机制，对于应对临床耐药现象、\n改善患者预后具有重要意义。\n\n1 材料与方法\n\n1.1 材料\n\nCAL27耐药细胞株由四川大学华西口腔医院构建；南美胎牛血清（FBS-PAN）购自成都宝科生物科技有限公司。CAL27细胞采用含10%胎牛血清及1%青霉素-链霉素双抗溶液的DMEM高糖培养基进行培养。\n\n2 结果\n\n实验结果显示，CAL27细胞经过长期、浓度梯度递增的紫杉醇诱导（图1A），构建的耐药细胞株可以在含50 nmol/L紫杉醇的培养基中稳定生长。CCK-8结果（图1B）显示，CAL27-PTX细胞在紫杉醇处理后始终保持高于CAL27的细胞活性。\n\n3 讨 论\n\n化疗耐药是导致肿瘤治疗失败的主要原因之一。多数肿瘤患者在化疗初期可获得一定程度的缓解，但随后的复发提示肿瘤已产生耐药性[17]。\n\n参考文献\n\n[1] SUNG H, FERLAY J, SIEGEL R L, et al. Global cancer statistics 2020. CA Cancer J Clin, 2021, 71(3): 209-249."),
        ];
        let made = by_rule(&pages, "");
        let part = |key: &str| made.sentences.iter().filter(|s| s.section == key).map(|s| s.text.as_str()).collect::<Vec<_>>();
        // Lines join without a space between Chinese letters, sentences end at `。`.
        // (`2022年全球新发病例约389 485例[1-2]。` is too short a sentence to show: about five words.)
        assert_eq!(part("introduction"), ["口腔鳞状细胞癌（oral squamous cell carcinoma, OSCC）是头颈部常见的恶性肿瘤之一，起源于口腔黏膜上皮。", "目前治疗手段多样，但手术联合化疗仍是主要方案。", "然而，随着化疗疗程延长，多数患者逐渐产生耐药，导致预后不良。", "因此，深入探讨OSCC耐药机制，对于应对临床耐药现象、改善患者预后具有重要意义。"]);
        assert_eq!(part("methods").len(), 2);
        assert_eq!(part("results")[1], "CCK-8结果（图1B）显示，CAL27-PTX细胞在紫杉醇处理后始终保持高于CAL27的细胞活性。");
        assert_eq!(part("discussion").len(), 2);
        assert!(made.sentences.iter().all(|s| s.language == "zh" && !s.text.contains("Sichuan") && !s.text.contains("To investigate")), "{:#?}", made.sentences);
        assert_eq!(made.language, "zh");
        for (heading, key) in [("1 材料与方法", "methods"), ("3 讨 论", "discussion"), ("一、引言", "introduction"), ("0 引言", "introduction"), ("结果与分析", "results"), ("参考文献", SKIP), ("【关键词】", SKIP), ("緒 言", "introduction"), ("考察", "discussion"), ("서 론", "introduction"), ("재료 및 방법", "methods"), ("Material und Methoden", "methods"), ("Resultados y discusión", "results"), ("Referências", SKIP)] {
            assert_eq!(canonical(heading), Some(key), "{heading}");
        }
        assert_eq!(canonical("紫杉醇耐药细胞株CAL27-PTX的构建"), None);
    }

    #[test]
    fn sentences_of_other_languages() {
        // 薬学雑誌 (Japanese, `，．`), 주간 건강과 질병 (Korean, `. ` before a letter without capitals), a Mexican seed study (Spanish abbreviations).
        let japanese = "一般用医薬品とは，医師の処方箋がなくても入手できる医薬品のことであり，いわゆる「市販薬」のことである．近年，この一般用医薬品をとりまく環境は大きく変化している．2014 年には，医薬品，医療機器等の品質，有効性及び安全性の確保等に関する法律（旧薬事法）改訂により販売制度が大きく変更された．";
        assert_eq!(split_sentences(japanese).len(), 3);
        assert!(split_sentences(japanese).iter().all(|s| usable(s)));
        let korean = "2021년에 CRE 전수조사를 통해 전국 시ㆍ도 보건환경연구원에 검사 의뢰된 22,152주 중 CRE로 최종 확인된 15,639주(70.6%)를 대상으로 분석하였다. 그 외 기타 장내세균속균종으로는 Citrobacter freundii와 K. aerogenes 등의 50종이 1,768주(11.5%) 확인되었다(표 1).";
        assert_eq!(split_sentences(korean).len(), 2);
        assert!(split_sentences(korean).iter().all(|s| usable(s)));
        let spanish = "En México, el chile silvestre Capsicum annuum var. Glabriusculum se denomina amashito (Aguilar et al., 2009). La semilla se secó aprox. tres días, p. ej. en estufa. Sin embargo, la germinación disminuye.";
        assert_eq!(split_sentences(spanish), ["En México, el chile silvestre Capsicum annuum var. Glabriusculum se denomina amashito (Aguilar et al., 2009).", "La semilla se secó aprox. tres días, p. ej. en estufa.", "Sin embargo, la germinación disminuye."]);
        let german = "Die Ereignisse werden z. B. in Abb. 2 gezeigt, vgl. Tabelle 1. Daraus folgt eine Zunahme.";
        assert_eq!(split_sentences(german).len(), 2);
        // Too short in any script, and Chinese without its end mark is not a sentence.
        assert!(!usable("化疗耐药是主要原因。") && !usable("钙离子信号通路是一类通过钙离子作为第二信使调控细胞增殖、凋亡等生物学过程的重要信号通路"));
        assert!(usable("钙离子信号通路是一类通过钙离子作为第二信使调控细胞增殖、凋亡等生物学过程的重要信号通路。"));
    }

    #[test]
    fn the_language_of_a_text() {
        let pages = |text: &str| vec![text.repeat(30)];
        assert_eq!(language(&pages(INTRO)), "en");
        assert_eq!(language(&pages("化疗耐药是导致肿瘤治疗失败的主要原因之一。Global cancer statistics 2020. ")), "zh");
        assert_eq!(language(&pages("一般用医薬品とは，医師の処方箋がなくても入手できる医薬品のことである．")), "ja");
        assert_eq!(language(&pages("항생제의 부적절한 처방과 오남용으로 인해 항생제 내성균의 출현이 증가하면서 ")), "ko");
        assert_eq!(language(&pages("Extreme Wetterereignisse stellen in der öffentlichen Wahrnehmung eine der greifbarsten Auswirkungen des Klimawandels dar und sind nicht selten. ")), "de");
        assert_eq!(language(&pages("El chile amashito es un alimento popular del sur de México y se consume en la región por los habitantes. ")), "es");
        assert_eq!(language(&pages("A semente de pimentão não germina bem, e os produtores usam uma técnica para a germinação das sementes com água. ")), "pt");
        assert_eq!(language(&["Fig. 1".to_string()]), "");
        assert!(LANGUAGES.iter().all(|(key, name)| key.len() == 2 && !name.is_empty()));
    }

    #[test]
    fn search_words_of_chinese_and_other_languages() {
        // Pairs of letters for Chinese, Japanese and Korean, a stem for English words, the commonest words of other languages left out.
        assert_eq!(words("钙离子信号 CAMK2a regulates"), ["钙离", "离子", "子信", "信号", "camk2a", "regulat"]);
        assert_eq!(words("항생제 내성"), ["항생", "생제", "내성"]);
        assert_eq!(words("die Ereignisse und der Klimawandel"), ["die", "ereigniss", "klimawandel"]);
        let texts = ["钙离子信号通路调控细胞增殖。", "手术联合化疗仍是主要方案。", "Calcium signalling regulates proliferation."];
        let found = rank(&texts, &["a", "b", "c"], "钙离子信号", 10);
        assert_eq!(found.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn what_is_being_written() {
        let text = "First paragraph, done.\nRoots adapt to soil. However, how they\n\nThird.";
        assert_eq!(writing_at(text, text.find("\n\n").unwrap()), "Roots adapt to soil. However, how they");
        assert_eq!(writing_at(text, 23), "Roots adapt to soil. However, how they");
        assert_eq!(writing_at(text, 5), "First");
        assert_eq!(writing_at("根系如何", 100), "根系如何");
        assert_eq!(writing_at("", 0), "");
        assert!(in_chinese("root 适应 soil") && !in_chinese("root–soil"));
    }

    #[test]
    fn candidates_by_relevance() {
        let texts = [
            "However, how roots respond to soil compaction at the cellular level remains poorly understood.",
            "Single-cell RNA sequencing has transformed the study of plant development.",
            "Roots respond to compacted soils by regulating cell wall remodelling.",
            "The regulation of root growth by auxin is well known.",
            "We thank the members of the laboratory for discussions.",
        ];
        let owners = ["a", "a", "b", "b", "c"];
        let found = rank(&texts, &owners, "root response to soil compaction", 10);
        // The two that speak of roots in compacted soil, then the one that only speaks of roots.
        let order: Vec<usize> = found.iter().map(|(index, _)| *index).collect();
        assert!(order == [0, 2, 3] || order == [2, 0, 3], "{order:?}");
        let mut regulated: Vec<usize> = rank(&texts, &owners, "regulated", 10).iter().map(|(i, _)| *i).collect();
        regulated.sort();
        assert_eq!(regulated, [2, 3]);
        assert!(rank(&texts, &owners, "的 the", 10).is_empty());
        // One paper takes three places at most.
        let many = ["roots grow in soil."; 5];
        assert_eq!(rank(&many, &["a"; 5], "roots soil", 10).len(), 3);
        assert_eq!(words("Regulates regulated regulating regulation cells analyses"), ["regulat", "regulat", "regulat", "regulat", "cell", "analys"]);
    }
}
