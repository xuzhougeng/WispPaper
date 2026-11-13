//! 文献库提问: what the model is given, the request for either protocol, and
//! reading the streamed answer. No HTTP here; the app sends the request.
//!
//! The library is put in front of the model first: every paper's record
//! (most relevant first) and the full-text pages that match the question,
//! each under a label like `[P3]` or `[P3 p.5]` the answer cites. What did
//! not fit is counted and shown, never dropped silently. Beyond that the
//! model has the library's tools (`tools`) and may search and read for
//! itself; a turn that ends in tool calls is answered with their results
//! and the conversation goes on.

use crate::fulltext::{bare, score, terms};
use crate::model::{FieldDef, PUBLICATION_STATUSES, Paper, PaperFile, READINGS, SCREENINGS, label};
use crate::tools::ToolDef;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// The Claude Messages API.
    #[default]
    Anthropic,
    /// Any service speaking OpenAI's chat completions.
    OpenAI,
    /// OpenAI's Responses API (`/responses`).
    Responses,
}

/// One model configuration from the settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    /// What the task bindings in the settings point at. Given once, when the
    /// profile is added or first read from an older settings file.
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    /// Empty means the ANTHROPIC_API_KEY / OPENAI_API_KEY environment variable.
    pub api_key: String,
    pub model: String,
    /// How hard this model thinks, a key of `EFFORTS`: empty leaves it to the
    /// service, `off` asks for no thinking, the rest are levels.
    pub effort: String,
    /// A proxy for this service alone: empty uses the global one, `direct` none at all.
    pub proxy: String,
}

/// The thinking settings a profile may have, and what they are called.
pub const EFFORTS: [(&str, &str); 7] = [("", "默认"), ("off", "关闭"), ("low", "低"), ("medium", "中"), ("high", "高"), ("xhigh", "极高"), ("max", "最高")];

pub const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

impl Default for Profile {
    fn default() -> Self {
        Profile { id: String::new(), name: "Claude".into(), protocol: Protocol::Anthropic, base_url: ANTHROPIC_BASE.into(), api_key: String::new(), model: DEFAULT_MODEL.into(), effort: String::new(), proxy: String::new() }
    }
}

impl Profile {
    /// What the profile is called where it is listed or credited: its name, else its model.
    pub fn title(&self) -> String {
        let name = if self.name.trim().is_empty() { self.model.trim() } else { self.name.trim() };
        if name.is_empty() { "未命名模型".into() } else { name.to_string() }
    }

    /// What is still missing before a request can be made, if anything.
    pub fn missing(&self) -> Option<&'static str> {
        if self.key().is_empty() { Some("还缺 API Key") } else if self.model.trim().is_empty() { Some("还缺模型名称") } else { None }
    }

    /// The key to send: the stored one, else the protocol's environment variable.
    pub fn key(&self) -> String {
        if !self.api_key.trim().is_empty() { return self.api_key.trim().to_string() }
        std::env::var(match self.protocol {
            Protocol::Anthropic => "ANTHROPIC_API_KEY",
            Protocol::OpenAI | Protocol::Responses => "OPENAI_API_KEY",
        })
        .unwrap_or_default()
    }
}

/// What a label in the answer points at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// `P3`
    pub label: String,
    pub paper_id: String,
    pub title: String,
}

/// What a conversation is about.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// The papers handed in (the whole library, or the current filter result).
    Library,
    /// One paper, read in full.
    Paper(String),
}

/// Everything the model is given for one conversation. Built once, with the
/// first question, and reused for the follow-ups so the prompt prefix stays
/// byte-identical and cacheable.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub instructions: String,
    pub documents: String,
    pub sources: Vec<Source>,
    /// Papers given with their full record.
    pub full: usize,
    /// Papers given by title only, for lack of room.
    pub titles_only: usize,
    /// Papers left out entirely.
    pub omitted: usize,
    /// Full-text pages included.
    pub pages: usize,
    /// Full-text pages of the paper in scope that did not fit.
    pub pages_omitted: usize,
    /// What each paper in scope was given as, in the library's order.
    pub given: Vec<Given>,
}

/// How much of a paper in scope the model was handed.
#[derive(Clone, Debug, PartialEq)]
pub struct Given {
    pub paper_id: String,
    /// `full` (its record, with abstract and notes), `title` (for lack of room), or `omitted`.
    pub record: &'static str,
    /// Pages of its full text handed over with the question.
    pub pages: usize,
}

impl Context {
    /// Give the papers outside the documents a label too: the tools reach the
    /// whole library, and what they find must be citable. The new labels
    /// continue after the highest one in use.
    pub fn add_sources(&mut self, papers: &[Paper]) {
        let mut next = self.sources.iter().filter_map(|s| s.label.get(1..).and_then(|n| n.parse::<usize>().ok())).max().unwrap_or(0);
        for paper in papers {
            if self.sources.iter().any(|s| s.paper_id == paper.id) { continue }
            next += 1;
            self.sources.push(Source { label: format!("P{next}"), paper_id: paper.id.clone(), title: paper.title.clone() });
        }
    }
}

/// Room for paper records, for full-text pages, and for the bare title list, in bytes.
const RECORD_BUDGET: usize = 400_000;
const PAGE_BUDGET: usize = 240_000;
const TITLE_BUDGET: usize = 120_000;
/// Room for one paper read in full.
const PAPER_BUDGET: usize = 1_200_000;
const MAX_LIBRARY_PAGES: usize = 40;

fn record(label_text: &str, p: &Paper, fields: &[FieldDef]) -> String {
    let mut out = format!("[{label_text}] {}\n", p.title);
    let mut line = |name: &str, value: &str| {
        if !value.trim().is_empty() { out.push_str(&format!("{name}：{}\n", value.trim())) }
    };
    line("作者", &p.authors);
    line("期刊", &p.journal);
    line("年份", &p.year.map(|y| y.to_string()).unwrap_or_default());
    line("DOI", &p.doi);
    line("PMID", &p.pmid);
    if p.publication_status != "unknown" { line("发表状态", label(&PUBLICATION_STATUSES, &p.publication_status)) }
    line("标签", &p.tags.join("、"));
    for field in fields {
        if let Some(values) = p.fields.get(&field.id) { line(&field.name, &values.join("、")) }
    }
    line("阅读状态", label(&READINGS, &p.reading));
    line("收录判断", label(&SCREENINGS, &p.screening));
    line("判断理由", &p.screening_reason);
    line("摘要", &p.abstract_text);
    line("我的笔记", &p.notes);
    out
}

/// Assemble what the model reads. `papers` is the scope's list in display
/// order; `fulltext` holds the extracted pages by paper id. `known` is what
/// the labels of an earlier part of the conversation name (one reopened
/// from the history): those papers keep their labels, so what was cited
/// before still points at the same paper, and the others are numbered on.
pub fn build_context(library_name: &str, topic: &str, papers: &[&Paper], fields: &[FieldDef], fulltext: &BTreeMap<String, (PaperFile, Vec<String>)>, scope: &Scope, question: &str, known: &[Source]) -> Context {
    let mut ctx = Context::default();
    let words = terms(question);
    let mut next = known.iter().filter_map(|s| s.label.get(1..).and_then(|n| n.parse::<usize>().ok())).max().unwrap_or(0);
    let mut labels: BTreeMap<&str, String> = BTreeMap::new();
    for p in papers {
        let label = match known.iter().find(|s| s.paper_id == p.id) {
            Some(source) => source.label.clone(),
            None => {
                next += 1;
                format!("P{next}")
            }
        };
        ctx.sources.push(Source { label: label.clone(), paper_id: p.id.clone(), title: p.title.clone() });
        labels.insert(p.id.as_str(), label);
    }
    let mut documents = String::new();

    match scope {
        Scope::Paper(id) => {
            if let Some(p) = papers.iter().find(|p| &p.id == id) {
                let tag = &labels[p.id.as_str()];
                documents.push_str("## 文献记录\n\n");
                documents.push_str(&record(tag, p, fields));
                ctx.full = 1;
                match fulltext.get(id) {
                    Some((_, pages)) => {
                        documents.push_str("\n## 全文（逐页）\n");
                        let mut used = 0;
                        for (i, page) in pages.iter().enumerate() {
                            if used + page.len() > PAPER_BUDGET {
                                ctx.pages_omitted = pages.len() - i;
                                break;
                            }
                            used += page.len();
                            documents.push_str(&format!("\n[{tag} p.{}]\n{}\n", i + 1, page.trim()));
                            ctx.pages += 1;
                        }
                    }
                    None => documents.push_str("\n（这篇文献没有可读取的全文，只有上面的记录。）\n"),
                }
            }
            ctx.sources.retain(|s| &s.paper_id == id);
        }
        Scope::Library => {
            // Most relevant first; without any match the list keeps its order.
            let mut order: Vec<(f32, usize)> = papers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let hay = format!("{} {} {} {} {}", p.title, p.abstract_text, p.tags.join(" "), p.notes, p.fields.values().flatten().cloned().collect::<Vec<_>>().join(" ")).to_lowercase();
                    (score(&hay, &words), i)
                })
                .collect();
            order.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
            let mut full: Vec<usize> = vec![];
            let mut titles: Vec<usize> = vec![];
            let (mut used, mut used_titles) = (0, 0);
            for (_, i) in order {
                let text = record(&labels[papers[i].id.as_str()], papers[i], fields);
                if used + text.len() <= RECORD_BUDGET {
                    used += text.len();
                    full.push(i);
                } else if used_titles + papers[i].title.len() + 24 <= TITLE_BUDGET {
                    used_titles += papers[i].title.len() + 24;
                    titles.push(i);
                } else {
                    ctx.omitted += 1;
                }
            }
            ctx.given = papers.iter().enumerate().map(|(i, p)| Given { paper_id: p.id.clone(), record: if full.contains(&i) { "full" } else if titles.contains(&i) { "title" } else { "omitted" }, pages: 0 }).collect();
            ctx.full = full.len();
            ctx.titles_only = titles.len();
            // Listed in the library's own order, so labels read in sequence.
            full.sort();
            titles.sort();
            documents.push_str("## 文献记录\n\n");
            for i in full {
                documents.push_str(&record(&labels[papers[i].id.as_str()], papers[i], fields));
                documents.push('\n');
            }
            if !titles.is_empty() {
                documents.push_str("## 仅列出题目的文献（篇幅所限，未附摘要）\n\n");
                for i in titles {
                    let p = papers[i];
                    documents.push_str(&format!("[{}] {}{}\n", labels[p.id.as_str()], p.title, p.year.map(|y| format!("（{y}）")).unwrap_or_default()));
                }
                documents.push('\n');
            }
            // Full-text pages that match the question.
            let mut hits: Vec<(f32, &str, usize, &str)> = vec![];
            if !words.is_empty() {
                for p in papers {
                    let Some((_, pages)) = fulltext.get(&p.id) else { continue };
                    for (n, page) in pages.iter().enumerate() {
                        let s = score(&page.to_lowercase(), &words);
                        if s > 0. { hits.push((s, p.id.as_str(), n, page.as_str())) }
                    }
                }
            }
            hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut used = 0;
            let mut chosen: Vec<(&str, usize, &str)> = vec![];
            for (_, id, n, page) in hits.into_iter().take(MAX_LIBRARY_PAGES) {
                if used + page.len() > PAGE_BUDGET { continue }
                used += page.len();
                chosen.push((id, n, page));
            }
            chosen.sort_by(|a, b| labels[a.0].len().cmp(&labels[b.0].len()).then(labels[a.0].cmp(&labels[b.0])).then(a.1.cmp(&b.1)));
            if !chosen.is_empty() {
                documents.push_str("## 与问题相关的全文页面\n");
                for (id, n, page) in &chosen {
                    documents.push_str(&format!("\n[{} p.{}]\n{}\n", labels[id], n + 1, page.trim()));
                }
            }
            ctx.pages = chosen.len();
            for (id, ..) in &chosen {
                if let Some(given) = ctx.given.iter_mut().find(|given| given.paper_id == *id) { given.pages += 1 }
            }
        }
    }

    let about = if topic.trim().is_empty() { String::new() } else { format!("这个文献库的主题：{}\n", topic.trim()) };
    let scope_line = match scope {
        Scope::Paper(_) => "用户正在阅读其中一篇文献，下面是它的记录和全文。".to_string(),
        Scope::Library => {
            let mut line = format!("下面是文献库中 {} 篇文献的记录", ctx.full);
            if ctx.titles_only > 0 { line.push_str(&format!("，另有 {} 篇只列出题目", ctx.titles_only)) }
            if ctx.omitted > 0 { line.push_str(&format!("，还有 {} 篇因篇幅未提供", ctx.omitted)) }
            if ctx.pages > 0 { line.push_str(&format!("，以及 {} 页与问题相关的全文", ctx.pages)) }
            line.push('。');
            line
        }
    };
    ctx.instructions = format!(
        "你是 Wisp Paper 里的文献助手，帮助用户理解和整理文献库「{library_name}」。\n{about}{scope_line}\n\n\
回答时遵循：\n\
- 依据提供的文献材料作答。材料里没有的内容，明确说明文献库中没有相关信息，不要凭印象补充文献细节；需要用到常识性背景时，说明它不出自文献库。\n\
- 引用文献时使用材料中的标签，例如 [P3]；引用全文某一页时写 [P3 p.5]。标签要紧跟在它支持的那句话后面。\n\
- 依据的是全文某一页里的一句话时，把这句原文放进标签：[P3 p.5 \"原文中的一句\"]。原文必须逐字出自那一页，一句即可，不翻译、不改写、不拼接；界面会打开 PDF 并高亮它。拿不准原文时只写页码。\n\
- 「我的笔记」是用户自己写的，可以引用，但要与论文本身的结论区分开。\n\
- 只列出题目的文献没有摘要，对它们的内容不要下结论。\n\
- 有的文献存了 PDF 却没读出文字，工具会给出读取时得到的原因。照原因转述即可，不要自己推测它是扫描件、加了密或者损坏了；全文检索看不到这些文献的正文，没有命中不等于文中没有。\n\
- 上面的材料不够时使用工具：search_fulltext 在已保存的全文里找短语或关键词，read_fulltext 按页读原文核对上下文，list_papers 和 get_paper 查文献记录。工具查的是整个文献库，不限于上面列出的文献。\n\
- search_online 可以在 PubMed、Europe PMC、Crossref、arXiv 里试检索，不写入文献库。保存或删除检索式、运行导入、撤销导入会改动文献库：只在用户明确要求时使用，并说明做了什么。收录判断由用户本人做出：你可以用 suggest_screening 提出建议，它会出现在筛选页等用户确认。自定义词条也一样由用户写入：你可以用 suggest_field 为一篇文献的一个词条提出取值和原文依据，它会出现在提取页等用户确认。
\n- 工具结果里每篇文献带有 label，就是引用它时用的标签；工具的 paperId 和 id 参数可以直接填标签（如 P3）。没有保存全文的文献只有记录和摘要，回答依据的只是摘要时要说明。\n\
- 用用户提问的语言回答，直接回答问题，不必复述材料。"
    );
    ctx.documents = documents;
    // A paper cited earlier that is not among these keeps its label too.
    for source in known {
        if !ctx.sources.iter().any(|s| s.paper_id == source.paper_id) { ctx.sources.push(source.clone()) }
    }
    ctx
}

/// Something an answer cites: a paper by its label, perhaps a page of its
/// full text, perhaps a sentence quoted from that page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Citation {
    pub label: String,
    pub page: Option<usize>,
    /// The quoted sentence; empty when the answer gave none.
    pub quote: String,
}

/// What an answer cites, in order of first appearance.
pub fn citations(answer: &str) -> Vec<Citation> {
    let mut found: Vec<Citation> = vec![];
    for (_, cited) in citation_spans(answer) {
        // The same thing cited again is listed once.
        for citation in cited {
            if !found.contains(&citation) { found.push(citation) }
        }
    }
    found
}

/// Where an answer cites: each pair of brackets that names a paper, as
/// the bytes it takes (brackets included) and what it cites.
pub fn citation_spans(answer: &str) -> Vec<(std::ops::Range<usize>, Vec<Citation>)> {
    let mut spans = vec![];
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let start = answer.len() - rest.len() + open;
        rest = &rest[open + 1..];
        // `[P3 p.5 "a sentence"]`: the quotation may hold brackets of its own, so the
        // label ends at the `]` right after the closing quotation mark.
        let quoted = rest.find(['"', '“', '「']).filter(|at| !rest[..*at].contains(']')).and_then(|at| {
            let mark = rest[at..].chars().next()?;
            let closing = match mark {
                '“' => '”',
                '「' => '」',
                _ => '"',
            };
            let start = at + mark.len_utf8();
            let end = start + rest[start..].find(closing)?;
            let tail = &rest[end + closing.len_utf8()..];
            let close = tail.find(']').filter(|close| tail[..*close].trim().is_empty())?;
            Some((&rest[..at], rest[start..end].trim(), end + closing.len_utf8() + close))
        });
        let (head, quote, close) = match quoted {
            Some(parts) => parts,
            None => {
                let Some(close) = rest.find(']') else { break };
                (&rest[..close], "", close)
            }
        };
        // "[P3]", "[P3 p.5]", "[P3, p.5]", "[P1, P2]"
        let mut found: Vec<Citation> = vec![];
        for part in head.split([',', '，', ';', '、']) {
            let part = part.trim();
            let mut words = part.split_whitespace();
            let Some(first) = words.next() else { continue };
            let page = |word: &str| word.trim_start_matches("p.").trim_start_matches('p').parse::<usize>().ok();
            if first.starts_with('P') && first.len() > 1 && first[1..].bytes().all(|b| b.is_ascii_digit()) {
                found.push(Citation { label: first.to_string(), page: words.next().and_then(page), quote: String::new() });
            } else if first.starts_with("p.") && words.next().is_none() && let (Some(page), Some(last)) = (page(first), found.last_mut()) {
                // "[P3, p.5]": the page belongs to the label before the comma.
                if last.page.is_none() { last.page = Some(page) }
            }
        }
        // The quotation belongs to the last paper named in its brackets.
        if let Some(last) = found.last_mut() { last.quote = quote.to_string() }
        rest = &rest[close + 1..];
        if !found.is_empty() { spans.push((start..answer.len() - rest.len(), found)) }
    }
    spans
}

/// A passage of a paper as a question hands it to the model: written the
/// way answers cite, so that an answer can point back at it. The marks a
/// quotation is closed by are taken out of the passage.
pub fn quoted_passage(label: &str, page: usize, text: &str) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().map(|c| if matches!(c, '"' | '“' | '”') { '\'' } else { c }).collect();
    format!("[{label} p.{page} \"{text}\"]")
}

/// How a quotation in an answer stands against the text of the paper it names.
#[derive(Clone, Debug, PartialEq)]
pub enum Quoted {
    /// Word for word where the answer says: on the page named, or in the abstract.
    Found,
    /// Word for word, but on this page (1-based) and not the one named.
    Elsewhere(usize),
    /// Only its beginning is in the text: on this page, and this share of it (0 to 1).
    Begins(usize, f32),
    /// Nowhere in the text.
    Missing,
    /// The paper has no text to look in, and the quotation is not in its abstract.
    NoText,
}

/// The least of a quotation that counts as its beginning, in compared characters.
const BEGINNING: usize = 20;

/// Look a quotation up in the paper it is said to be from. Letters and
/// digits are compared and nothing else (`bare`), so line breaks and
/// hyphenation do not matter and a changed word or number does. This says
/// whether the words are there: whether they bear out the claim made with
/// them is for the reader.
pub fn check_quote(quote: &str, page: Option<usize>, paper: &Paper, pages: Option<&[String]>) -> Quoted {
    let wanted = bare(quote);
    if wanted.is_empty() { return Quoted::Missing }
    let in_record = bare(&paper.abstract_text).contains(&wanted) || bare(&paper.title).contains(&wanted);
    let Some(pages) = pages else { return if in_record { Quoted::Found } else { Quoted::NoText } };
    let pages: Vec<String> = pages.iter().map(|page| bare(page)).collect();
    let named = page.and_then(|page| page.checked_sub(1)).filter(|at| *at < pages.len());
    if named.is_some_and(|at| pages[at].contains(&wanted)) || (page.is_none() && in_record) { return Quoted::Found }
    if let Some(at) = pages.iter().position(|page| page.contains(&wanted)) { return if page.is_some() { Quoted::Elsewhere(at + 1) } else { Quoted::Found } }
    if in_record { return Quoted::Found }
    // How far each page goes along with the quotation from its first character on; the page named wins a tie.
    let letters: Vec<char> = wanted.chars().collect();
    let reach = |page: &String| {
        let (mut low, mut high) = (0, letters.len());
        while low < high {
            let middle = (low + high).div_ceil(2);
            if page.contains(&letters[..middle].iter().collect::<String>()) { low = middle } else { high = middle - 1 }
        }
        low
    };
    let best = pages.iter().enumerate().map(|(at, page)| (reach(page), named == Some(at), at)).max_by_key(|(reach, named, at)| (*reach, *named, std::cmp::Reverse(*at)));
    match best {
        Some((reach, _, at)) if reach >= BEGINNING.min(letters.len()) && reach < letters.len() => Quoted::Begins(at + 1, reach as f32 / letters.len() as f32),
        _ => Quoted::Missing,
    }
}

/// An answer about a paper as a passage for that paper's notes. A label
/// means something only in its conversation, so the paper's own goes
/// (`[P1 p.5 "…"]` becomes `[p.5 "…"]`) and another paper's gives way to
/// its title; pages and quotations stay. The last line says what this is:
/// a model's reading, which nobody has checked against the paper.
pub fn note_from_answer(question: &str, answer: &str, sources: &[Source], paper_id: &str, date: &str) -> String {
    let mut body = String::with_capacity(answer.len());
    let mut rest = answer.trim();
    while let Some(open) = rest.find('[') {
        body.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        // What stands before a quotation, or the whole of the brackets: the labels are here.
        let end = rest.find([']', '"', '“', '「']).unwrap_or(rest.len());
        let separates = |c: char| c.is_whitespace() || matches!(c, ',' | '，' | ';' | '、');
        let (mut head, mut word, mut dropped) = (String::new(), String::new(), false);
        for c in rest[..end].chars().chain([' ']) {
            if !separates(c) {
                word.push(c);
                continue;
            }
            if !word.is_empty() {
                match sources.iter().find(|source| source.label == word) {
                    Some(source) if source.paper_id == paper_id => dropped = true,
                    Some(source) => {
                        head.push_str(&format!("《{}》", source.title));
                        dropped = false;
                    }
                    None => {
                        head.push_str(&word);
                        dropped = false;
                    }
                }
                word.clear();
            }
            // What separated a label that went from the next word goes with it.
            if !dropped { head.push(c) }
        }
        head.pop();
        rest = &rest[end..];
        let closes = rest.starts_with(']');
        let head = if closes { head.trim_matches(separates) } else { head.trim_start_matches(separates) };
        if head.is_empty() && closes {
            // `[P1]` alone: nothing is left to say, and the space before it goes too.
            rest = &rest[1..];
            let kept = body.trim_end_matches(' ').len();
            body.truncate(kept);
        } else {
            body.push('[');
            body.push_str(head);
        }
    }
    body.push_str(rest);
    let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let asked = if question.is_empty() { String::new() } else { format!("问：{question}\n") };
    format!("{asked}{}\n（{date} 提问所得 · 模型的回答，未核对原文）", body.trim())
}

// ---- requests

#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    /// The profile's own proxy setting, for whoever sends the request.
    pub proxy: String,
}

/// Models whose safety classifiers can decline a request, and for which the
/// API can then rerun it on another model inside the same call.
const FALLBACK_MODELS: [&str; 5] = ["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

/// Claude models that cannot stop thinking: a request that tries is refused,
/// so "off" becomes the lowest effort for them.
const ALWAYS_THINKING: [&str; 3] = ["claude-fable", "claude-mythos", "claude-opus-5-5"];

/// Put the profile's thinking setting into a request body. Nothing is sent
/// for the default, so a service that knows none of these parameters still
/// answers. The three protocols, and the services behind the OpenAI one,
/// each spell it their own way.
fn apply_effort(profile: &Profile, base: &str, body: &mut Value) {
    let (effort, model) = (profile.effort.trim(), profile.model.trim());
    if effort.is_empty() || !EFFORTS.iter().any(|(key, _)| *key == effort) { return }
    // Chat completions and Responses know three levels; the two above them are the highest of those.
    let level = if matches!(effort, "xhigh" | "max") { "high" } else { effort };
    match (profile.protocol, effort) {
        (Protocol::Anthropic, "off") => {
            if ALWAYS_THINKING.iter().any(|name| model.contains(name)) {
                body["output_config"] = json!({"effort": "low"});
            } else if model.contains("claude-sonnet-5-5") {
                // Its way of not thinking; it takes no other field beside it.
                body["thinking"] = json!({"type": "between_tools"});
            } else {
                body["thinking"] = json!({"type": "disabled"});
            }
        }
        (Protocol::Anthropic, _) => {
            body["thinking"] = json!({"type": "adaptive"});
            body["output_config"] = json!({"effort": effort});
        }
        (Protocol::OpenAI, "off") => {
            if base.contains("api.openai.com") {
                body["reasoning_effort"] = json!("none");
            } else if base.contains("dashscope") || base.contains("siliconflow") {
                body["enable_thinking"] = json!(false);
            } else {
                // DeepSeek, 火山方舟, 智谱 and Kimi turn thinking off this way.
                body["thinking"] = json!({"type": "disabled"});
            }
        }
        (Protocol::OpenAI, _) => body["reasoning_effort"] = json!(level),
        (Protocol::Responses, _) => body["reasoning"] = json!({"effort": if effort == "off" { "none" } else { level }}),
    }
}

/// A question as it goes on the wire; the same for both protocols.
pub fn user_message(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

/// A picture that goes with a question.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`: what every protocol takes.
    pub media_type: String,
    /// The file's bytes in base64.
    pub data: String,
}

/// A question with pictures, as each protocol carries them. Without
/// pictures it is `user_message`; without text only the pictures go.
pub fn user_message_with(protocol: Protocol, text: &str, images: &[ImageData]) -> Value {
    if images.is_empty() { return user_message(text) }
    let url = |image: &ImageData| format!("data:{};base64,{}", image.media_type, image.data);
    let mut content: Vec<Value> = match protocol {
        // Claude reads a picture best when it comes before the question about it.
        Protocol::Anthropic => images.iter().map(|image| json!({"type": "image", "source": {"type": "base64", "media_type": image.media_type, "data": image.data}})).collect(),
        Protocol::OpenAI => images.iter().map(|image| json!({"type": "image_url", "image_url": {"url": url(image)}})).collect(),
        Protocol::Responses => images.iter().map(|image| json!({"type": "input_image", "image_url": url(image)})).collect(),
    };
    if !text.trim().is_empty() {
        match protocol {
            Protocol::Anthropic => content.push(json!({"type": "text", "text": text})),
            Protocol::OpenAI => content.insert(0, json!({"type": "text", "text": text})),
            Protocol::Responses => content.insert(0, json!({"type": "input_text", "text": text})),
        }
    }
    json!({"role": "user", "content": content})
}

/// Bytes as standard base64, padded: how a picture travels in a request.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() { TABLE[(n >> shift & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// An answer already given, when a conversation is replayed to another protocol.
pub fn assistant_message(text: &str) -> Value {
    json!({"role": "assistant", "content": text})
}

/// The streaming request for one step of a conversation. `wire` is the
/// conversation so far in the profile's protocol: questions, the model's
/// turns exactly as it sent them, and tool results. It only ever grows, so
/// the prefix stays cacheable and the model's thinking blocks stay valid.
pub fn chat_request(profile: &Profile, ctx: &Context, wire: &[Value], tools: &[ToolDef]) -> Result<HttpRequest, String> {
    let key = profile.key();
    if key.is_empty() { return Err("还没有配置 API Key。请在「设置 › AI」里填写，或设置对应的环境变量。".into()) }
    if profile.model.trim().is_empty() { return Err("请在「设置 › AI」里填写模型名称。".into()) }
    let base = profile.base_url.trim().trim_end_matches('/');
    if !base.starts_with("https://") && !base.starts_with("http://") { return Err("模型服务地址应以 https:// 或 http:// 开头。".into()) }
    match profile.protocol {
        Protocol::Anthropic => {
            let mut headers = vec![("x-api-key".to_string(), key), ("anthropic-version".to_string(), "2026-10-21".to_string()), ("content-type".to_string(), "application/json".to_string())];
            let mut body = json!({
                "model": profile.model.trim(),
                // Streaming, so a long answer has room; thinking is the model's default unless the profile says otherwise.
                "max_tokens": 64000,
                "stream": true,
                // The documents come last in the system prompt with a cache breakpoint: follow-up
                // questions in the same conversation read them from the cache.
                "system": [
                    {"type": "text", "text": ctx.instructions},
                    {"type": "text", "text": ctx.documents, "cache_control": {"type": "ephemeral"}},
                ],
                "messages": wire,
            });
            if !tools.is_empty() {
                body["tools"] = tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema})).collect();
            }
            apply_effort(profile, base, &mut body);
            // Only on Anthropic's own API: the other platforms and proxies reject these parameters.
            if base == ANTHROPIC_BASE {
                // The steps of a tool loop resend the conversation; cache it up to the last block.
                body["cache_control"] = json!({"type": "ephemeral"});
                if FALLBACK_MODELS.contains(&profile.model.trim()) {
                    headers.push(("anthropic-beta".to_string(), "server-side-fallback-2026-07-01".to_string()));
                    body["fallbacks"] = json!("default");
                }
            }
            Ok(HttpRequest { url: format!("{base}/v1/messages"), headers, body, proxy: profile.proxy.trim().to_string() })
        }
        Protocol::OpenAI => {
            let mut all = vec![json!({"role": "system", "content": format!("{}\n\n{}", ctx.instructions, ctx.documents)})];
            all.extend(wire.iter().cloned());
            let mut body = json!({"model": profile.model.trim(), "stream": true, "messages": all});
            if !tools.is_empty() {
                body["tools"] = tools.iter().map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}})).collect();
            }
            apply_effort(profile, base, &mut body);
            Ok(HttpRequest { url: format!("{base}/chat/completions"), headers: vec![("authorization".to_string(), format!("Bearer {key}")), ("content-type".to_string(), "application/json".to_string())], body, proxy: profile.proxy.trim().to_string() })
        }
        Protocol::Responses => {
            let mut body = json!({
                "model": profile.model.trim(),
                "stream": true,
                "instructions": format!("{}\n\n{}", ctx.instructions, ctx.documents),
                "input": wire,
                // Nothing is kept on the service: the conversation is sent whole each time, and the
                // model's reasoning comes back encrypted so that it can be handed in again with it.
                "store": false,
                "include": ["reasoning.encrypted_content"],
            });
            if !tools.is_empty() {
                body["tools"] = tools.iter().map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.input_schema})).collect();
            }
            apply_effort(profile, base, &mut body);
            Ok(HttpRequest { url: format!("{base}/responses"), headers: vec![("authorization".to_string(), format!("Bearer {key}")), ("content-type".to_string(), "application/json".to_string())], body, proxy: profile.proxy.trim().to_string() })
        }
    }
}

// ---- the streamed turn

/// A tool the model asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments; `Err` when what the model sent is not a JSON object.
    pub args: Result<Value, String>,
}

/// One turn of the model, read to its end.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// The turn as it goes back on the wire; `Null` when the model sent
    /// nothing. One message for Claude and chat completions; for the
    /// Responses API an array of output items (see `wire`).
    pub message: Value,
    pub calls: Vec<ToolCall>,
    /// The protocol's stop reason; "end" when the stream gave none.
    pub stop: String,
}

impl Turn {
    /// What the turn adds to the conversation on the wire.
    pub fn wire(&self) -> Vec<Value> {
        match &self.message {
            Value::Null => vec![],
            Value::Array(items) => items.clone(),
            message => vec![message.clone()],
        }
    }

    /// Whether the calls may run: not when the turn was cut off or declined,
    /// since a call's arguments may then be half written.
    pub fn runnable(&self) -> bool {
        !matches!(self.stop.as_str(), "max_tokens" | "length" | "refusal" | "content_filter" | "model_context_window_exceeded")
    }
}

/// What the app hears from a request in flight.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    /// More of the answer's text.
    Text(String),
    /// The service was busy or unreachable and is being asked again: which try this is, and why.
    Retrying(usize, String),
    Done(Turn),
    Error(String),
}

fn push_text(value: &mut Value, more: &str) {
    match value {
        Value::String(text) => text.push_str(more),
        other => *other = json!(more),
    }
}

/// Reads the server-sent events of one turn. Both protocols put a complete
/// JSON object on every `data:` line.
pub struct TurnReader {
    protocol: Protocol,
    /// Anthropic: the content blocks as they arrive, by index, with the
    /// pieces of each tool call's JSON beside them.
    blocks: Vec<Value>,
    inputs: Vec<String>,
    /// OpenAI: the text, and (id, name, arguments) of each tool call.
    text: String,
    calls: Vec<(String, String, String)>,
    stop: String,
}

impl TurnReader {
    pub fn new(protocol: Protocol) -> TurnReader {
        TurnReader { protocol, blocks: vec![], inputs: vec![], text: String::new(), calls: vec![], stop: String::new() }
    }

    /// One line of the stream: the answer text it adds, if any. `Err` is an
    /// error the service reported inside the stream.
    pub fn line(&mut self, line: &str) -> Result<Option<String>, String> {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { return Ok(None) };
        if data.is_empty() || data == "[DONE]" { return Ok(None) }
        let Ok(value) = serde_json::from_str::<Value>(data) else { return Ok(None) };
        if let Some(message) = value["error"]["message"].as_str() { return Err(message.to_string()) }
        match self.protocol {
            Protocol::Anthropic => {
                let index = value["index"].as_u64().unwrap_or(0) as usize;
                match value["type"].as_str() {
                    Some("content_block_start") => {
                        while self.blocks.len() <= index {
                            self.blocks.push(Value::Null);
                            self.inputs.push(String::new());
                        }
                        self.blocks[index] = value["content_block"].clone();
                    }
                    Some("content_block_delta") if index < self.blocks.len() => {
                        let delta = &value["delta"];
                        match delta["type"].as_str() {
                            Some("text_delta") => {
                                let text = delta["text"].as_str().unwrap_or("");
                                push_text(&mut self.blocks[index]["text"], text);
                                if !text.is_empty() { return Ok(Some(text.to_string())) }
                            }
                            Some("input_json_delta") => self.inputs[index].push_str(delta["partial_json"].as_str().unwrap_or("")),
                            // Thinking goes back to the model unchanged with the next step, signature included.
                            Some("thinking_delta") => push_text(&mut self.blocks[index]["thinking"], delta["thinking"].as_str().unwrap_or("")),
                            Some("signature_delta") => self.blocks[index]["signature"] = delta["signature"].clone(),
                            _ => {}
                        }
                    }
                    Some("message_delta") => {
                        if let Some(reason) = value["delta"]["stop_reason"].as_str() { self.stop = reason.to_string() }
                    }
                    _ => {}
                }
            }
            Protocol::Responses => match value["type"].as_str() {
                Some("response.output_text.delta") => {
                    if let Some(text) = value["delta"].as_str() && !text.is_empty() { return Ok(Some(text.to_string())) }
                }
                // Each finished item whole: a message, a function call with its arguments, a reasoning item.
                Some("response.output_item.done") => {
                    let index = value["output_index"].as_u64().unwrap_or(self.blocks.len() as u64) as usize;
                    while self.blocks.len() <= index { self.blocks.push(Value::Null) }
                    self.blocks[index] = value["item"].clone();
                }
                Some("response.completed") => self.stop = "stop".into(),
                Some("response.incomplete") => {
                    self.stop = match value["response"]["incomplete_details"]["reason"].as_str() {
                        Some("content_filter") => "content_filter",
                        _ => "length",
                    }
                    .into();
                }
                Some("response.failed") => return Err(value["response"]["error"]["message"].as_str().unwrap_or("模型服务没有完成这次回答。").to_string()),
                Some("error") => return Err(value["message"].as_str().unwrap_or("模型服务返回了错误。").to_string()),
                _ => {}
            },
            Protocol::OpenAI => {
                let choice = &value["choices"][0];
                if let Some(reason) = choice["finish_reason"].as_str() { self.stop = reason.to_string() }
                for call in choice["delta"]["tool_calls"].as_array().into_iter().flatten() {
                    let index = call["index"].as_u64().unwrap_or(self.calls.len().saturating_sub(1) as u64) as usize;
                    while self.calls.len() <= index { self.calls.push(Default::default()) }
                    let (id, name, arguments) = &mut self.calls[index];
                    if let Some(value) = call["id"].as_str() && !value.is_empty() { *id = value.to_string() }
                    name.push_str(call["function"]["name"].as_str().unwrap_or(""));
                    arguments.push_str(call["function"]["arguments"].as_str().unwrap_or(""));
                }
                if let Some(text) = choice["delta"]["content"].as_str() && !text.is_empty() {
                    self.text.push_str(text);
                    return Ok(Some(text.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// The turn, once the stream has ended.
    pub fn finish(self) -> Turn {
        let parse = |raw: &str| -> Result<Value, String> {
            if raw.trim().is_empty() { return Ok(json!({})) }
            match serde_json::from_str::<Value>(raw) {
                Ok(value) if value.is_object() => Ok(value),
                Ok(_) => Err("工具参数应为一个 JSON 对象。".into()),
                Err(err) => Err(format!("工具参数不是有效的 JSON：{err}")),
            }
        };
        let stop = if self.stop.is_empty() { "end".to_string() } else { self.stop };
        let mut calls = vec![];
        match self.protocol {
            Protocol::Anthropic => {
                // When another model took over mid-answer, what the first one thought and
                // called before the `fallback` block is not sent back, and its calls do not run.
                let boundary = self.blocks.iter().rposition(|b| b["type"] == "fallback").unwrap_or(0);
                let mut kept = vec![];
                for (index, mut block) in self.blocks.into_iter().enumerate() {
                    let kind = block["type"].as_str().unwrap_or("").to_string();
                    if index < boundary && matches!(kind.as_str(), "thinking" | "redacted_thinking" | "tool_use") { continue }
                    match kind.as_str() {
                        "tool_use" => {
                            let args = parse(&self.inputs[index]);
                            block["input"] = args.clone().unwrap_or_else(|_| json!({}));
                            calls.push(ToolCall { id: block["id"].as_str().unwrap_or("").to_string(), name: block["name"].as_str().unwrap_or("").to_string(), args });
                        }
                        // The API rejects an empty text block when it is sent back.
                        "text" if block["text"].as_str().unwrap_or("").is_empty() => continue,
                        "" => continue,
                        _ => {}
                    }
                    kept.push(block);
                }
                let message = if kept.is_empty() { Value::Null } else { json!({"role": "assistant", "content": kept}) };
                Turn { message, calls, stop }
            }
            Protocol::Responses => {
                // The output items go back as they came, reasoning included: the service
                // wants a function call handed in again together with the reasoning before it.
                let items: Vec<Value> = self.blocks.into_iter().filter(|item| !item.is_null()).collect();
                for item in items.iter().filter(|item| item["type"] == "function_call") {
                    calls.push(ToolCall { id: item["call_id"].as_str().unwrap_or("").to_string(), name: item["name"].as_str().unwrap_or("").to_string(), args: parse(item["arguments"].as_str().unwrap_or("")) });
                }
                // The stream names no reason for stopping to call tools; the calls themselves say it.
                let stop = if stop == "stop" && !calls.is_empty() { "tool_calls".to_string() } else { stop };
                Turn { message: if items.is_empty() { Value::Null } else { Value::Array(items) }, calls, stop }
            }
            Protocol::OpenAI => {
                let mut wire_calls = vec![];
                for (n, (id, name, arguments)) in self.calls.into_iter().enumerate() {
                    if name.is_empty() { continue }
                    // Some compatible servers leave the id out.
                    let id = if id.is_empty() { format!("call_{n}") } else { id };
                    let args = parse(&arguments);
                    wire_calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": if args.is_ok() && !arguments.trim().is_empty() { arguments } else { "{}".to_string() }}}));
                    calls.push(ToolCall { id, name, args });
                }
                let message = if self.text.is_empty() && wire_calls.is_empty() {
                    Value::Null
                } else {
                    let mut message = json!({"role": "assistant", "content": if self.text.is_empty() { Value::Null } else { json!(self.text) }});
                    if !wire_calls.is_empty() { message["tool_calls"] = json!(wire_calls) }
                    message
                };
                Turn { message, calls, stop }
            }
        }
    }
}

/// The messages that answer a turn's tool calls: (call id, output, failed).
/// Anthropic takes every result of a turn in one user message.
pub fn tool_results(protocol: Protocol, results: &[(String, String, bool)]) -> Vec<Value> {
    match protocol {
        Protocol::Anthropic => vec![json!({"role": "user", "content": results.iter().map(|(id, output, failed)| json!({"type": "tool_result", "tool_use_id": id, "content": output, "is_error": failed})).collect::<Vec<_>>()})],
        Protocol::OpenAI => results.iter().map(|(id, output, _)| json!({"role": "tool", "tool_call_id": id, "content": output})).collect(),
        Protocol::Responses => results.iter().map(|(id, output, _)| json!({"type": "function_call_output", "call_id": id, "output": output})).collect(),
    }
}

/// Add a word from the app to the tool results that are about to go back,
/// without a turn of its own: for Claude a text block after the results in
/// the same message, for the other two a message after them.
pub fn with_note(protocol: Protocol, mut messages: Vec<Value>, note: &str) -> Vec<Value> {
    match (protocol, messages.last_mut().and_then(|last| last["content"].as_array_mut())) {
        (Protocol::Anthropic, Some(blocks)) => blocks.push(json!({"type": "text", "text": note})),
        _ => messages.push(user_message(note)),
    }
    messages
}

/// What the model is told with the results before the last step it gets for a question.
pub const LAST_STEP_NOTE: &str = "（系统提示）这次提问的步数快用完了：下一步请根据已经查到的内容直接回答，不要再调用工具。还没查完的部分在回答里说明，用户可以接着问。";

// ---- labels in tool calls

/// The paper a label or an id names: `P3` (any case) or the id itself.
fn source_of<'a>(sources: &'a [Source], value: &str) -> Option<&'a Source> {
    sources.iter().find(|s| s.paper_id == value || s.label.eq_ignore_ascii_case(value.trim()))
}

/// Let the model name a paper by its label: `paperId` and `id` arguments
/// holding one are replaced by the paper's id.
pub fn resolve_labels(args: &mut Value, sources: &[Source]) {
    for key in ["paperId", "id"] {
        if let Some(found) = args[key].as_str().and_then(|value| source_of(sources, value)) { args[key] = json!(found.paper_id) }
    }
}

/// Put the label beside every paper id in a tool result, so the answer can cite it.
pub fn add_labels(value: &mut Value, sources: &[Source]) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|item| add_labels(item, sources)),
        Value::Object(map) => {
            let label = ["paperId", "id"].iter().find_map(|key| map.get(*key).and_then(Value::as_str).and_then(|id| sources.iter().find(|s| s.paper_id == id))).map(|s| s.label.clone());
            if let Some(label) = label { map.insert("label".into(), json!(label)); }
            map.values_mut().for_each(|item| add_labels(item, sources));
        }
        _ => {}
    }
}

// ---- slash commands

/// A command typed at the start of a message.
pub struct Slash {
    /// What follows the slash.
    pub name: &'static str,
    pub about: &'static str,
    /// What to write after the name; empty when the command stands alone.
    pub hint: &'static str,
    /// The request it stands for (`{}` is what follows the name); empty for
    /// a command the window carries out itself.
    pub template: &'static str,
    /// What the command works on when nothing follows its name; empty when something must.
    pub alone: &'static str,
}

pub const SLASH: [Slash; 7] = [
    Slash {
        name: "search",
        about: "在全文和记录里检索",
        hint: "关键词或短语",
        template: "在文献库里检索：{}\n\n先用 search_fulltext 查已保存的全文，再用 list_papers 查题目、摘要、标签和笔记。列出命中的文献；全文命中的给出页码和原句。有文献因为没有保存全文而只查了记录时，说明这一点。",
        alone: "",
    },
    Slash {
        name: "cite",
        about: "为一段文字逐句配引用",
        hint: "要配引用的文字",
        template: "为下面这段文字逐句找出文献库中可以引用的文献：\n\n{}\n\n先调用 find_citations 取得每句的候选，再用 read_fulltext 或文献记录核对候选是否真的支持这句话。逐句给出结果：可以引用的文献（标签和页码）和它依据的原句；没有合适文献的句子直接说明没有，不要勉强配引用。",
        alone: "",
    },
    Slash {
        name: "review",
        about: "确认要求和大纲，分阶段撰写带引用的综述",
        hint: "主题",
        template: "围绕下面的主题，依据可核实的文献撰写综述：{}\n\n按以下步骤进行；用户只要求大纲或某一部分时，按其指定范围交付。\n\n1. 确认写作要求：整理用户已给出的核心问题、范围、读者、语言、综述类型、篇幅、文献时间范围与引用要求；其他影响任务的要求（如图表）也列出。未给出的标为待定或提出建议，不能当作用户已经确认。集中询问会影响写作的缺项，等待用户确认或调整；用户已明确授权按建议继续时，记录采用的要求即可。不要虚构约束。\n\n2. 检索与大纲：用 list_papers 和 search_fulltext 检索文献库，必要时用 get_paper 读取记录；如需扩展，可用 search_online 试查文献数据库，并说明它返回的是候选记录，不等于已阅读全文。按确认的范围给出全文主线、各节要回答的问题、代表文献、预计篇幅和证据缺口。列明每个来源实际读到全文、摘要还是仅有题录；需要补查的地方单独列出。展示大纲后结束本轮，等待用户明确确认；用户提出调整时更新大纲并再次确认，再继续写正文。\n\n3. 确认大纲后补充证据：按章节针对缺口和分歧检索，必要时用 search_fulltext 定位并用 read_fulltext 阅读相关页及上下文，核对对象、方法、单位、统计口径和局限。关键数字、研究间比较和有争议的结论动笔前核对原文；不能取得或读懂原文时如实标记，不能把摘要、检索片段或题录当成全文。需要补充文献时说明缺口；取得开放全文或导入记录须按工具能力和用户意愿进行。\n\n4. 分节写作：按已确认的大纲和篇幅要求组织引言、主题章节及结论；不要逐篇罗列。每段围绕一个中心判断比较回答同一问题的研究，说明一致、分歧及可能的原因，不把推测当作事实。区分原文发现与综合判断、相关性与因果；叙述强度与证据相称。每个有依据的论断给引用标签，依据全文的写上实际页码；库外候选没有文献库标签时写明可核实的来源，不要编造标签；只有摘要可依据的明确标为摘要，不编造页码、来源或结论。证据不足时说明其影响，不用常识补齐。\n\n5. 核对与交付：逐项检查重要论断与引用是否相符、数字和适用条件是否一致，复核引言、正文与结论，说明未能核对的内容和剩余证据缺口。按实际完成情况报告篇幅和使用的独立文献数量，不确定时说明未统计。输出可供用户保存到写作页的完整内容；未实际保存时不得声称已经保存。",
        alone: "",
    },
    Slash {
        name: "compare",
        about: "比较几篇文献的方法和结论",
        hint: "文献标签或题目，如 P1 P3",
        template: "比较下面这几篇文献：{}\n\n先用 get_paper 和 read_fulltext 把它们读一遍。用一张表列出各自的研究问题、材料与方法、主要结论和局限，表后用几句话说明它们一致、互补和矛盾的地方。依据全文的内容给出页码；只有摘要可依据的文献要说明。",
        alone: "",
    },
    Slash {
        name: "write",
        about: "参照文献库里同一部分的写法起草一段",
        hint: "哪一部分和要写的内容，如：导言 根系如何感知土壤紧实",
        template: "参照文献库里的文献，帮我起草论文的一段：{}\n\n先判断这段属于论文的哪个部分（摘要、导言、方法、结果、讨论），用 find_sentences 取这一部分的候选句：先不带 query 看各篇文献怎么开头，再用英文关键词查与要写的内容相近的句子。然后用英文写出这一段：借鉴候选句的结构和措辞，但句子要自己写，不要照抄；引述他人结论的地方给出引用标签和页码。写完后列出你参考的 3 到 5 个候选句（原句、标签、页码），说明各借鉴了什么。文献库里没有这一部分的句子可参考时直接说明。",
        alone: "",
    },
    Slash {
        name: "verify",
        about: "逐条核对上一个回答的依据",
        hint: "要核对的论断；不写就核对上一个回答",
        template: "逐条核对下面这段内容的依据：{}\n\n写的是「上一个回答」时，核对的是你在这次对话里给出的上一个回答。做法：\n1. 把其中的论断一条条列出来，不合并、不遗漏。\n2. 对每一条，用 search_fulltext 找到出处，再用 read_fulltext 读那一页的上下文；只凭关键词命中不算核对。\n3. 每一条按这个顺序写：论断 → 原文摘录（写成 [P3 p.5 \"逐字的一句原文\"]）→ 这句原文支持到什么程度、有什么局限。\n\n要求：摘录必须逐字出自那一页，不翻译、不改写、不把几处拼成一句；界面会拿它和原文逐字比对，并标出比对的结果。只有摘要可依据的，说明依据是摘要，不写页码。没有读到全文、或者原文里找不到依据的，直接写「无法核对」并说明原因，不要用印象补上。原文和论断不一致的，照实指出。最后用一两句话说明哪几条有原文依据、哪几条没有。",
        alone: "上一个回答",
    },
    Slash { name: "new", about: "开始一次新对话", hint: "", template: "", alone: "" },
];

/// A command the person wrote: a name and the request it stands for, kept
/// in `Settings::commands`. It is a prompt template and nothing more: it
/// runs nothing and reaches nothing a typed question could not.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Command {
    /// What follows the slash: letters, digits, `-` and `_`, in lower case.
    pub name: String,
    pub about: String,
    /// The request. `{}` is where what follows the name goes, and then
    /// something must follow; without it, what follows is added at the end.
    pub template: String,
}

impl Command {
    /// The request this command stands for with `argument` after its name.
    fn expand(&self, argument: &str) -> Result<String, String> {
        let (name, template) = (&self.name, self.template.trim());
        if template.is_empty() { return Err(format!("/{name} 还没有写模板。在「设置 › 命令」里补上。")) }
        match (template.contains("{}"), argument.is_empty()) {
            (true, true) => Err(format!("/{name} 后面写上内容：模板里的 {{}} 会换成它。")),
            (true, false) => Ok(template.replace("{}", argument)),
            (false, true) => Ok(template.to_string()),
            (false, false) => Ok(format!("{template}\n\n{argument}")),
        }
    }
}

/// What is wrong with a name for a command of one's own, if anything.
/// `others` are the names of the person's other commands.
pub fn command_name_problem<'a>(name: &str, mut others: impl Iterator<Item = &'a str>) -> Option<String> {
    if name.is_empty() { return Some("命令要有名称。".into()) }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') { return Some("名称只能用字母、数字、汉字、- 和 _，不能有空格。".into()) }
    if name.chars().count() > 32 { return Some("名称最多 32 个字。".into()) }
    if name.chars().any(char::is_uppercase) { return Some("名称里的字母用小写。".into()) }
    if SLASH.iter().any(|command| command.name == name) { return Some(format!("/{name} 是内置命令，换一个名称。")) }
    if others.any(|other| other == name) { return Some(format!("已经有一个 /{name} 了。")) }
    None
}

/// What a typed message asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A request for the model: the message itself, or what a slash command stands for.
    Ask(String),
    /// `/new`: leave this conversation and start another.
    NewChat,
}

/// Read a typed message; `custom` are the person's own commands, which a
/// built-in one of the same name comes before. The error is a sentence for
/// the person: an unknown command, or one that is missing what it works on.
pub fn read_input(input: &str, custom: &[Command]) -> Result<Input, String> {
    let input = input.trim();
    let Some(rest) = input.strip_prefix('/') else { return Ok(Input::Ask(input.to_string())) };
    let (name, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let Some(command) = SLASH.iter().find(|command| command.name == name) else {
        if let Some(command) = custom.iter().find(|command| command.name == name) { return command.expand(argument.trim()).map(Input::Ask) }
        let known = SLASH.iter().map(|command| (command.name, command.about)).chain(custom.iter().map(|command| (command.name.as_str(), command.about.as_str())));
        return Err(format!("没有 /{name} 这个命令。可用：{}", known.map(|(name, about)| if about.is_empty() { format!("/{name}") } else { format!("/{name}（{about}）") }).collect::<Vec<_>>().join("、")));
    };
    if command.template.is_empty() { return Ok(Input::NewChat) }
    let argument = if argument.trim().is_empty() { command.alone } else { argument.trim() };
    if argument.is_empty() { return Err(format!("/{name} 后面写上内容：{}。", command.about)) }
    Ok(Input::Ask(command.template.replace("{}", argument)))
}

/// The request behind a question kept before conversations kept what they
/// sent (`Said::User::prompt`): a built-in command is expanded as it is
/// now. A command of the person's own is not, since its template then is
/// not known: the question goes as it was typed.
pub fn expand_slash(input: &str) -> String {
    match read_input(input, &[]) {
        Ok(Input::Ask(prompt)) => prompt,
        _ => input.trim().to_string(),
    }
}

/// A command on offer over the box.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub name: String,
    pub about: String,
    /// What to write after the name.
    pub hint: String,
    /// One of the person's own.
    pub custom: bool,
    /// Taking it carries it out at once (`/new`); the others are written out for the person to go on.
    pub runs: bool,
}

/// The commands to offer while a message is still the beginning of one: a
/// slash and perhaps part of a name, nothing after it. Those whose name
/// starts with what was typed come first, then those described by it
/// (`/引` finds 「配引用」); the built-in ones before the person's own.
pub fn slash_matches(input: &str, custom: &[Command]) -> Vec<Offer> {
    let Some(typed) = input.strip_prefix('/') else { return vec![] };
    if typed.chars().any(char::is_whitespace) { return vec![] }
    let typed = typed.to_lowercase();
    let builtin = SLASH.iter().map(|command| Offer { name: command.name.into(), about: command.about.into(), hint: command.hint.into(), custom: false, runs: command.template.is_empty() });
    // One that a built-in command hides, or that has no name yet, cannot be called.
    let own = custom.iter().filter(|command| command_name_problem(&command.name, std::iter::empty()).is_none()).map(|command| Offer {
        name: command.name.clone(),
        about: command.about.clone(),
        hint: if command.template.contains("{}") { "要处理的内容" } else { "可以接着补充" }.into(),
        custom: true,
        runs: false,
    });
    let all: Vec<Offer> = builtin.chain(own).collect();
    let (mut found, rest): (Vec<Offer>, Vec<Offer>) = all.into_iter().partition(|offer| offer.name.starts_with(&typed));
    if !typed.is_empty() { found.extend(rest.into_iter().filter(|offer| offer.about.to_lowercase().contains(&typed))) }
    found
}

// ---- what a conversation leaves behind

/// One thing said in a conversation, as it is kept in the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Said {
    /// A question as typed, with the ids of the pictures that went with it.
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
        /// What went to the model where that is not the text: what a slash
        /// command stood for when it was sent, so a template changed or
        /// removed since does not change what was asked. Empty in
        /// conversations from before this was kept.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        prompt: String,
    },
    Answer { text: String },
    /// A tool the assistant called, as the window described it, and what came back.
    Tool {
        call: String,
        #[serde(default)]
        result: String,
        #[serde(default)]
        failed: bool,
    },
}

/// A conversation as it is kept: what was said, and the papers its labels
/// name. What the model read is not kept: a conversation taken up again
/// reads the library as it is then, under the same labels.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    pub items: Vec<Said>,
    pub sources: Vec<Source>,
}

impl Transcript {
    /// What the conversation is listed as: the start of its first question.
    pub fn title(&self) -> String {
        let first = self.items.iter().find_map(|item| match item {
            Said::User { text, images, .. } => Some((text.split_whitespace().collect::<Vec<_>>().join(" "), !images.is_empty())),
            _ => None,
        });
        match first {
            Some((text, _)) if !text.is_empty() => {
                if text.chars().count() > 48 { format!("{}…", text.chars().take(48).collect::<String>()) } else { text }
            }
            Some((_, true)) => "（图片）".into(),
            _ => "未命名对话".into(),
        }
    }

    pub fn questions(&self) -> usize {
        self.items.iter().filter(|item| matches!(item, Said::User { .. })).count()
    }
}

// ---- one-off requests

/// A single question with its own instructions and no library in front of
/// the model: what the guide's query suggestions are asked with.
pub fn plain_request(profile: &Profile, system: &str, prompt: &str) -> Result<HttpRequest, String> {
    let mut request = chat_request(profile, &Context { instructions: system.into(), ..Default::default() }, &[user_message(prompt)], &[])?;
    match profile.protocol {
        // No documents to cache, and an empty text block would be refused.
        Protocol::Anthropic => {
            request.body["system"] = json!(system);
            if let Some(body) = request.body.as_object_mut() { body.remove("cache_control"); }
        }
        Protocol::OpenAI => request.body["messages"][0]["content"] = json!(system),
        Protocol::Responses => request.body["instructions"] = json!(system),
    }
    Ok(request)
}

/// What a model is told when it reads a page of a paper that gave no text.
pub const READ_PAGE_PROMPT: &str = "这是一篇学术论文的一页的图像。按阅读顺序逐字转写这一页的文字（双栏先左栏后右栏）。标题单独成行；段落之间空一行；同一段内不要换行，行末被连字符拆开的单词要接回去。\n\
不要转写页眉页脚和页码、图片里的文字、表格内容；图注和表注可以保留。保持原文的语言，不要翻译、不要总结、不要添加任何说明或 Markdown 标记。\n这一页没有正文文字时，只输出 EMPTY。";

/// A request for the text of one page, given as a picture.
pub fn read_page_request(profile: &Profile, image: ImageData) -> Result<HttpRequest, String> {
    picture_request(profile, "你是一个准确的文字转写工具。", READ_PAGE_PROMPT, image)
}

/// A one-off question about one picture, outside any conversation.
pub fn picture_request(profile: &Profile, system: &str, prompt: &str, image: ImageData) -> Result<HttpRequest, String> {
    let mut request = plain_request(profile, system, prompt)?;
    let message = user_message_with(profile.protocol, prompt, &[image]);
    match profile.protocol {
        Protocol::Anthropic => request.body["messages"] = json!([message]),
        Protocol::OpenAI => request.body["messages"][1] = message,
        Protocol::Responses => request.body["input"] = json!([message]),
    }
    Ok(request)
}

/// What a model made of a page: its text, empty when it said there is none.
pub fn page_reading(reply: &str) -> String {
    let text = reply.trim().trim_start_matches("```text").trim_start_matches("```").trim_end_matches("```").trim();
    if text == "EMPTY" { String::new() } else { text.to_string() }
}

// ---- translation

/// What a translation is made with unless the settings say otherwise.
pub const TRANSLATION_PROMPT: &str = "你是翻译助手。把用户给出的文本翻译成简体中文，保留术语、基因名、物种学名和数字原样。\n只输出译文，不加解释、不加引号、不加标题。";

/// The longest text one translation takes, in bytes: a title, an abstract
/// or a passage is far shorter, and anything longer is refused, not cut.
pub const TRANSLATE_LIMIT: usize = 60_000;

/// The instructions in force: the person's own, else `TRANSLATION_PROMPT`.
pub fn translation_prompt(custom: &str) -> &str {
    if custom.trim().is_empty() { TRANSLATION_PROMPT } else { custom.trim() }
}

/// The request that translates `text` under the person's instructions.
pub fn translate_request(profile: &Profile, prompt: &str, text: &str) -> Result<HttpRequest, String> {
    let text = text.trim();
    if text.is_empty() { return Err("没有要翻译的文字。".into()) }
    if text.len() > TRANSLATE_LIMIT { return Err("这段文字太长，无法一次翻译。".into()) }
    plain_request(profile, translation_prompt(prompt), text)
}

// ---- writing assistance

/// What the model is asked to do with a paragraph of the manuscript on the
/// writing page. The answer is shown beside the paragraph; only the person
/// puts it into the draft.
#[derive(Clone, Debug, PartialEq)]
pub enum Assist {
    /// Rewrite it after the candidate sentences beside it, citing where they are drawn on.
    Rewrite,
    /// Put it into the manuscript's language.
    Translate,
    /// Tighten it, changing as little as needed.
    Polish,
    /// One of the person's own commands (`Settings::commands`), by name.
    Own(String),
}

impl Assist {
    /// What the action is called on its button and over its result.
    pub fn title(&self, language: &str) -> String {
        match self {
            Assist::Rewrite => "参照候选句改写".into(),
            Assist::Translate => format!("译成{}", language_name(language)),
            Assist::Polish => "润色".into(),
            Assist::Own(name) => format!("/{name}"),
        }
    }
}

/// A manuscript language as it reads in a sentence: `English` is 英文, others as they are given.
pub fn language_name(language: &str) -> String {
    match language.trim().to_lowercase().as_str() {
        "" | "en" | "english" | "英文" | "英语" => "英文".into(),
        "zh" | "chinese" | "中文" | "汉语" | "简体中文" => "中文".into(),
        other => other.to_string(),
    }
}

/// A candidate sentence a rewrite may draw on: its text and where it is from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Example {
    pub text: String,
    /// How the paper is cited in the draft, `Smith et al., 2020`.
    pub cite: String,
    pub page: Option<usize>,
    pub heading: String,
}

/// The most candidates a rewrite is shown: the closest come first, and more only blur the paragraph.
pub const ASSIST_EXAMPLES: usize = 12;

/// The longest paragraph an action takes, in bytes: the selection may be a whole section, not a book.
pub const ASSIST_LIMIT: usize = 20_000;

/// What a writing request is made of: the instructions and the message.
pub fn assist_prompt(assist: &Assist, language: &str, paragraph: &str, examples: &[Example], own: &[Command]) -> Result<(String, String), String> {
    let paragraph = paragraph.trim();
    if paragraph.is_empty() { return Err("没有要处理的文字：先选中一段，或把光标放在要处理的段落里。".into()) }
    if paragraph.len() > ASSIST_LIMIT { return Err("选中的文字太长，一次处理一两段就好。".into()) }
    let language = language_name(language);
    let system = format!(
        "你是学术写作助手，帮用户修改他正在写的论文稿件。稿件语言是{language}。\n\
         只输出处理后的文字本身：不加解释、不加标题、不加引号，不用 Markdown。段落之间空一行。\n\
         段落里已有的引用（如 (Smith et al., 2020)、[3]）保持原样，不删除、不改写；不要编造文献或数据。"
    );
    let message = match assist {
        Assist::Rewrite => {
            if examples.is_empty() { return Err("右边还没有候选句可参照。先写几句，或在右上角的框里找句子。".into()) }
            let listed: Vec<String> = examples.iter().take(ASSIST_EXAMPLES).enumerate().map(|(n, example)| {
                let mut from = vec![example.cite.trim().to_string()];
                if let Some(page) = example.page { from.push(format!("p. {page}")) }
                if !example.heading.trim().is_empty() { from.push(example.heading.trim().to_string()) }
                from.retain(|part| !part.is_empty());
                format!("[{}] ({}) {}", n + 1, from.join(", "), example.text.trim())
            }).collect();
            format!(
                "参照下面这些候选句，用{language}改写我的段落。候选句出自文献库里的论文同一部分，括号里是它的出处。\n\n\
                 要求：\n\
                 1. 意思以我的段落为准：不增加我没有写的论断，也不删掉我写了的。\n\
                 2. 借鉴候选句的结构、衔接和措辞习惯，但每句话都要自己写，不要照抄候选句（连续相同的词不超过五六个）。\n\
                 3. 我的段落里已有的引用一个都不能删、不能改，留在它所支持的那句话上。\n\
                 4. 我的段落在陈述某篇候选论文的发现时，补上这篇论文的引用：用候选句前面括号里的出处，写成 (作者, 年份) 的样子。候选句正文里引用的其他文献不是这篇论文本身，不要搬过来；也不要给我自己的论断配引用。\n\n\
                 候选句：\n{}\n\n要改写的段落：\n{paragraph}",
                listed.join("\n")
            )
        }
        Assist::Translate => format!(
            "把下面这段译成{language}的学术论文文字：意思准确，用这个领域的论文惯用的说法，而不是逐字直译。\n\
             术语、基因名、物种学名、数字和单位保持原样；已经是{language}的部分照原样保留。\n\n{paragraph}"
        ),
        Assist::Polish => format!(
            "润色下面这段{language}文字：改得越少越好，只改语法错误、不准确或不地道的用词、冗余和不通顺的地方。\n\
             不改变意思，不增删论断，不调整段落的结构；本来就好的句子原样保留。\n\n{paragraph}"
        ),
        Assist::Own(name) => {
            let Some(command) = own.iter().find(|command| command.name == *name) else { return Err(format!("没有 /{name} 这个命令了，可能已在「设置 › 命令」里删掉。")) };
            return Ok((
                format!(
                    "你是学术写作助手，帮用户处理他正在写的论文稿件里的一段文字。稿件语言是{language}。\n\
                     要求是修改这段文字时，只输出修改后的文字，不加解释；要求是检查或评论时，直接给出意见。不用 Markdown。\n\
                     段落里已有的引用保持原样；不要编造文献或数据。"
                ),
                command.expand(paragraph)?,
            ));
        }
    };
    Ok((system, message))
}

/// The request for one action on a paragraph.
pub fn assist_request(profile: &Profile, assist: &Assist, language: &str, paragraph: &str, examples: &[Example], own: &[Command]) -> Result<HttpRequest, String> {
    let (system, message) = assist_prompt(assist, language, paragraph, examples, own)?;
    plain_request(profile, &system, &message)
}

/// The paragraph an action works on when nothing is selected: the line the
/// caret is in, without the spaces around it. A caret on an empty line
/// takes the paragraph before it, where one has just been finished.
pub fn paragraph_at(text: &str, caret: usize) -> std::ops::Range<usize> {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    let line = |at: usize| (text[..at].rfind('\n').map_or(0, |i| i + 1), text[at..].find('\n').map_or(text.len(), |i| at + i));
    let (mut start, mut end) = line(caret);
    if text[start..end].trim().is_empty() {
        let before = text[..start].trim_end();
        if before.is_empty() { return caret..caret }
        (start, end) = line(before.len());
    }
    let trimmed = text[start..end].trim_start();
    let start = end - trimmed.len();
    start..start + trimmed.trim_end().len()
}

/// A piece of text in a comparison of two versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Same,
    /// Only in the first version.
    Removed,
    /// Only in the second.
    Added,
}

/// The words a comparison goes by: runs of letters and digits, every other
/// character (a Chinese character, a space, a mark) on its own.
fn diff_tokens(text: &str) -> Vec<&str> {
    let mut tokens = vec![];
    let mut start = None;
    for (at, c) in text.char_indices() {
        let wordish = c.is_alphanumeric() && (c as u32) < 0x2E80;
        match (wordish, start) {
            (true, None) => start = Some(at),
            (true, Some(_)) => {}
            (false, from) => {
                if let Some(from) = from { tokens.push(&text[from..at]) }
                start = None;
                tokens.push(&text[at..at + c.len_utf8()]);
            }
        }
    }
    if let Some(from) = start { tokens.push(&text[from..]) }
    tokens
}

/// The most words of each version a comparison is made for: beyond that the
/// table would be large, and the texts are shown as they are.
const DIFF_TOKENS: usize = 1500;

/// Where two versions of a paragraph differ, word by word, as pieces of the
/// first (`Same`, `Removed`) and of the second (`Same`, `Added`). `None`
/// when they share too little for marks to help (a translation), or are
/// too long to compare.
pub fn word_diff(before: &str, after: &str) -> Option<(Vec<(Change, String)>, Vec<(Change, String)>)> {
    let (a, b) = (diff_tokens(before), diff_tokens(after));
    if a.len() > DIFF_TOKENS || b.len() > DIFF_TOKENS || a.is_empty() || b.is_empty() { return None }
    // Longest common subsequence, from the end.
    let mut table = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    if (table[0][0] as usize) * 2 < a.len().min(b.len()) { return None }
    let (mut left, mut right): (Vec<(Change, String)>, Vec<(Change, String)>) = (vec![], vec![]);
    let push = |side: &mut Vec<(Change, String)>, change: Change, token: &str| match side.last_mut() {
        Some((last, text)) if *last == change => text.push_str(token),
        _ => side.push((change, token.to_string())),
    };
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            push(&mut left, Change::Same, a[i]);
            push(&mut right, Change::Same, b[j]);
            (i, j) = (i + 1, j + 1);
        } else if j < b.len() && (i == a.len() || table[i][j + 1] >= table[i + 1][j]) {
            push(&mut right, Change::Added, b[j]);
            j += 1;
        } else {
            push(&mut left, Change::Removed, a[i]);
            i += 1;
        }
    }
    Some((left, right))
}

/// A search the model proposes for one literature database.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryIdea {
    /// A key of `sources::KINDS`.
    pub source: String,
    pub query: String,
    /// What this query aims at, in a sentence.
    pub why: String,
}

/// The instructions and the question that ask for search queries on a
/// topic, for the databases in `kinds`.
pub fn query_prompt(library_name: &str, topic: &str, keywords: &str, kinds: &[String]) -> (String, String) {
    let system = "你是文献检索专家，熟悉各文献数据库的检索语法。你的任务是把一个研究主题写成可以直接运行的检索式。".to_string();
    let mut prompt = format!("文献库：{library_name}\n");
    if !topic.trim().is_empty() { prompt.push_str(&format!("主题：{}\n", topic.trim())) }
    if !keywords.trim().is_empty() { prompt.push_str(&format!("用户给出的关键词或说明：{}\n", keywords.trim())) }
    prompt.push_str("\n要检索的数据库和它们的语法：\n");
    for kind in kinds {
        prompt.push_str(&format!("- {kind}：{}\n", crate::sources::syntax(kind)));
    }
    prompt.push_str(
        "\n为每个数据库写 2 到 3 条检索式，总数不超过 8 条。同一个数据库的几条要有明显不同的取向，让用户能比较哪条靠谱：\
例如一条宽（核心概念加同义词，宁多勿漏），一条聚焦（加上限定，命中少而准），一条换一个角度或术语体系。\n\
关键词是中文时，换成英文的专业术语，并补上常用的同义词、缩写和拼写变体。检索式必须符合该数据库的语法，可以直接粘贴运行。\n\n\
只输出一个 JSON 数组，不要任何其他文字：\n\
[{\"source\": \"数据库的键，如 pubmed\", \"query\": \"检索式\", \"why\": \"一句中文，说明这条检索式的取向和它可能漏掉或多带进来的东西\"}]",
    );
    (system, prompt)
}

/// The queries in a reply to `query_prompt`. The array may sit inside other
/// text or a code fence; entries for an unknown database or without a query
/// are dropped.
pub fn parse_queries(reply: &str) -> Result<Vec<QueryIdea>, String> {
    let ideas: Vec<QueryIdea> = reply
        .find('[')
        .zip(reply.rfind(']'))
        .filter(|(start, end)| start < end)
        .and_then(|(start, end)| serde_json::from_str::<Vec<Value>>(&reply[start..=end]).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let field = |key: &str| item[key].as_str().unwrap_or("").trim().to_string();
            let idea = QueryIdea { source: field("source").to_lowercase(), query: field("query"), why: field("why") };
            (crate::sources::KINDS.iter().any(|(key, _)| *key == idea.source) && !idea.query.is_empty()).then_some(idea)
        })
        .collect();
    if ideas.is_empty() { return Err("模型没有给出可用的检索式。可以换个说法再试一次，或者自己新建检索式。".into()) }
    Ok(ideas)
}

// ---- screening suggestions

/// Room for a paper's full text in one screening or extraction request, in bytes.
const SCREEN_BUDGET: usize = 240_000;

const SCREEN_SYSTEM: &str = "你是文献筛选助手。用户正在为一个文献库逐篇筛选文献，会给出筛选标准和一篇文献的信息（题目、书目信息、摘要，可能还有全文）。\n\
判断这篇文献属于哪一类：\n\
- include：符合筛选标准，是这个文献库要收的文献。\n\
- related：不完全符合，但与主题相关，可能有参考价值，值得留着备查。\n\
- irrelevant：与主题无关。\n\
- uncertain：给出的信息不足以判断。\n\
要求：\n\
- 只依据给出的内容判断，不要补充文中没有的信息。\n\
- reason 用中文写 1 到 3 句，说出文中的具体依据。\n\
- excerpt 是支持判断的一段原文，必须逐字摘自给出的摘要或全文，不超过 300 字，不翻译、不改写、不把几处拼在一起；没有合适的就留空字符串。\n\
- confidence 是你对这个判断的把握，0 到 1 之间的小数。\n\
只输出一个 JSON 对象，不要其他文字：{\"verdict\": \"include\" | \"related\" | \"irrelevant\" | \"uncertain\", \"reason\": \"…\", \"excerpt\": \"…\", \"confidence\": 0.8}";

/// The instructions and the question that ask whether one paper belongs in
/// the library, and what the model gets to read: "fulltext", "abstract", or
/// "none" when there is neither and asking would be guessing.
pub fn screen_prompt(topic: &str, criteria: &str, paper: &Paper, pages: Option<&[String]>) -> (String, String, &'static str) {
    let mut prompt = String::new();
    if !criteria.trim().is_empty() { prompt.push_str(&format!("筛选标准：{}\n", criteria.trim())) }
    if !topic.trim().is_empty() { prompt.push_str(&format!("文献库的主题：{}\n", topic.trim())) }
    let (text, basis) = paper_to_read(paper, pages);
    prompt.push_str(&text);
    (SCREEN_SYSTEM.to_string(), prompt, basis)
}

/// A paper as a model reads it one at a time (筛选, 提取): its record, its
/// abstract and, when stored, its full text page by page; and what that
/// comes to: "fulltext", "abstract", or "none" when there is neither.
pub fn paper_to_read(paper: &Paper, pages: Option<&[String]>) -> (String, &'static str) {
    let mut prompt = format!("\n## 文献\n题目：{}\n", paper.title);
    for (name, value) in [("作者", paper.authors.clone()), ("期刊", paper.journal.clone()), ("年份", paper.year.map(|y| y.to_string()).unwrap_or_default())] {
        if !value.trim().is_empty() { prompt.push_str(&format!("{name}：{}\n", value.trim())) }
    }
    if !paper.abstract_text.trim().is_empty() { prompt.push_str(&format!("摘要：{}\n", paper.abstract_text.trim())) }
    let pages = pages.filter(|pages| pages.iter().any(|page| !page.trim().is_empty()));
    if let Some(pages) = pages {
        prompt.push_str("\n## 全文（逐页）\n");
        let mut used = 0;
        for (n, page) in pages.iter().enumerate() {
            if used + page.len() > SCREEN_BUDGET {
                prompt.push_str(&format!("\n（后面的 {} 页因篇幅未提供。）\n", pages.len() - n));
                break;
            }
            used += page.len();
            prompt.push_str(&format!("\n[p.{}]\n{}\n", n + 1, page.trim()));
        }
    }
    let basis = if pages.is_some() { "fulltext" } else if paper.abstract_text.trim().is_empty() { "none" } else { "abstract" };
    (prompt, basis)
}

/// Where a quotation stands: `Some(None)` in the abstract or title,
/// `Some(Some(page))` on a full-text page, `None` when it is in neither.
/// Only letters and digits are compared, so the spacing, line breaks and
/// hyphenation of PDF text do not hide a quotation that is really there.
pub fn locate_quote(quote: &str, paper: &Paper, pages: Option<&[String]>) -> Option<Option<usize>> {
    let wanted = bare(quote);
    // A few characters would be found anywhere.
    if wanted.chars().count() < 8 { return None }
    if bare(&paper.abstract_text).contains(&wanted) || bare(&paper.title).contains(&wanted) { return Some(None) }
    pages.into_iter().flatten().position(|page| bare(page).contains(&wanted)).map(|n| Some(n + 1))
}

/// What the model said about one paper, with its quotation checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    /// A key of `SCREENINGS`: included, excluded (shown as 「相关」), irrelevant or uncertain.
    pub verdict: String,
    pub reason: String,
    /// The quotation, when it really is in the abstract or the full text; empty otherwise.
    pub excerpt: String,
    pub page: Option<usize>,
    pub confidence: Option<f64>,
}

/// Read the reply to `screen_prompt`. A quotation that is not in the text
/// the model was given is dropped rather than shown as evidence.
pub fn parse_judgement(reply: &str, paper: &Paper, pages: Option<&[String]>) -> Result<Judgement, String> {
    let value: Value = reply.find('{').zip(reply.rfind('}')).filter(|(start, end)| start < end).and_then(|(start, end)| serde_json::from_str(&reply[start..=end]).ok()).ok_or("模型的回答不是要求的格式。")?;
    let verdict = match value["verdict"].as_str().unwrap_or("").trim().to_lowercase().as_str() {
        "include" | "included" => "included",
        "related" => "excluded",
        "irrelevant" => "irrelevant",
        "uncertain" => "uncertain",
        other => return Err(format!("模型给出了不认识的结论：{other}")),
    };
    let quote = value["excerpt"].as_str().unwrap_or("").trim();
    let found = locate_quote(quote, paper, pages);
    Ok(Judgement {
        verdict: verdict.into(),
        reason: value["reason"].as_str().unwrap_or("").trim().to_string(),
        excerpt: if found.is_some() { quote.to_string() } else { String::new() },
        page: found.flatten(),
        confidence: value["confidence"].as_f64().map(|c| c.clamp(0., 1.)),
    })
}

/// What to tell the reader when a turn did not end normally.
pub fn stop_note(reason: &str) -> Option<&'static str> {
    match reason {
        "refusal" | "content_filter" => Some("模型拒绝回答这个问题。"),
        "max_tokens" | "length" => Some("回答达到长度上限，被截断了。"),
        "model_context_window_exceeded" => Some("材料超出了模型的上下文长度，回答可能不完整。请缩小提问范围。"),
        _ => None,
    }
}

/// The message inside an error response body; the body itself when it is not JSON.
pub fn error_message(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().or(v["message"].as_str()).map(String::from)).unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match status {
        401 | 403 => "API Key 无效或没有权限",
        404 => "地址或模型名称不存在",
        429 => "请求过于频繁或额度不足",
        500..=599 => "模型服务暂时不可用",
        _ => "请求被拒绝",
    };
    format!("{hint}（HTTP {status}）：{}", detail.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotations_checked_against_the_text() {
        let paper = Paper { title: "Root atlas".into(), abstract_text: "WOX5 maintains the quiescent centre of the root.".into(), ..Default::default() };
        let pages = vec!["Introduction. The root tip of Arabidopsis.".to_string(), "Libraries were prepared with\n10x Genomics Chromium v3 and sequenced to 50,000 reads per cell.".to_string()];
        let check = |quote: &str, page: Option<usize>| check_quote(quote, page, &paper, Some(&pages));
        // Where it is said to be, whatever the line breaks and the case.
        assert_eq!(check("Libraries were prepared with 10X genomics chromium v3", Some(2)), Quoted::Found);
        // On another page than the one named; without a page, anywhere will do.
        assert_eq!((check("The root tip of Arabidopsis", Some(2)), check("The root tip of Arabidopsis", None)), (Quoted::Elsewhere(1), Quoted::Found));
        // The abstract counts, with or without a text.
        assert_eq!((check("WOX5 maintains the quiescent centre", Some(1)), check_quote("WOX5 maintains the quiescent centre", None, &paper, None)), (Quoted::Found, Quoted::Found));
        // A number changed in its second half: only the beginning is there, and it is said how much.
        let Quoted::Begins(page, share) = check("Libraries were prepared with 10x Genomics Chromium v3 and sequenced to 80,000 reads per cell.", Some(2)) else { panic!("not a beginning") };
        assert!(page == 2 && share > 0.6 && share < 0.9, "{share}");
        // Made up, and nothing to look in.
        assert_eq!((check("Protoplasts were sorted by flow cytometry before loading", Some(2)), check_quote("anything at all", Some(1), &paper, None)), (Quoted::Missing, Quoted::NoText));
        // A few shared first letters are no beginning.
        assert_eq!(check("Libraries of congress hold many books about roots", Some(2)), Quoted::Missing);
    }

    #[test]
    fn where_an_answer_cites() {
        let answer = "根尖 [P3] 用了 [P1 p.2 \"Libraries [12] were prepared.\"]，见 [1] 和 [P2, p.7]。";
        let spans = citation_spans(answer);
        assert_eq!(spans.iter().map(|(range, _)| &answer[range.clone()]).collect::<Vec<_>>(), ["[P3]", "[P1 p.2 \"Libraries [12] were prepared.\"]", "[P2, p.7]"]);
        assert_eq!(spans[1].1, [Citation { label: "P1".into(), page: Some(2), quote: "Libraries [12] were prepared.".into() }]);
    }


    #[test]
    fn an_answer_becomes_a_note() {
        let sources = [Source { label: "P1".into(), paper_id: "a".into(), title: "Root atlas".into() }, Source { label: "P2".into(), paper_id: "b".into(), title: "Leaf atlas".into() }];
        let note = note_from_answer(" 方法 是什么？\n", "用了 10x 平台 [P1 p.2 \"Libraries were prepared [12] with Chromium.\"]，结论见 [P1]。\n\n与 [P2, P1 p.3] 一致，另见 [P1、P2] 和文献 [12]。", &sources, "a", "2026-11-11");
        assert_eq!(note, "问：方法 是什么？\n用了 10x 平台 [p.2 \"Libraries were prepared [12] with Chromium.\"]，结论见。\n\n与 [《Leaf atlas》, p.3] 一致，另见 [《Leaf atlas》] 和文献 [12]。\n（2026-11-11 提问所得 · 模型的回答，未核对原文）");
        // Without a question, and with nothing to rewrite.
        assert_eq!(note_from_answer("", "没有标签。", &sources, "a", "2026-11-11"), "没有标签。\n（2026-11-11 提问所得 · 模型的回答，未核对原文）");
    }


    fn paper(id: &str, title: &str, abstract_text: &str) -> Paper {
        let mut p = Paper { id: id.into(), title: title.into(), abstract_text: abstract_text.into(), ..Default::default() };
        p.normalize();
        p
    }

    fn fulltext(id: &str, pages: &[&str]) -> BTreeMap<String, (PaperFile, Vec<String>)> {
        [(id.to_string(), (PaperFile::default(), pages.iter().map(|p| p.to_string()).collect()))].into()
    }

    #[test]
    fn library_context() {
        let a = paper("a", "Root stem cell niche", "WOX5 maintains the quiescent centre.");
        let b = paper("b", "Leaf development", "Unrelated.");
        let fields = [FieldDef { id: "F1".into(), name: "物种".into(), ..Default::default() }];
        let mut a2 = a.clone();
        a2.fields.insert("F1".into(), vec!["Arabidopsis".into()]);
        let text = fulltext("a", &["Introduction about leaves.", "WOX5 expression in the root tip."]);
        let ctx = build_context("根", "根尖发育", &[&a2, &b], &fields, &text, &Scope::Library, "What does WOX5 do in the root?", &[]);
        assert_eq!((ctx.full, ctx.titles_only, ctx.omitted, ctx.pages), (2, 0, 0, 1));
        assert!(ctx.instructions.contains("「根」") && ctx.instructions.contains("根尖发育") && ctx.instructions.contains("2 篇"));
        assert!(ctx.documents.contains("[P1] Root stem cell niche\n"));
        assert!(ctx.documents.contains("物种：Arabidopsis"));
        assert!(ctx.documents.contains("[P1 p.2]\nWOX5 expression"));
        assert!(!ctx.documents.contains("Introduction about leaves"));
        assert_eq!(ctx.sources[1], Source { label: "P2".into(), paper_id: "b".into(), title: "Leaf development".into() });
        // What each paper was handed over as, for the list on the question page.
        assert_eq!(ctx.given, [Given { paper_id: "a".into(), record: "full", pages: 1 }, Given { paper_id: "b".into(), record: "full", pages: 0 }]);
    }

    #[test]
    fn paper_context_reads_everything() {
        let a = paper("a", "Root", "Abs");
        let b = paper("b", "Leaf", "Abs");
        let text = fulltext("b", &["one", "two"]);
        let ctx = build_context("L", "", &[&a, &b], &[], &text, &Scope::Paper("b".into()), "summarise", &[]);
        assert_eq!((ctx.full, ctx.pages, ctx.pages_omitted, ctx.sources.len()), (1, 2, 0, 1));
        assert!(ctx.documents.contains("[P2 p.1]\none") && ctx.documents.contains("[P2 p.2]\ntwo"));
        assert!(!ctx.documents.contains("Root"));
        let none = build_context("L", "", &[&a], &[], &BTreeMap::new(), &Scope::Paper("a".into()), "q", &[]);
        assert!(none.documents.contains("没有可读取的全文"));
    }

    #[test]
    fn budget_is_counted_not_hidden() {
        let big = "x".repeat(150_000);
        let papers: Vec<Paper> = (0..4).map(|i| paper(&format!("p{i}"), &format!("Paper {i}"), &big)).collect();
        let refs: Vec<&Paper> = papers.iter().collect();
        let ctx = build_context("L", "", &refs, &[], &BTreeMap::new(), &Scope::Library, "anything", &[]);
        assert_eq!((ctx.full, ctx.titles_only), (2, 2));
        assert!(ctx.documents.contains("仅列出题目"));
        assert!(ctx.instructions.contains("另有 2 篇只列出题目"));
    }

    #[test]
    fn cited_labels() {
        let cite = |label: &str, page: Option<usize>, quote: &str| Citation { label: label.into(), page, quote: quote.into() };
        assert_eq!(citations("根尖 [P3] 与 [P1 p.5]；另见 [P2, p.7] 和 [P4、P3]，[1] 不是标签。"), [cite("P3", None, ""), cite("P1", Some(5), ""), cite("P2", Some(7), ""), cite("P4", None, "")]);
        // A quoted sentence, with a comma and brackets of its own, in either kind of quotation mark.
        assert_eq!(
            citations("用了 10x 平台 [P1 p.2 \"Libraries were prepared, as described [12], with Chromium.\"]，另见 [P2 p.3 “第二句”] 和 [P1 p.2]。"),
            [cite("P1", Some(2), "Libraries were prepared, as described [12], with Chromium."), cite("P2", Some(3), "第二句"), cite("P1", Some(2), "")]
        );
        // The same quotation twice is one; an unclosed one is no quotation, and what follows is still read.
        assert_eq!(citations("[P1 p.2 \"a b c\"] [P1 p.2 \"a b c\"] [P5 p.1 \"open] [P6]"), [cite("P1", Some(2), "a b c"), cite("P5", Some(1), ""), cite("P6", None, "")]);
        assert_eq!(citations("说了 \"引号\" 的话 [P2]"), [cite("P2", None, "")]);
    }

    #[test]
    fn requests() {
        let ctx = Context { instructions: "I".into(), documents: "D".into(), ..Default::default() };
        let wire = [user_message("hi")];
        let tools = crate::tools::library_catalog();
        let claude = Profile { api_key: "k".into(), ..Default::default() };
        let r = chat_request(&claude, &ctx, &wire, &tools).unwrap();
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(r.body["model"], "claude-opus-5-5");
        assert_eq!(r.body["fallbacks"], "default");
        assert_eq!(r.body["system"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(r.body["cache_control"]["type"], "ephemeral");
        assert_eq!(r.body["messages"], json!([{"role": "user", "content": "hi"}]));
        assert!(r.body.get("thinking").is_none() && r.body.get("temperature").is_none() && r.body.get("tool_choice").is_none());
        assert!(r.headers.iter().any(|(k, v)| k == "anthropic-beta" && v == "server-side-fallback-2026-07-01"));
        let tool = r.body["tools"].as_array().unwrap().iter().find(|t| t["name"] == "search_fulltext").unwrap();
        assert!(tool["input_schema"]["properties"]["query"].is_object() && tool["input_schema"]["properties"].get("library").is_none());
        // A proxy does not get the parameters only Anthropic's own API takes.
        let proxied = chat_request(&Profile { base_url: "https://proxy.example/".into(), ..claude.clone() }, &ctx, &wire, &tools).unwrap();
        assert!(proxied.body.get("fallbacks").is_none() && proxied.body.get("cache_control").is_none());
        assert_eq!(proxied.url, "https://proxy.example/v1/messages");
        assert!(chat_request(&claude, &ctx, &wire, &[]).unwrap().body.get("tools").is_none());

        let other = Profile { protocol: Protocol::OpenAI, base_url: "http://localhost:11434/v1".into(), api_key: "k".into(), model: "qwen".into(), ..Default::default() };
        let r = chat_request(&other, &ctx, &wire, &tools).unwrap();
        assert_eq!(r.url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(r.body["messages"][0]["content"], "I\n\nD");
        assert_eq!(r.body["messages"][1]["role"], "user");
        assert_eq!((r.body["tools"][0]["type"].as_str(), r.body["tools"][0]["function"]["parameters"]["type"].as_str()), (Some("function"), Some("object")));
        assert!(chat_request(&Profile { base_url: "ftp://x".into(), api_key: "k".into(), ..Default::default() }, &ctx, &wire, &tools).is_err());
    }

    #[test]
    fn thinking_per_profile() {
        let body = |protocol: Protocol, base: &str, model: &str, effort: &str| plain_request(&Profile { protocol, base_url: base.into(), api_key: "k".into(), model: model.into(), effort: effort.into(), ..Default::default() }, "s", "p").unwrap().body;
        let claude = |model: &str, effort: &str| body(Protocol::Anthropic, ANTHROPIC_BASE, model, effort);
        // The default sends nothing, and neither does a value from a newer build.
        for effort in ["", "turbo"] {
            let b = claude("claude-opus-5-5", effort);
            assert!(b.get("thinking").is_none() && b.get("output_config").is_none(), "{effort}");
        }
        let b = claude("claude-opus-5-5", "xhigh");
        assert_eq!((&b["thinking"], &b["output_config"]), (&json!({"type": "adaptive"}), &json!({"effort": "xhigh"})));
        // Off, as each model takes it: disabled, between tools, or the lowest effort where thinking cannot stop.
        assert_eq!(claude("claude-haiku-5-5", "off")["thinking"], json!({"type": "disabled"}));
        let b = claude("claude-sonnet-5-5", "off");
        assert!(b["thinking"] == json!({"type": "between_tools"}) && b.get("output_config").is_none());
        for model in ["claude-opus-5-5", "claude-fable-5-2", "anthropic.claude-mythos-5-2"] {
            let b = claude(model, "off");
            assert!(b.get("thinking").is_none() && b["output_config"] == json!({"effort": "low"}), "{model}");
        }

        let chat = |base: &str, effort: &str| body(Protocol::OpenAI, base, "m", effort);
        assert!(chat("https://api.deepseek.com/v1", "").get("reasoning_effort").is_none());
        assert_eq!((&chat("https://api.openai.com/v1", "medium")["reasoning_effort"], &chat("https://api.openai.com/v1", "max")["reasoning_effort"]), (&json!("medium"), &json!("high")));
        assert_eq!(chat("https://api.openai.com/v1", "off")["reasoning_effort"], "none");
        assert_eq!(chat("https://api.deepseek.com/v1", "off")["thinking"], json!({"type": "disabled"}));
        assert_eq!(chat("https://dashscope.aliyuncs.com/compatible-mode/v1", "off")["enable_thinking"], false);
        let responses = |effort: &str| body(Protocol::Responses, "https://api.openai.com/v1", "m", effort);
        assert_eq!((&responses("off")["reasoning"], &responses("xhigh")["reasoning"]), (&json!({"effort": "none"}), &json!({"effort": "high"})));
        assert!(responses("").get("reasoning").is_none());
    }

    #[test]
    fn translation_request() {
        let profile = Profile { api_key: "k".into(), effort: "off".into(), model: "claude-haiku-5-5".into(), ..Default::default() };
        let r = translate_request(&profile, "  ", " Root stem cells. ").unwrap();
        assert_eq!((r.body["system"].as_str(), r.body["messages"][0]["content"].as_str()), (Some(TRANSLATION_PROMPT), Some("Root stem cells.")));
        assert_eq!(r.body["thinking"], json!({"type": "disabled"}));
        assert_eq!(translate_request(&profile, "译成日语。", "x").unwrap().body["system"], "译成日语。");
        // Nothing to translate, and a text that would have to be cut, are both refused.
        assert!(translate_request(&profile, "", "  ").is_err());
        assert!(translate_request(&profile, "", &"a".repeat(TRANSLATE_LIMIT + 1)).is_err());
    }

    #[test]
    fn writing_requests() {
        let profile = Profile { api_key: "k".into(), model: "claude-opus-5-5".into(), ..Default::default() };
        let examples = vec![
            Example { text: "Root growth is limited by soil strength.".into(), cite: "Smith et al., 2020".into(), page: Some(3), heading: "Introduction".into() },
            Example { text: "Ethylene mediates the response.".into(), cite: "Lee, 2019".into(), page: None, heading: String::new() },
        ];
        let paragraph = "  Compacted soil slows roots (Smith et al., 2020).  ";
        let r = assist_request(&profile, &Assist::Rewrite, "English", paragraph, &examples, &[]).unwrap();
        let (system, message) = (r.body["system"].as_str().unwrap(), r.body["messages"][0]["content"].as_str().unwrap());
        // The language, the rule that citations stay, and every candidate with where it is from.
        assert!(system.contains("稿件语言是英文") && system.contains("保持原样"));
        assert!(message.contains("[1] (Smith et al., 2020, p. 3, Introduction) Root growth is limited by soil strength."));
        assert!(message.contains("[2] (Lee, 2019) Ethylene mediates the response."));
        assert!(message.contains("已有的引用一个都不能删") && message.contains("候选句正文里引用的其他文献") && message.contains("不要照抄") && message.ends_with("要改写的段落：\nCompacted soil slows roots (Smith et al., 2020)."));
        // Without candidates there is nothing to rewrite after.
        assert!(assist_request(&profile, &Assist::Rewrite, "English", paragraph, &[], &[]).is_err());
        let many: Vec<Example> = (0..20).map(|n| Example { text: format!("Sentence {n}."), ..Default::default() }).collect();
        let message = assist_prompt(&Assist::Rewrite, "en", "x", &many, &[]).unwrap().1;
        assert!(message.contains("Sentence 11.") && !message.contains("Sentence 12."));

        let (system, message) = assist_prompt(&Assist::Translate, "English", "根系在紧实土壤中生长变慢。", &[], &[]).unwrap();
        assert!(system.contains("英文") && message.contains("译成英文的学术论文文字") && message.ends_with("根系在紧实土壤中生长变慢。"));
        assert_eq!(Assist::Translate.title("German"), "译成german");
        let message = assist_prompt(&Assist::Polish, "", "Roots grows slow.", &[], &[]).unwrap().1;
        assert!(message.contains("改得越少越好") && message.ends_with("Roots grows slow."));

        // The person's own command: the paragraph where {} stands, else at the end.
        let own = vec![Command { name: "style".into(), about: String::new(), template: "按本刊格式检查：{}。".into() }, Command { name: "short".into(), about: String::new(), template: "缩短到两句。".into() }];
        assert_eq!(assist_prompt(&Assist::Own("style".into()), "English", "Roots.", &[], &own).unwrap().1, "按本刊格式检查：Roots.。");
        assert_eq!(assist_prompt(&Assist::Own("short".into()), "English", "Roots.", &[], &own).unwrap().1, "缩短到两句。\n\nRoots.");
        assert!(assist_prompt(&Assist::Own("gone".into()), "English", "Roots.", &[], &own).is_err());
        // Nothing to work on, and too much, are refused before anything is sent.
        assert!(assist_prompt(&Assist::Polish, "English", " \n ", &[], &[]).is_err());
        assert!(assist_prompt(&Assist::Polish, "English", &"a".repeat(ASSIST_LIMIT + 1), &[], &[]).is_err());
    }

    #[test]
    fn paragraph_under_the_caret() {
        let text = "First one.\n\n  Second one.  \nThird.";
        let at = |caret: usize| &text[paragraph_at(text, caret)];
        assert_eq!((at(0), at(5), at(15), at(text.len())), ("First one.", "First one.", "Second one.", "Third."));
        // On the empty line after a paragraph: that paragraph.
        assert_eq!(at(11), "First one.");
        assert!(paragraph_at("\n\n", 1).is_empty());
    }

    #[test]
    fn differences_word_by_word() {
        let (before, after) = word_diff("Roots grows slow in soil.", "Roots grow slowly in soil.").unwrap();
        assert_eq!(before, vec![(Change::Same, "Roots ".into()), (Change::Removed, "grows".into()), (Change::Same, " ".into()), (Change::Removed, "slow".into()), (Change::Same, " in soil.".into())]);
        assert_eq!(after, vec![(Change::Same, "Roots ".into()), (Change::Added, "grow".into()), (Change::Same, " ".into()), (Change::Added, "slowly".into()), (Change::Same, " in soil.".into())]);
        // Each side put together is its text again.
        let joined = |pieces: &[(Change, String)]| pieces.iter().map(|(_, text)| text.as_str()).collect::<String>();
        assert_eq!((joined(&before), joined(&after)), ("Roots grows slow in soil.".to_string(), "Roots grow slowly in soil.".to_string()));
        // A translation shares nothing worth marking.
        assert!(word_diff("根系在紧实土壤中生长变慢。", "Roots grow slowly in compacted soil.").is_none());
    }

    fn read(protocol: Protocol, lines: &[&str]) -> (String, Turn) {
        let mut reader = TurnReader::new(protocol);
        let mut text = String::new();
        for line in lines {
            if let Some(more) = reader.line(line).unwrap() { text.push_str(&more) }
        }
        (text, reader.finish())
    }

    #[test]
    fn anthropic_turn_with_tools() {
        let (text, turn) = read(
            Protocol::Anthropic,
            &[
                "event: content_block_start",
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}}"#,
                r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"先查"}}"#,
                r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"全文。"}}"#,
                r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"search_fulltext","input":{}}}"#,
                r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\": \"chro"}}"#,
                r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"mium\"}"}}"#,
                r#"data: {"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_2","name":"library_status","input":{}}}"#,
                r#"data: {"type":"content_block_stop","index":3}"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
            ],
        );
        assert_eq!(text, "先查全文。");
        assert_eq!(turn.stop, "tool_use");
        assert!(turn.runnable());
        assert_eq!(turn.calls, [ToolCall { id: "toolu_1".into(), name: "search_fulltext".into(), args: Ok(json!({"query": "chromium"})) }, ToolCall { id: "toolu_2".into(), name: "library_status".into(), args: Ok(json!({})) }]);
        // The turn goes back as it came: thinking with its signature, text, and both calls with their input.
        assert_eq!(
            turn.message,
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hm", "signature": "sig=="},
                {"type": "text", "text": "先查全文。"},
                {"type": "tool_use", "id": "toolu_1", "name": "search_fulltext", "input": {"query": "chromium"}},
                {"type": "tool_use", "id": "toolu_2", "name": "library_status", "input": {}},
            ]})
        );
        assert_eq!(
            tool_results(Protocol::Anthropic, &[("toolu_1".into(), "{}".into(), false), ("toolu_2".into(), "坏了".into(), true)]),
            [json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "{}", "is_error": false}, {"type": "tool_result", "tool_use_id": "toolu_2", "content": "坏了", "is_error": true}]})]
        );
    }

    #[test]
    fn anthropic_turn_edges() {
        // A plain answer; an empty text block is not sent back.
        let (text, turn) = read(
            Protocol::Anthropic,
            &[
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello [P1]"}}"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            ],
        );
        assert_eq!((text.as_str(), turn.calls.len(), turn.stop.as_str()), ("Hello [P1]", 0, "end_turn"));
        assert_eq!(turn.message["content"], json!([{"type": "text", "text": "Hello [P1]"}]));

        // Cut off inside a call: the half-written arguments are an error, and nothing may run.
        let (_, cut) = read(
            Protocol::Anthropic,
            &[
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"get_paper","input":{}}}"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"id\": \"P"}}"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
            ],
        );
        assert!(!cut.runnable() && cut.calls[0].args.is_err());
        assert_eq!(cut.message["content"][0]["input"], json!({}));

        // Another model took over: thinking and calls before the fallback block are dropped, the block stays.
        let (_, fell) = read(
            Protocol::Anthropic,
            &[
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"a","signature":"s"}}"#,
                r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"old","name":"get_paper","input":{}}}"#,
                r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"fallback","model":"claude-opus-4-8"}}"#,
                r#"data: {"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"new","name":"library_status","input":{}}}"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            ],
        );
        assert_eq!(fell.calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["new"]);
        assert_eq!(fell.message["content"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect::<Vec<_>>(), ["fallback", "tool_use"]);

        // Nothing at all, and an error inside the stream.
        let (_, nothing) = read(Protocol::Anthropic, &["event: ping", "data: {\"type\":\"ping\"}"]);
        assert_eq!((nothing.message.clone(), nothing.stop.as_str()), (Value::Null, "end"));
        assert_eq!(TurnReader::new(Protocol::Anthropic).line(r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), Err("Overloaded".into()));
    }

    #[test]
    fn openai_turn_with_tools() {
        let (text, turn) = read(
            Protocol::OpenAI,
            &[
                r#"data: {"choices":[{"delta":{"role":"assistant","content":"查一下"},"finish_reason":null}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_fulltext","arguments":""}}]},"finish_reason":null}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"paperId\":"}}]},"finish_reason":null}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"P2\"}"}},{"index":1,"function":{"name":"library_status","arguments":"{}"}}]},"finish_reason":null}]}"#,
                r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                "data: [DONE]",
            ],
        );
        assert_eq!((text.as_str(), turn.stop.as_str()), ("查一下", "tool_calls"));
        // The second call came without an id.
        assert_eq!(turn.calls, [ToolCall { id: "call_a".into(), name: "read_fulltext".into(), args: Ok(json!({"paperId": "P2"})) }, ToolCall { id: "call_1".into(), name: "library_status".into(), args: Ok(json!({})) }]);
        assert_eq!(turn.message["content"], "查一下");
        assert_eq!(turn.message["tool_calls"][0], json!({"id": "call_a", "type": "function", "function": {"name": "read_fulltext", "arguments": "{\"paperId\":\"P2\"}"}}));
        assert_eq!(tool_results(Protocol::OpenAI, &[("call_a".into(), "{}".into(), false), ("call_1".into(), "x".into(), true)]), [json!({"role": "tool", "tool_call_id": "call_a", "content": "{}"}), json!({"role": "tool", "tool_call_id": "call_1", "content": "x"})]);

        let (text, plain) = read(Protocol::OpenAI, &[r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#, r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#, "data: [DONE]"]);
        assert_eq!((text.as_str(), plain.stop.as_str(), plain.message), ("Hi", "stop", json!({"role": "assistant", "content": "Hi"})));
        let (_, bad) = read(Protocol::OpenAI, &[r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"get_paper","arguments":"[1]"}}]},"finish_reason":"length"}]}"#]);
        assert!(bad.calls[0].args.is_err() && !bad.runnable());
        assert_eq!(bad.message["tool_calls"][0]["function"]["arguments"], "{}");
        assert!(stop_note("refusal").is_some() && stop_note("end_turn").is_none() && stop_note("tool_use").is_none());
        assert_eq!(error_message(401, r#"{"error":{"message":"bad key"}}"#), "API Key 无效或没有权限（HTTP 401）：bad key");
    }

    #[test]
    fn responses_turn_with_tools() {
        let profile = Profile { protocol: Protocol::Responses, base_url: "https://api.openai.com/v1/".into(), api_key: "k".into(), model: "gpt-x".into(), ..Default::default() };
        let ctx = Context { instructions: "I".into(), documents: "D".into(), ..Default::default() };
        let mut wire = vec![user_message("哪篇提到 Chromium？")];
        let r = chat_request(&profile, &ctx, &wire, &crate::tools::library_catalog()).unwrap();
        assert_eq!(r.url, "https://api.openai.com/v1/responses");
        assert!(r.headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer k"));
        assert_eq!((r.body["instructions"].as_str(), r.body["store"].as_bool(), r.body["include"][0].as_str(), r.body["stream"].as_bool()), (Some("I\n\nD"), Some(false), Some("reasoning.encrypted_content"), Some(true)));
        assert_eq!(r.body["input"], json!([{"role": "user", "content": "哪篇提到 Chromium？"}]));
        // A function tool is flat here: no `function` wrapper as in chat completions.
        let tool = r.body["tools"].as_array().unwrap().iter().find(|t| t["name"] == "search_fulltext").unwrap();
        assert_eq!((tool["type"].as_str(), tool["parameters"]["type"].as_str(), tool.get("function")), (Some("function"), Some("object"), None));
        assert!(r.body.get("messages").is_none() && chat_request(&profile, &ctx, &wire, &[]).unwrap().body.get("tools").is_none());

        let (text, turn) = read(
            Protocol::Responses,
            &[
                "event: response.created",
                r#"data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}"#,
                r#"data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"gAAA"}}"#,
                r#"data: {"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"先查"}"#,
                r#"data: {"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"全文。"}"#,
                r#"data: {"type":"response.output_item.done","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"先查全文。","annotations":[]}]}}"#,
                r#"data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"query\":"}"#,
                r#"data: {"type":"response.output_item.done","output_index":2,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"search_fulltext","arguments":"{\"query\":\"chromium\"}"}}"#,
                r#"data: {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}"#,
            ],
        );
        assert_eq!((text.as_str(), turn.stop.as_str(), turn.runnable()), ("先查全文。", "tool_calls", true));
        assert_eq!(turn.calls, [ToolCall { id: "call_1".into(), name: "search_fulltext".into(), args: Ok(json!({"query": "chromium"})) }]);
        // Every item goes back as it came, the encrypted reasoning first, then the result by call id.
        let items = turn.wire();
        assert_eq!(items.iter().map(|item| item["type"].as_str().unwrap()).collect::<Vec<_>>(), ["reasoning", "message", "function_call"]);
        assert_eq!(items[0]["encrypted_content"], "gAAA");
        wire.extend(items);
        wire.extend(tool_results(Protocol::Responses, &[("call_1".into(), "{\"total\":1}".into(), false)]));
        let next = chat_request(&profile, &ctx, &wire, &[]).unwrap();
        assert_eq!(next.body["input"].as_array().unwrap().len(), 5);
        assert_eq!(next.body["input"][4], json!({"type": "function_call_output", "call_id": "call_1", "output": "{\"total\":1}"}));

        // A plain answer, a cut-off one, and the two ways a failure is reported.
        let (text, plain) = read(Protocol::Responses, &[r#"data: {"type":"response.output_text.delta","delta":"Hi"}"#, r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hi"}]}}"#, r#"data: {"type":"response.completed","response":{}}"#]);
        assert_eq!((text.as_str(), plain.stop.as_str(), plain.calls.len(), plain.wire().len()), ("Hi", "stop", 0, 1));
        let (_, cut) = read(Protocol::Responses, &[r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c","name":"get_paper","arguments":"{\"id\":"}}"#, r#"data: {"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#]);
        assert!(!cut.runnable() && cut.calls[0].args.is_err() && stop_note(&cut.stop).is_some());
        assert_eq!(TurnReader::new(Protocol::Responses).line(r#"data: {"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}"#), Err("boom".into()));
        assert_eq!(TurnReader::new(Protocol::Responses).line(r#"data: {"type":"error","code":"rate_limit_exceeded","message":"slow down"}"#), Err("slow down".into()));
        let (_, nothing) = read(Protocol::Responses, &[]);
        assert_eq!((nothing.message.clone(), nothing.wire().len(), nothing.stop.as_str()), (Value::Null, 0, "end"));

        let plain = plain_request(&profile, "S", "Q").unwrap();
        assert_eq!((plain.body["instructions"].clone(), plain.body["input"].clone()), (json!("S"), json!([{"role": "user", "content": "Q"}])));
        // One turn of the other protocols is one message.
        assert_eq!(Turn { message: json!({"role": "assistant", "content": "x"}), calls: vec![], stop: "stop".into() }.wire().len(), 1);
    }

    #[test]
    fn labels_for_tools() {
        let a = paper("a", "Root", "");
        let b = paper("b", "Leaf", "");
        let c = paper("c", "Stem", "");
        // The list in view is a and b; the library also holds c.
        let mut ctx = build_context("L", "", &[&a, &b], &[], &BTreeMap::new(), &Scope::Library, "q", &[]);
        ctx.add_sources(&[a.clone(), c.clone()]);
        assert_eq!(ctx.sources.iter().map(|s| (s.label.as_str(), s.paper_id.as_str())).collect::<Vec<_>>(), [("P1", "a"), ("P2", "b"), ("P3", "c")]);
        // One paper keeps the label it has in the whole list; the others follow the highest.
        let mut one = build_context("L", "", &[&a, &b], &[], &BTreeMap::new(), &Scope::Paper("b".into()), "q", &[]);
        one.add_sources(&[a.clone(), b.clone(), c.clone()]);
        assert_eq!(one.sources.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["P2", "P3", "P4"]);

        let mut args = json!({"paperId": "p3", "page": 2});
        resolve_labels(&mut args, &ctx.sources);
        assert_eq!(args, json!({"paperId": "c", "page": 2}));
        let mut unknown = json!({"id": "P9"});
        resolve_labels(&mut unknown, &ctx.sources);
        assert_eq!(unknown, json!({"id": "P9"}));
        let mut result = json!({"total": 1, "hits": [{"paperId": "c", "page": 1}], "papers": [{"id": "a"}, {"id": "zzz"}]});
        add_labels(&mut result, &ctx.sources);
        assert_eq!(result, json!({"total": 1, "hits": [{"paperId": "c", "page": 1, "label": "P3"}], "papers": [{"id": "a", "label": "P1"}, {"id": "zzz"}]}));
        assert!(ctx.instructions.contains("search_fulltext") && ctx.instructions.contains("label"));
    }

    #[test]
    fn query_ideas() {
        let (system, prompt) = query_prompt("根", "根尖发育", "单细胞 干细胞微环境", &["pubmed".into(), "arxiv".into()]);
        assert!(system.contains("检索"));
        assert!(prompt.contains("主题：根尖发育") && prompt.contains("单细胞 干细胞微环境") && prompt.contains("- pubmed：PubMed 检索式") && prompt.contains("- arxiv：") && !prompt.contains("- crossref"));
        // No topic, no line for it.
        assert!(!query_prompt("根", " ", "x", &[]).1.contains("主题："));

        let reply = "好的：\n```json\n[{\"source\": \"PubMed\", \"query\": \" root[Title] AND \\\"single cell\\\" \", \"why\": \"聚焦\"},\n {\"source\": \"scholar\", \"query\": \"x\"}, {\"source\": \"arxiv\", \"query\": \"\"}, {\"source\": \"arxiv\", \"query\": \"abs:[root]\"}]\n```";
        assert_eq!(
            parse_queries(reply).unwrap(),
            [QueryIdea { source: "pubmed".into(), query: "root[Title] AND \"single cell\"".into(), why: "聚焦".into() }, QueryIdea { source: "arxiv".into(), query: "abs:[root]".into(), why: String::new() }]
        );
        for bad in ["我不知道", "[]", "] [", "[{\"source\": \"pubmed\"}]"] {
            assert!(parse_queries(bad).is_err(), "{bad}");
        }

        let claude = Profile { api_key: "k".into(), ..Default::default() };
        let r = plain_request(&claude, "S", "Q").unwrap();
        assert_eq!((r.body["system"].clone(), r.body["messages"].clone()), (json!("S"), json!([{"role": "user", "content": "Q"}])));
        assert!(r.body.get("cache_control").is_none() && r.body.get("tools").is_none() && r.body["fallbacks"] == "default");
        let other = plain_request(&Profile { protocol: Protocol::OpenAI, base_url: "http://x/v1".into(), api_key: "k".into(), model: "m".into(), ..Default::default() }, "S", "Q").unwrap();
        assert_eq!(other.body["messages"], json!([{"role": "system", "content": "S"}, {"role": "user", "content": "Q"}]));
        assert!(plain_request(&Profile::default(), "S", "Q").is_err() || !Profile::default().key().is_empty());
    }

    #[test]
    fn screening_judgements() {
        let p = paper("a", "Root stem cell niche", "WOX5 maintains the quiescent centre of the Arabidopsis root.");
        let pages = vec!["Introduction.".to_string(), "Libraries were prepared with\n10x Genomics Chromium.".to_string()];
        let (system, prompt, basis) = screen_prompt("根尖发育", "只收拟南芥根的单细胞研究", &p, Some(&pages));
        assert!(system.contains("related") && system.contains("逐字"));
        assert!(prompt.starts_with("筛选标准：只收拟南芥根的单细胞研究\n文献库的主题：根尖发育\n") && prompt.contains("题目：Root stem cell niche") && prompt.contains("[p.2]\nLibraries were prepared"));
        assert_eq!(basis, "fulltext");
        assert_eq!(screen_prompt("t", "", &p, None).2, "abstract");
        assert_eq!(screen_prompt("t", "", &p, Some(&[" ".to_string()])).2, "abstract");
        assert_eq!(screen_prompt("t", "", &paper("b", "Only a title", ""), None).2, "none");

        // The quotation is found across the line break and case of the page, and its page kept.
        let judged = parse_judgement("```json\n{\"verdict\": \"Include\", \"reason\": \" 用了 10x 平台 \", \"excerpt\": \"prepared with 10X genomics chromium\", \"confidence\": 1.4}\n```", &p, Some(&pages)).unwrap();
        assert_eq!(judged, Judgement { verdict: "included".into(), reason: "用了 10x 平台".into(), excerpt: "prepared with 10X genomics chromium".into(), page: Some(2), confidence: Some(1.) });
        // `related` is stored under the key the window shows as 「相关」.
        let related = parse_judgement(r#"{"verdict": "related", "excerpt": "WOX5 maintains the quiescent centre"}"#, &p, None).unwrap();
        assert_eq!((related.verdict.as_str(), related.excerpt.as_str(), related.page, related.confidence), ("excluded", "WOX5 maintains the quiescent centre", None, None));
        // A quotation that is not in the text is not evidence.
        let invented = parse_judgement(r#"{"verdict": "irrelevant", "reason": "r", "excerpt": "This paper studies leaves only."}"#, &p, Some(&pages)).unwrap();
        assert_eq!((invented.verdict.as_str(), invented.excerpt.as_str()), ("irrelevant", ""));
        assert_eq!(locate_quote("WOX5", &p, None), None);
        for bad in ["不知道", r#"{"verdict": "maybe"}"#, "} {"] {
            assert!(parse_judgement(bad, &p, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn slash_commands() {
        assert_eq!(read_input("  WOX5 做什么？ ", &[]).unwrap(), Input::Ask("WOX5 做什么？".into()));
        let search = expand_slash("/search 10x Genomics");
        assert!(search.starts_with("在文献库里检索：10x Genomics\n") && search.contains("search_fulltext"));
        let cite = expand_slash("/cite 根尖有干细胞。\n它们由 WOX5 维持。");
        assert!(cite.contains("根尖有干细胞。\n它们由 WOX5 维持。") && cite.contains("find_citations"));
        assert!(read_input("/search", &[]).unwrap_err().contains("/search 后面写上内容"));
        assert!(read_input("/dance now", &[]).unwrap_err().contains("/cite"));
        assert_eq!(read_input("/new", &[]).unwrap(), Input::NewChat);
        // A message that cannot be expanded is replayed as it was typed.
        assert_eq!(expand_slash("/dance now"), "/dance now");
        // Every command that stands for a request says what to write after it.
        assert!(SLASH.iter().all(|command| command.template.is_empty() == command.hint.is_empty() && command.template.contains("{}") != command.template.is_empty()));
        // Expanding /review preserves the topic and asks for both decisions before drafting.
        let review = expand_slash("/review 根系如何感知土壤紧实");
        assert!(review.contains("根系如何感知土壤紧实") && review.contains("等待用户确认或调整") && review.contains("等待用户明确确认"));
    }

    #[test]
    fn own_commands() {
        let command = |name: &str, about: &str, template: &str| Command { name: name.into(), about: about.into(), template: template.into() };
        let own = [command("style-check", "找出空泛的表达", "指出下面文字里的套话：\n{}\n保留 {} 里的数值。"), command("润色", "", "把上一个回答改得更简洁。"), command("search", "mine", "x"), command("empty", "", "  ")];
        // The text after the name goes where the template says, as it is: braces in it are not read again.
        assert_eq!(read_input("/style-check a {} b\n第二行", &own).unwrap(), Input::Ask("指出下面文字里的套话：\na {} b\n第二行\n保留 a {} b\n第二行 里的数值。".into()));
        assert!(read_input("/style-check  ", &own).unwrap_err().contains("/style-check 后面写上内容"));
        // A template without a place for it stands alone, and takes what follows at its end.
        assert_eq!(read_input("/润色", &own).unwrap(), Input::Ask("把上一个回答改得更简洁。".into()));
        assert_eq!(read_input("/润色 用英文", &own).unwrap(), Input::Ask("把上一个回答改得更简洁。\n\n用英文".into()));
        assert!(read_input("/empty", &own).unwrap_err().contains("还没有写模板"));
        // A built-in command is never replaced by one of the same name.
        assert!(matches!(read_input("/search x", &own), Ok(Input::Ask(prompt)) if prompt.starts_with("在文献库里检索：x")));
        assert!(read_input("/dance", &own).unwrap_err().contains("/style-check（找出空泛的表达）、/润色"));
        // A conversation from before the request was kept does not guess at a template.
        assert_eq!(expand_slash("/style-check 一段话"), "/style-check 一段话");

        let names = |input: &str| slash_matches(input, &own).into_iter().map(|offer| offer.name).collect::<Vec<_>>();
        assert_eq!(names("/"), ["search", "cite", "review", "compare", "write", "verify", "new", "style-check", "润色", "empty"]);
        assert_eq!(names("/s"), ["search", "style-check"]);
        assert_eq!(names("/润"), ["润色"]);
        assert_eq!(names("/空泛"), ["style-check"]);
        let offers = slash_matches("/", &own);
        assert!(offers.iter().all(|offer| offer.custom == !SLASH.iter().any(|command| command.name == offer.name) && offer.runs == (offer.name == "new")));

        let problem = |name: &str| command_name_problem(name, own.iter().map(|command| command.name.as_str()));
        for good in ["methods-check", "检查方法", "a_1"] { assert_eq!(problem(good), None, "{good}") }
        for bad in ["", "a b", "a/b", "{}", "Style", "new", "cite", "润色", "style-check", &"a".repeat(33)] { assert!(problem(bad).is_some(), "{bad}") }
    }

    #[test]
    fn slash_popup() {
        let names = |input: &str| slash_matches(input, &[]).into_iter().map(|command| command.name).collect::<Vec<_>>();
        assert_eq!(names("/"), ["search", "cite", "review", "compare", "write", "verify", "new"]);
        // A command that can stand alone works on what it says it does.
        assert!(matches!(read_input("/verify", &[]), Ok(Input::Ask(prompt)) if prompt.contains("逐条核对下面这段内容的依据：上一个回答")));
        assert!(read_input("/cite", &[]).is_err());
        assert_eq!(names("/c"), ["cite", "compare"]);
        assert_eq!(names("/RE"), ["review"]);
        // Found by what it does, after those found by name.
        assert_eq!(names("/配"), ["cite"]);
        assert_eq!(names("/引"), ["cite", "review"]);
        assert_eq!(names("/综述"), ["review"]);
        // Not a command being typed: no slash, something after the name, or no such command.
        assert!(names("search").is_empty() && names("/search ").is_empty() && names("/search x").is_empty() && names("/zz").is_empty() && names(" /s").is_empty());
    }

    #[test]
    fn pictures_in_a_question() {
        assert_eq!((base64(b""), base64(b"f"), base64(b"fo"), base64(b"foo"), base64(b"foob")), (String::new(), "Zg==".into(), "Zm8=".into(), "Zm9v".into(), "Zm9vYg==".into()));
        let image = ImageData { media_type: "image/png".into(), data: "AAAA".into() };
        assert_eq!(user_message_with(Protocol::Anthropic, "这是什么？", &[]), user_message("这是什么？"));
        assert_eq!(
            user_message_with(Protocol::Anthropic, "这是什么？", std::slice::from_ref(&image)),
            json!({"role": "user", "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}, {"type": "text", "text": "这是什么？"}]})
        );
        assert_eq!(
            user_message_with(Protocol::OpenAI, "这是什么？", std::slice::from_ref(&image)),
            json!({"role": "user", "content": [{"type": "text", "text": "这是什么？"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]})
        );
        // A picture alone: no empty text block, which Claude would refuse.
        assert_eq!(user_message_with(Protocol::Responses, " ", &[image]), json!({"role": "user", "content": [{"type": "input_image", "image_url": "data:image/png;base64,AAAA"}]}));
    }

    #[test]
    fn a_note_with_the_results() {
        let results = [("toolu_1".to_string(), "{}".to_string(), false)];
        let claude = with_note(Protocol::Anthropic, tool_results(Protocol::Anthropic, &results), "快到头了");
        assert_eq!(claude, [json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "{}", "is_error": false}, {"type": "text", "text": "快到头了"}]})]);
        let openai = with_note(Protocol::OpenAI, tool_results(Protocol::OpenAI, &results), "快到头了");
        assert_eq!(openai[1], json!({"role": "user", "content": "快到头了"}));
        assert_eq!(with_note(Protocol::Responses, tool_results(Protocol::Responses, &results), "快到头了").len(), 2);
    }

    #[test]
    fn labels_survive_a_reopened_conversation() {
        let (a, b, c) = (paper("a", "Alpha", ""), paper("b", "Beta", ""), paper("c", "Gamma", ""));
        // The earlier conversation knew b as P1 and a paper since removed from the list as P4.
        let known = [Source { label: "P1".into(), paper_id: "b".into(), title: "Beta".into() }, Source { label: "P4".into(), paper_id: "gone".into(), title: "Gone".into() }];
        let ctx = build_context("L", "", &[&a, &b, &c], &[], &BTreeMap::new(), &Scope::Library, "q", &known);
        let label = |id: &str| ctx.sources.iter().find(|s| s.paper_id == id).map(|s| s.label.as_str());
        assert_eq!((label("a"), label("b"), label("c"), label("gone")), (Some("P5"), Some("P1"), Some("P6"), Some("P4")));
        assert!(ctx.documents.contains("[P1] Beta") && ctx.documents.contains("[P5] Alpha"));
        // A question about one paper keeps the others' labels as well.
        let one = build_context("L", "", &[&a, &b, &c], &[], &BTreeMap::new(), &Scope::Paper("a".into()), "q", &known);
        assert_eq!(one.sources.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["P5", "P1", "P4"]);
    }

    #[test]
    fn transcripts() {
        let transcript = Transcript {
            items: vec![
                Said::User { text: "这张图\n说明了什么？".into(), images: vec!["I-1".into()], prompt: String::new() },
                Said::Tool { call: "检索全文：WOX5".into(), result: "3 条结果".into(), failed: false },
                Said::Answer { text: "见 [P1]。".into() },
            ],
            sources: vec![Source { label: "P1".into(), paper_id: "a".into(), title: "Alpha".into() }],
        };
        let text = serde_json::to_string(&transcript).unwrap();
        assert!(text.contains(r#""kind":"user""#) && text.contains(r#""paperId":"a""#));
        assert_eq!(serde_json::from_str::<Transcript>(&text).unwrap(), transcript);
        assert_eq!((transcript.title(), transcript.questions()), ("这张图 说明了什么？".to_string(), 1));
        // Kept by an older build, or damaged: what is missing is empty, not an error.
        assert_eq!(serde_json::from_str::<Transcript>("{}").unwrap(), Transcript::default());
        let pictures = Transcript { items: vec![Said::User { text: String::new(), images: vec!["I-1".into()], prompt: String::new() }], sources: vec![] };
        assert_eq!((pictures.title(), Transcript::default().title()), ("（图片）".to_string(), "未命名对话".to_string()));
    }
}
