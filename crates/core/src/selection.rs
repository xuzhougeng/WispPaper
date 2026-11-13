//! What a click selects in a text being written: a word (a double click),
//! a sentence (⌘ or Ctrl with a click, as in Word) or a paragraph (a triple
//! click). All three take a Markdown text and a byte in it and give a range
//! of bytes; a paragraph is a line, as everywhere in the manuscript.

use crate::{citefind, markdown};
use icu_segmenter::WordSegmenter;
use icu_segmenter::options::WordBreakInvariantOptions;
use std::ops::Range;

/// How far a click reaches, and a drag that follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Word,
    Sentence,
    Paragraph,
}

/// The `unit` around byte `at`.
pub fn around(text: &str, at: usize, unit: Unit) -> Range<usize> {
    match unit {
        Unit::Word => word(text, at),
        Unit::Sentence => sentence(text, at),
        Unit::Paragraph => paragraph(text, at),
    }
}

/// The line `at` is on, without its line break.
fn line(text: &str, at: usize) -> Range<usize> {
    text[..at].rfind('\n').map_or(0, |i| i + 1)..text[at..].find('\n').map_or(text.len(), |i| at + i)
}

fn floor(text: &str, at: usize) -> usize {
    text.floor_char_boundary(at.min(text.len()))
}

/// The word at `at`. Words are cut by Unicode's rules with ICU's
/// dictionaries, so a run of Chinese or Japanese comes apart into its
/// words and not into single characters. A citation `[@A; @B]` is one
/// word, since it is drawn as one. On a space or a mark: that run.
pub fn word(text: &str, at: usize) -> Range<usize> {
    let at = floor(text, at);
    let line = line(text, at);
    if line.is_empty() { return at..at }
    if let Some(cited) = citation(text, &line, at) { return cited }
    let words = &text[line.clone()];
    let rel = at - line.start;
    // Each piece between two breaks, and whether it is a word.
    let mut pieces: Vec<(Range<usize>, bool)> = vec![];
    let mut breaks = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default()).segment_str(words);
    let mut from = breaks.next().unwrap_or(0);
    while let Some(to) = breaks.next() {
        pieces.push((from..to, breaks.is_word_like()));
        from = to;
    }
    let Some(n) = pieces.iter().position(|(range, _)| range.start <= rel && rel < range.end).or(pieces.len().checked_sub(1)) else { return at..at };
    // Just past a word's last letter, where a click on its right half lands: that word.
    let n = if !pieces[n].1 && pieces[n].0.start == rel && n > 0 && pieces[n - 1].1 { n - 1 } else { n };
    let (mut range, wordy) = pieces[n].clone();
    // A term the dictionary does not know comes apart into single characters
    // (拟南芥 as 拟 南 芥): those next to each other are taken together, up to a word like 在 or 的.
    let single = |piece: &(Range<usize>, bool)| piece.1 && {
        let mut chars = words[piece.0.clone()].chars();
        chars.next().is_some_and(|c| han(c) && !JOINING.contains(c)) && chars.next().is_none()
    };
    if single(&pieces[n]) {
        range.start = pieces[..n].iter().rev().take_while(|piece| single(piece)).last().map_or(range.start, |piece| piece.0.start);
        range.end = pieces[n + 1..].iter().take_while(|piece| single(piece)).last().map_or(range.end, |piece| piece.0.end);
    }
    // A run of spaces or marks is taken together.
    if !wordy {
        let blank = words[range.clone()].trim().is_empty();
        let alike = |piece: &(Range<usize>, bool)| !piece.1 && words[piece.0.clone()].trim().is_empty() == blank;
        range.start = pieces[..n].iter().rev().take_while(|piece| alike(piece)).last().map_or(range.start, |piece| piece.0.start);
        range.end = pieces[n + 1..].iter().take_while(|piece| alike(piece)).last().map_or(range.end, |piece| piece.0.end);
    }
    line.start + range.start..line.start + range.end
}

/// Characters that stand as words of their own between terms: prepositions, particles, conjunctions.
const JOINING: &str = "的地得了着过在和与及或是被对从将把为而等也都就则且并其之以于由向这那有不很更最所各每该此即又再仍可能会已";

fn han(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}' | '\u{20000}'..='\u{2FA1F}')
}

/// The Pandoc citation `[@…]` that byte `at` stands in, if any.
fn citation(text: &str, line: &Range<usize>, at: usize) -> Option<Range<usize>> {
    let words = &text[line.clone()];
    let rel = at - line.start;
    let open = words.match_indices("[@").map(|(i, _)| i).take_while(|i| *i <= rel).last()?;
    let close = open + words[open..].find(']')?;
    (close >= rel && !words[open + 1..close].contains('[')).then(|| line.start + open..line.start + close + 1)
}

/// What a paragraph says: its line without the Markdown it opens with (a
/// quote, a list's marker, a heading's hashes) and without the spaces
/// around it. An empty line gives an empty range where the click was.
pub fn paragraph(text: &str, at: usize) -> Range<usize> {
    let at = floor(text, at);
    let line = line(text, at);
    let raw = &text[line.clone()];
    let mut start = markdown::continued(raw).map_or(0, |c| c.marker);
    let rest = &raw[start..];
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && rest[hashes..].starts_with([' ', '\t']) { start += hashes }
    let said = &raw[start..];
    let start = line.start + start + (said.len() - said.trim_start().len());
    let end = start + said.trim().len();
    if end > start { start..end } else { at..at }
}

/// The sentence at `at`, within its paragraph: cut where `citefind` cuts
/// (full stops of either script, not after an abbreviation or inside
/// brackets), except that a semicolon goes on with its sentence. Between
/// two sentences: the one that follows.
pub fn sentence(text: &str, at: usize) -> Range<usize> {
    let at = floor(text, at);
    let para = paragraph(text, at);
    if para.is_empty() { return para }
    let all: Vec<Range<usize>> = citefind::whole_sentences(&text[para.clone()]).into_iter().map(|range| para.start + range.start..para.start + range.end).collect();
    all.iter().find(|range| range.start <= at && at < range.end).or_else(|| all.iter().find(|range| range.start > at)).or(all.last()).cloned().unwrap_or(para)
}

/// The selection a drag makes that began on `first` (a unit) and is now at
/// `at`: from the first unit to the one under the pointer, whole. Whether
/// the pointer is before the first unit, so the selection runs backwards.
pub fn extend(text: &str, first: &Range<usize>, at: usize, unit: Unit) -> (Range<usize>, bool) {
    let here = around(text, at, unit);
    let here = if here.is_empty() { at..at } else { here };
    if here.start < first.start { (here.start..first.end, true) } else { (first.start..here.end.max(first.end), false) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(text: &str, at: &str, unit: Unit) -> String {
        let at = text.find(at).unwrap();
        text[around(text, at, unit)].to_string()
    }

    #[test]
    fn words_of_either_script() {
        let text = "Phosphate starvation induces root hair growth.";
        assert_eq!(pick(text, "starvation", Unit::Word), "starvation");
        assert_eq!(pick(text, "arvation", Unit::Word), "starvation");
        // Right after a word: that word, not the space.
        let after = text.find(" induces").unwrap();
        assert_eq!(&text[word(text, after)], "starvation");
        assert_eq!(&text[word(text, text.len())], ".");
        let zh = "我们发现拟南芥在低磷条件下根毛变长。";
        assert_eq!(pick(zh, "南芥", Unit::Word), "拟南芥");
        assert_eq!(pick(zh, "发现", Unit::Word), "发现");
        assert_eq!(pick(zh, "磷", Unit::Word), "低磷");
        assert_eq!(pick(zh, "在", Unit::Word), "在");
        let zh = "磷酸盐缺乏诱导根毛伸长，突变体表型明显。";
        assert_eq!(pick(zh, "根毛", Unit::Word), "根毛");
        assert_eq!(pick("can't stop", "an't", Unit::Word), "can't");
    }

    #[test]
    fn spaces_and_citations() {
        let text = "root   hairs [@Zhang2024; @Li2020] grow.";
        assert_eq!(pick(text, "  hairs", Unit::Word), "   ");
        assert_eq!(pick(text, "Li2020", Unit::Word), "[@Zhang2024; @Li2020]");
        assert_eq!(pick(text, "[@", Unit::Word), "[@Zhang2024; @Li2020]");
        assert_eq!(pick("", "", Unit::Word), "");
        assert_eq!(word("a\n\nb", 2), 2..2);
    }

    #[test]
    fn sentences_keep_abbreviations_and_semicolons() {
        let text = "Roots grow (Fig. 2). Hairs, e.g. long ones, grow too; leaves do not. 根毛变长。叶片不变！";
        assert_eq!(pick(text, "grow (Fig", Unit::Sentence), "Roots grow (Fig. 2).");
        assert_eq!(pick(text, "long ones", Unit::Sentence), "Hairs, e.g. long ones, grow too; leaves do not.");
        assert_eq!(pick(text, "leaves", Unit::Sentence), "Hairs, e.g. long ones, grow too; leaves do not.");
        assert_eq!(pick(text, "变长", Unit::Sentence), "根毛变长。");
        assert_eq!(pick(text, "叶片", Unit::Sentence), "叶片不变！");
        // On the space between two: the one that follows.
        let gap = text.find(" Hairs").unwrap();
        assert_eq!(&text[sentence(text, gap)], "Hairs, e.g. long ones, grow too; leaves do not.");
        let cited = "Roots (Fig. 2). Hairs grow; leaves do not [@Zhang2024]. Mutants stay short.";
        assert_eq!(pick(cited, "leaves", Unit::Sentence), "Hairs grow; leaves do not [@Zhang2024].");
        // Not past the paragraph.
        assert_eq!(pick("One. Two\nThree.", "Two", Unit::Sentence), "Two");
    }

    #[test]
    fn paragraphs_without_their_markdown() {
        let text = "# Results\n\n- Roots grow. Hairs too.\n> 1. quoted item\n  plain line  \n";
        assert_eq!(pick(text, "Results", Unit::Paragraph), "Results");
        assert_eq!(pick(text, "Hairs", Unit::Paragraph), "Roots grow. Hairs too.");
        assert_eq!(pick(text, "quoted", Unit::Paragraph), "quoted item");
        assert_eq!(pick(text, "plain", Unit::Paragraph), "plain line");
        assert_eq!(paragraph(text, 10), 10..10);
        assert_eq!(pick(text, "Roots", Unit::Sentence), "Roots grow.");
    }

    #[test]
    fn a_drag_takes_whole_units() {
        let text = "one two three four";
        let first = word(text, text.find("two").unwrap());
        assert_eq!(extend(text, &first, text.find("hree").unwrap(), Unit::Word), (4..13, false));
        assert_eq!(extend(text, &first, 1, Unit::Word), (0..7, true));
        assert_eq!(extend(text, &first, 5, Unit::Word), (4..7, false));
    }
}
