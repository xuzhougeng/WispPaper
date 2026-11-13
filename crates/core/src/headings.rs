//! Headings told by how a paragraph looks, for a Word file whose headings
//! carry no heading style (typed in bold, or not set apart at all). A
//! heading there is short, does not end like a sentence, is numbered
//! (`一、`, `（一）`, `1.`, `1.1`, `第二章`, `II.`), ends with the figure it
//! shows (`（图1）`, `(Fig. 2)`), is set in bold, or stands alone between
//! longer paragraphs. These are only guesses: the window shows them, the
//! person ticks the ones that start a section (`manuscript::from_word_with`),
//! and nothing is cut without that.

use crate::structure::{self, canonical, width};

/// What makes a paragraph look like a heading, the strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    /// It names a part every paper has: 摘要, Introduction, 参考文献.
    Named,
    Numbered,
    /// It ends with the figure it shows: `（图1）`.
    Figure,
    Bold,
    /// Short, between two longer paragraphs.
    Alone,
}

/// A paragraph taken for a heading.
#[derive(Clone, Debug, PartialEq)]
pub struct Guess {
    /// Its place among the file's paragraphs.
    pub paragraph: usize,
    pub text: String,
    /// 1 for the file's highest headings, 2 for those under them, …
    pub level: u8,
    pub sign: Sign,
    /// The part it names (a key of `structure::SECTIONS` or `structure::SKIP`).
    pub kind: Option<&'static str>,
    /// Ticked before the person looks: a heading of the highest level, or one that names a part.
    pub likely: bool,
}

/// How a heading is numbered. The order is the usual order of levels: a
/// file numbered both `一、` and `1.` has the `1.` headings under the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Numbering {
    /// `第一章`, `第2部分`.
    Chapter,
    /// `一、`, `二．`.
    Chinese,
    /// `I.`, `IV.`.
    Roman,
    /// `第一节`.
    Part,
    /// `（一）`.
    ChineseParen,
    /// `1`, `1.`, `1.2`, `1.2.3`: how many numbers.
    Arabic(u8),
    /// `（1）`, `1)`.
    Paren,
}

/// No heading is wider than this (`structure::width`: about 40 Chinese characters).
pub const LONGEST: usize = 120;

/// A heading with no other sign than standing alone is no wider than this.
const ALONE_WIDTH: usize = 60;

/// A paragraph of running text is at least this wide.
const BODY_WIDTH: usize = 60;

fn chinese_numeral(c: char) -> bool {
    "一二三四五六七八九十百零〇两".contains(c)
}

/// The numbering a paragraph opens with, and the words after it.
pub fn numbering(text: &str) -> Option<(Numbering, &str)> {
    let text = text.trim_start();
    // The words after the number: they start with a letter.
    fn words(rest: &str) -> Option<&str> {
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '、' | '．' | '.' | ':' | '：' | '-' | '–' | '—'));
        rest.chars().next().filter(|c| c.is_alphabetic() || structure::is_cjk(*c) || matches!(*c, '“' | '"' | '‘' | '《')).map(|_| rest)
    }
    // 第一章, 第2节
    if let Some(rest) = text.strip_prefix('第') {
        let n = rest.chars().take_while(|c| chinese_numeral(*c) || c.is_ascii_digit()).map(char::len_utf8).sum::<usize>();
        if n > 0 {
            let after = &rest[n..];
            for (mark, kind) in [("章", Numbering::Chapter), ("部分", Numbering::Chapter), ("节", Numbering::Part), ("節", Numbering::Part)] {
                if let Some(rest) = after.strip_prefix(mark) { return words(rest).map(|rest| (kind, rest)) }
            }
        }
        return None;
    }
    // （一） （1） (1)
    for open in ['（', '('] {
        let Some(rest) = text.strip_prefix(open) else { continue };
        let close = rest.find(['）', ')'])?;
        let inner = &rest[..close];
        let after = &rest[close + rest[close..].chars().next().map_or(1, char::len_utf8)..];
        let kind = if !inner.is_empty() && inner.chars().all(chinese_numeral) && inner.chars().count() <= 3 {
            Numbering::ChineseParen
        } else if !inner.is_empty() && inner.len() <= 2 && inner.chars().all(|c| c.is_ascii_digit()) {
            Numbering::Paren
        } else {
            return None;
        };
        return words(after).map(|rest| (kind, rest));
    }
    // 一、
    let n: usize = text.chars().take_while(|c| chinese_numeral(*c)).map(char::len_utf8).sum();
    if n > 0 && text[..n].chars().count() <= 3 {
        let after = &text[n..];
        return if after.starts_with(['、', '．', '.']) { words(after).map(|rest| (Numbering::Chinese, rest)) } else { None };
    }
    // II. Results
    let n = text.chars().take_while(|c| matches!(c, 'I' | 'V' | 'X')).count();
    if (1..=4).contains(&n) {
        let after = &text[n..];
        if after.starts_with(". ") || after.starts_with('、') || after.starts_with("．") { return words(after).map(|rest| (Numbering::Roman, rest)) }
        return None;
    }
    // 1  1.  1.2  1.2.3  1)
    let mut at = 0;
    let mut depth = 0u8;
    loop {
        let digits = text[at..].chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 2 { break }
        at += digits;
        depth += 1;
        match text[at..].chars().next() {
            Some(c @ ('.' | '．')) if text[at + c.len_utf8()..].starts_with(|c: char| c.is_ascii_digit()) => at += c.len_utf8(),
            _ => break,
        }
    }
    if depth == 0 || text[..at].ends_with(['.', '．']) { return None }
    let after = &text[at..];
    if let Some(rest) = after.strip_prefix([')', '）']) { return (depth == 1).then(|| words(rest).map(|rest| (Numbering::Paren, rest))).flatten() }
    // After the number something that sets it apart: a space, a mark, a dot; never a letter run on (`2021年`, `3D`).
    let apart = after.starts_with(|c: char| c.is_whitespace() || matches!(c, '、' | '．' | '.' | ':' | '：'));
    if !apart { return None }
    words(after).map(|rest| (Numbering::Arabic(depth), rest))
}

/// Where a figure named at a paragraph's end starts: `（图1）`, `(Fig. 2A, B)`, `（表 1）`.
pub fn figure_at_end(text: &str) -> Option<usize> {
    let text = text.trim_end();
    let close = text.chars().last().filter(|c| matches!(c, ')' | '）'))?;
    let open = text.rfind(['(', '（'])?;
    let inner = text[open..text.len() - close.len_utf8()].trim_start_matches(['(', '（']).trim();
    let lower = inner.to_lowercase();
    let names = ["图", "表", "附图", "fig", "figure", "table", "supplementary", "extended data"];
    let named = names.iter().any(|name| lower.starts_with(name));
    (named && inner.chars().any(|c| c.is_ascii_digit()) && width(inner) <= 36 && open > 0).then_some(open)
}

/// What a heading says once its number and the figure at its end are left out.
fn words_of(text: &str) -> &str {
    let text = text.trim();
    let text = figure_at_end(text).map_or(text, |at| text[..at].trim_end());
    numbering(text).map_or(text, |(_, rest)| rest.trim())
}

fn ends_a_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['。', '．', '.', '!', '?', '！', '？', '；', ';', '，', ',', '：', ':', '、', '…'])
}

/// The look of a paragraph that may be a heading, or none for one that cannot be.
#[derive(Clone, Copy, Debug)]
struct Look {
    numbering: Option<Numbering>,
    figure: bool,
    named: Option<&'static str>,
    width: usize,
}

fn look(text: &str) -> Option<Look> {
    let text = text.trim();
    let w = width(text);
    // A part every paper has, told by its name as before: `摘要`, `Abstract:`, `参考文献`.
    let named = (w <= 60).then(|| canonical(text).or_else(|| canonical(words_of(text)))).flatten();
    if named.is_some() { return Some(Look { numbering: numbering(text).map(|(n, _)| n), figure: false, named, width: w }) }
    if w > LONGEST || ends_a_sentence(text) { return None }
    let figure = figure_at_end(text).is_some();
    let number = numbering(text).map(|(n, _)| n);
    let words = words_of(text);
    if words.chars().filter(|c| c.is_alphabetic()).count() < 2 { return None }
    // A legend, a reference, an address, a line of keywords.
    if crate::figure::opening(text).is_some() || text.starts_with('[') || text.contains('@') || text.contains("http") { return None }
    if number.is_none() && (words.contains([':', '：']) || words.matches([',', '，']).count() >= 2) { return None }
    // Two sentences in one.
    if words.contains(['。', '！', '？', '；']) || words.contains("; ") { return None }
    if words.split(". ").skip(1).any(|after| after.starts_with(|c: char| c.is_uppercase())) { return None }
    // A heading in Latin letters starts with a capital or a number: "Root hairs", not "and then".
    if number.is_none() && words.starts_with(|c: char| c.is_lowercase()) { return None }
    Some(Look { numbering: number, figure, named: None, width: w })
}

/// Running text: long, and ends as a sentence does (or with what is cited after it).
fn is_body(text: &str, than: usize) -> bool {
    let w = width(text.trim());
    let end = text.trim_end().trim_end_matches([')', '）', ']', '】', '"', '”', '’', '\'']).trim_end();
    w >= BODY_WIDTH.max(than * 2) && (ends_a_sentence(text) || end.ends_with(|c: char| c.is_ascii_digit()) || ends_a_sentence(end))
}

/// References follow this heading: what comes after it is a list, not text with headings.
fn opens_references(text: &str) -> bool {
    let lower = words_of(text).to_lowercase();
    ["参考文献", "參考文獻", "引用文献", "references", "reference", "literature cited", "bibliography", "literatur", "références", "referencias", "referências", "참고문헌"].iter().any(|name| lower == *name)
}

/// The headings a file without heading styles seems to have. `paragraphs`
/// are its paragraphs, each with whether it is set in bold; `title` the
/// one that is its title, which is never a heading.
pub fn guess(paragraphs: &[(&str, bool)], title: Option<usize>) -> Vec<Guess> {
    let looks: Vec<Option<Look>> = paragraphs.iter().enumerate().map(|(n, (text, _))| if Some(n) == title { None } else { look(text) }).collect();
    let mut found: Vec<(usize, Sign, Look)> = vec![];
    let mut references = false;
    for (n, (text, bold)) in paragraphs.iter().enumerate() {
        let Some(this) = looks[n] else { continue };
        if let Some(kind) = this.named {
            found.push((n, Sign::Named, this));
            references = kind == structure::SKIP && opens_references(text);
            continue;
        }
        // In the references every line is short and numbered.
        if references { continue }
        let (before, after) = (n.checked_sub(1).map(|b| (b, paragraphs[b].0)), paragraphs.get(n + 1).map(|(text, _)| *text));
        let next_look = looks.get(n + 1).copied().flatten();
        // A numbered line next to another numbered alike is an item of a list.
        let same = |other: Option<Look>| this.numbering.is_some() && other.is_some_and(|other| other.numbering == this.numbering && other.named.is_none());
        if same(next_look) || same(before.and_then(|(b, _)| looks[b])) { continue }
        // What follows a heading is longer than it, or a heading under it.
        let under = |next: Look| next.named.is_some() || match (this.numbering, next.numbering) {
            (Some(mine), Some(theirs)) => theirs > mine,
            _ => false,
        };
        let followed = match after {
            Some(next) => (width(next.trim()) > this.width + this.width / 2 && !next_look.is_some_and(|next| next.numbering.is_some() && next.numbering == this.numbering)) || next_look.is_some_and(under),
            None => false,
        };
        if !followed { continue }
        let sign = if this.numbering.is_some() {
            Sign::Numbered
        } else if this.figure {
            Sign::Figure
        } else if *bold {
            Sign::Bold
        } else {
            // Nothing but where it stands: short, after running text or another heading, before running text.
            let after_text = before.is_some_and(|(b, text)| (Some(b) != title && is_body(text, this.width)) || found.last().is_some_and(|(last, _, _)| *last == b));
            if this.width > ALONE_WIDTH || !after_text || !after.is_some_and(|next| is_body(next, this.width)) { continue }
            Sign::Alone
        };
        found.push((n, sign, this));
    }
    // Levels: the numbering of each kind in its usual order; a heading without a number is of the highest level, or under the numbered ones when it names nothing.
    let mut kinds: Vec<Numbering> = found.iter().filter_map(|(_, _, look)| look.numbering).collect();
    kinds.sort();
    kinds.dedup();
    found
        .into_iter()
        .map(|(n, sign, look)| {
            let level = match look.numbering {
                Some(kind) => kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8 + 1,
                None if look.named.is_some() || kinds.is_empty() => 1,
                None => 2,
            };
            Guess { paragraph: n, text: paragraphs[n].0.trim().to_string(), level, sign, kind: look.named, likely: sign == Sign::Named || level == 1 }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(paragraphs: &[&str], title: Option<usize>) -> Vec<(String, u8, Sign, bool)> {
        let with: Vec<(&str, bool)> = paragraphs.iter().map(|text| (*text, false)).collect();
        guess(&with, title).into_iter().map(|g| (g.text, g.level, g.sign, g.likely)).collect()
    }

    #[test]
    fn numbers_are_read_in_their_kinds() {
        let kind = |text: &str| numbering(text).map(|(kind, rest)| (kind, rest.to_string()));
        assert_eq!(kind("一、单细胞测序的原理"), Some((Numbering::Chinese, "单细胞测序的原理".into())));
        assert_eq!(kind("（二）根的发育"), Some((Numbering::ChineseParen, "根的发育".into())));
        assert_eq!(kind("第三章 讨论"), Some((Numbering::Chapter, "讨论".into())));
        assert_eq!(kind("1 引言"), Some((Numbering::Arabic(1), "引言".into())));
        assert_eq!(kind("1. Introduction"), Some((Numbering::Arabic(1), "Introduction".into())));
        assert_eq!(kind("2.1 Plant material"), Some((Numbering::Arabic(2), "Plant material".into())));
        assert_eq!(kind("2.1.3　细胞分离"), Some((Numbering::Arabic(3), "细胞分离".into())));
        assert_eq!(kind("(1) first"), Some((Numbering::Paren, "first".into())));
        assert_eq!(kind("II. Results"), Some((Numbering::Roman, "Results".into())));
        for not in ["2021年以来，单细胞技术", "3D imaging", "10x Genomics", "Introduction", "In 2020 we", "一些研究表明", "1.", "（图1）"] {
            assert_eq!(numbering(not), None, "{not}");
        }
        assert_eq!(figure_at_end("单细胞转录组测序技术的发展（图1）"), Some("单细胞转录组测序技术的发展".len()));
        assert!(figure_at_end("Root development (Fig. 2A, B)").is_some());
        assert!(figure_at_end("Roots (and shoots)").is_none() && figure_at_end("（图1）").is_none());
    }

    #[test]
    fn a_chinese_review_without_heading_styles() {
        let paragraphs = [
            "单细胞转录组测序技术在植物发育生物学上的应用及进展",
            "张三，李四，王五",
            "摘要：单细胞转录组测序技术能够在单个细胞水平上解析基因表达，为研究植物发育提供了新的视角。本文综述了该技术的发展及其在植物中的应用。",
            "关键词：单细胞；转录组；植物发育",
            "单细胞转录组测序技术的发展（图1）",
            "自2009年首次报道以来，单细胞转录组测序技术经历了快速的发展，通量不断提高，成本不断下降，已经成为研究细胞异质性的重要工具。",
            "图1 单细胞转录组测序技术的发展历程",
            "早期的方法依赖于手工挑取细胞，通量很低；基于微流控和液滴的方法使一次实验可以测定数万个细胞。",
            "原生质体的制备",
            "植物细胞有细胞壁，需要先酶解制备原生质体，这一步骤可能改变部分基因的表达，需要在分析时加以考虑。",
            "在根发育研究中的应用（图2）",
            "拟南芥根尖是单细胞转录组研究最多的器官，已经构建了包含各种细胞类型的高分辨率图谱。",
            "展望",
            "随着空间转录组等技术的发展，单细胞水平的研究将更加深入。",
            "参考文献",
            "Zhang Y, Li H. Single-cell RNA-seq in plants",
            "Wang X, Chen Z. A root atlas",
        ];
        assert_eq!(
            shown(&paragraphs, Some(0)),
            [
                ("单细胞转录组测序技术的发展（图1）".to_string(), 1, Sign::Figure, true),
                ("原生质体的制备".to_string(), 1, Sign::Alone, true),
                ("在根发育研究中的应用（图2）".to_string(), 1, Sign::Figure, true),
                ("展望".to_string(), 1, Sign::Named, true),
                ("参考文献".to_string(), 1, Sign::Named, true),
            ]
        );
    }

    #[test]
    fn a_chinese_paper_numbered_in_two_levels() {
        let paragraphs = [
            "水稻根系发育的单细胞图谱",
            "摘要",
            "我们构建了水稻根尖的单细胞转录组图谱，鉴定了十二种细胞类型，并发现了新的发育轨迹。",
            "一、引言",
            "根系是植物吸收水分和养分的器官，其发育受到多种因素的调控，近年来单细胞技术为此提供了新手段。",
            "二、材料与方法",
            "（一）植物材料",
            "选用日本晴水稻，在人工气候室中培养七天后取根尖用于实验，每个样品取三个生物学重复。",
            "（二）原生质体制备",
            "根尖用纤维素酶和离析酶混合液处理两小时，过滤后离心收集原生质体，并检测其活力。",
            "实验中使用的主要试剂如下：",
            "（1）纤维素酶",
            "（2）离析酶",
            "三、结果",
            "共获得一万二千个高质量细胞，聚类得到十二个细胞群，各细胞群的标记基因与已知细胞类型一致。",
        ];
        assert_eq!(
            shown(&paragraphs, Some(0)),
            [
                ("摘要".to_string(), 1, Sign::Named, true),
                ("一、引言".to_string(), 1, Sign::Named, true),
                ("二、材料与方法".to_string(), 1, Sign::Named, true),
                ("（一）植物材料".to_string(), 2, Sign::Numbered, false),
                ("（二）原生质体制备".to_string(), 2, Sign::Numbered, false),
                ("三、结果".to_string(), 1, Sign::Named, true),
            ]
        );
    }

    #[test]
    fn an_english_paper_without_heading_styles() {
        let paragraphs = [
            "Single-cell transcriptomics of plant development",
            "Jane Smith, Wei Zhang, Ana Lopez",
            "Abstract",
            "Single-cell RNA sequencing has changed how plant development is studied. Here we review what it has shown about roots, leaves and flowers.",
            "1. Introduction",
            "Plant organs are made of many cell types, each with its own pattern of gene expression, which bulk sequencing averages away.",
            "1.1 Why single cells",
            "A single cell is the unit at which fate is decided, so measuring cells one by one shows the decisions as they happen.",
            "2. Root development (Fig. 2)",
            "The Arabidopsis root tip has been profiled most, and atlases now resolve every cell type along its developmental trajectory.",
            "Figure 2. A single-cell atlas of the root tip",
            "Trajectories reconstructed from these atlases agree with what lineage tracing showed decades ago.",
            "Protoplasting artefacts",
            "Cell walls must be digested before cells can be captured, which induces stress genes that have to be removed from the analysis.",
            "We note three limits:",
            "1) cost",
            "2) the loss of position",
            "Conclusions",
            "Single-cell methods will become routine in plant biology as costs fall.",
            "References",
            "Smith J (2020) Root atlas. Nature 1: 2-3",
            "Lopez A (2021) Leaf cells",
        ];
        assert_eq!(
            shown(&paragraphs, Some(0)),
            [
                ("Abstract".to_string(), 1, Sign::Named, true),
                ("1. Introduction".to_string(), 1, Sign::Named, true),
                ("1.1 Why single cells".to_string(), 2, Sign::Numbered, false),
                ("2. Root development (Fig. 2)".to_string(), 1, Sign::Numbered, true),
                ("Protoplasting artefacts".to_string(), 2, Sign::Alone, false),
                ("Conclusions".to_string(), 1, Sign::Named, true),
                ("References".to_string(), 1, Sign::Named, true),
            ]
        );
    }

    #[test]
    fn bold_short_lines_are_headings_where_nothing_else_says() {
        let paragraphs = [("Notes on roots", false), ("Roots grow down into the soil and take up water, which the plant needs.", false), ("Root hairs", true), ("Hairs widen the surface the root takes water through; they live a few days.", false), ("A short line", false), ("Not body text either", false)];
        let found: Vec<(String, Sign)> = guess(&paragraphs, Some(0)).into_iter().map(|g| (g.text, g.sign)).collect();
        assert_eq!(found, [("Root hairs".to_string(), Sign::Bold)]);
    }
}
