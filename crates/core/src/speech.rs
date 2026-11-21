//! 朗读: a paper's text read aloud by DeepSeek's speech service
//! (`POST {host}/v1/tools/tts`). What is decided here: which text is read
//! (a PDF page's lines joined again, citation numbers and addresses left
//! out, the reference list not read), how it is cut into requests, the
//! request itself, and the sound that comes back (16-bit mono PCM at 24 kHz)
//! as samples. Sending is `wisp_net::speak`, playing the window's.

use crate::ai::HttpRequest;
use serde_json::json;

/// What the service sends with `format: pcm`: 16-bit little-endian mono at this rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// The voices the service has, by `voice_id`, with what the settings call them. The first is its default.
pub const VOICES: [(&str, &str); 2] = [("stella", "女声"), ("tide", "男声")];

/// What one request carries at most, in the service's count (`billed`). The service takes 10,000;
/// a smaller piece brings the first sound sooner and wastes little when the reader stops.
pub const PIECE: usize = 1_500;

/// The voice to ask with: the one chosen when the service has it, else its default.
pub fn voice(chosen: &str) -> &'static str {
    VOICES.iter().find(|(key, _)| *key == chosen.trim()).map_or(VOICES[0].0, |(key, _)| key)
}

/// Whether a model service's address is DeepSeek's, the one that speaks.
pub fn is_service(base_url: &str) -> bool {
    host(base_url).ends_with("deepseek.com")
}

fn host(base_url: &str) -> String {
    base_url.split("://").nth(1).unwrap_or(base_url).split(['/', ':', '?']).next().unwrap_or("").trim().to_lowercase()
}

/// Where the sound is asked for: the speech service sits at the host's root, whatever path chat has (`/v1`, `/anthropic`).
pub fn endpoint(base_url: &str) -> String {
    let base = base_url.trim();
    let scheme = if base.starts_with("http://") { "http" } else { "https" };
    let rest = base.split("://").nth(1).unwrap_or(base);
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    format!("{scheme}://{authority}/v1/tools/tts")
}

/// The request for one piece of text, in `voice` (a key of `VOICES`).
pub fn request(base_url: &str, key: &str, proxy: &str, voice_id: &str, text: &str) -> Result<HttpRequest, String> {
    if key.trim().is_empty() { return Err("朗读要用 DeepSeek 的 API Key：请在「设置 › AI」里填写。".into()) }
    if text.trim().is_empty() { return Err("没有可朗读的文字。".into()) }
    Ok(HttpRequest {
        url: endpoint(base_url),
        headers: vec![("authorization".into(), format!("Bearer {}", key.trim())), ("content-type".into(), "application/json".into())],
        // The text is already plain: Markdown filtering would only take its asterisks and brackets.
        body: json!({"text": text, "voice_id": voice(voice_id), "format": "pcm", "filter_markdown": false}),
        proxy: proxy.trim().to_string(),
    })
}

/// A character the service counts twice: Chinese, Japanese and Korean script and full-width forms.
fn wide(c: char) -> bool {
    matches!(c, '\u{2E80}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}' | '\u{20000}'..='\u{2FA1F}')
}

/// What a text counts for in a request: a Chinese character two, anything else one.
pub fn billed(text: &str) -> usize {
    text.chars().map(|c| if wide(c) { 2 } else { 1 }).sum()
}

/// The heading a reference list begins under, on a line of its own.
fn references_heading(line: &str) -> bool {
    let line = line.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c.is_whitespace()).trim_end_matches([':', '：']).trim();
    ["references", "bibliography", "literature cited", "references and notes", "参考文献", "引用文献"].iter().any(|h| line.eq_ignore_ascii_case(h))
}

/// A page's text as it is read: its lines joined again (a word hyphenated at a line's end made whole,
/// Chinese joined without a space), lines that are only a page number left out, and the bracketed
/// citation numbers (`[12]`, `[3–5, 8]`) and web addresses that only interrupt the listener taken out.
pub fn readable(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.chars().all(|c| c.is_ascii_digit())) {
        let last = out.chars().last();
        let first = line.chars().next();
        match (last, first) {
            (None, _) => {}
            // `exam-` + `ple`: one word again; a hyphen before a capital or a digit stays (`Wnt-`, `COVID-19`).
            (Some('-'), Some(next)) if next.is_lowercase() && out.chars().rev().nth(1).is_some_and(char::is_alphabetic) => { out.pop(); }
            (Some('-'), _) if out.chars().rev().nth(1).is_some_and(char::is_alphanumeric) => {}
            (Some(before), Some(next)) if wide(before) || wide(next) => {}
            _ => out.push(' '),
        }
        out.push_str(line);
    }
    let out = without_citations(&out);
    let out: Vec<&str> = out.split_whitespace().filter(|word| !(word.starts_with("http://") || word.starts_with("https://") || word.starts_with("www.") || word.to_ascii_lowercase().starts_with("doi:"))).collect();
    // A citation taken from before a full stop leaves a space there: `cells [3].` → `cells .`
    out.join(" ").replace(" .", ".").replace(" ,", ",").replace(" ;", ";")
}

/// The text without bracketed citation numbers: `[12]`, `[3, 5]`, `[3–5]`, `［1］`.
fn without_citations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(['[', '［']) {
        let open_len = rest[open..].chars().next().map_or(1, char::len_utf8);
        let inside = &rest[open + open_len..];
        let close = inside.find([']', '］']);
        let cited = close.is_some_and(|close| {
            let inner = &inside[..close];
            !inner.trim().is_empty() && inner.chars().any(|c| c.is_ascii_digit()) && inner.chars().all(|c| c.is_ascii_digit() || matches!(c, ',' | '，' | '-' | '–' | '—' | ' '))
        });
        out.push_str(&rest[..open]);
        match close {
            Some(close) if cited => {
                let close_len = inside[close..].chars().next().map_or(1, char::len_utf8);
                rest = &inside[close + close_len..];
            }
            _ => {
                out.push_str(&rest[open..open + open_len]);
                rest = inside;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Where a sentence may end: after these, and for the Latin ones only before a space.
fn sentence_end(text: &str, at: usize, c: char) -> bool {
    match c {
        '。' | '！' | '？' | '；' => true,
        '.' | '!' | '?' | ';' => text[at + c.len_utf8()..].chars().next().is_none_or(char::is_whitespace),
        _ => false,
    }
}

/// The text cut into pieces of at most `budget` (`billed`), at the ends of sentences where it can be,
/// else at a comma or a space, else anywhere.
pub fn pieces(text: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(16);
    let mut sentences = vec![];
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if sentence_end(text, at, c) {
            let end = at + c.len_utf8();
            sentences.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() { sentences.push(&text[start..]) }
    let mut out: Vec<String> = vec![];
    let mut current = String::new();
    for sentence in sentences.into_iter().flat_map(|s| cut(s, budget)) {
        if !current.is_empty() && billed(&current) + billed(&sentence) > budget {
            out.push(current.trim().to_string());
            current.clear();
        }
        current.push_str(&sentence);
    }
    if !current.trim().is_empty() { out.push(current.trim().to_string()) }
    out.retain(|piece| !piece.is_empty());
    out
}

/// A sentence too long for one piece, cut at commas or spaces, else anywhere.
fn cut(sentence: &str, budget: usize) -> Vec<String> {
    if billed(sentence) <= budget { return vec![sentence.to_string()] }
    let mut out = vec![];
    let mut current = String::new();
    for c in sentence.chars() {
        current.push(c);
        let soft = matches!(c, ',' | '，' | '、' | ':' | '：') || c.is_whitespace();
        if billed(&current) >= budget || (soft && billed(&current) >= budget * 3 / 4) {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() { out.push(current) }
    out
}

/// What is read from page `first` (1-based) on: each page's readable text in pieces, with its page.
/// Reading stops at the reference list.
pub fn from_pages(pages: &[String], first: usize) -> Vec<(usize, String)> {
    let mut out = vec![];
    for (index, page) in pages.iter().enumerate().skip(first.max(1) - 1) {
        let lines: Vec<&str> = page.lines().collect();
        let end = lines.iter().position(|line| references_heading(line));
        let text = readable(&lines[..end.unwrap_or(lines.len())].join("\n"));
        out.extend(pieces(&text, PIECE).into_iter().map(|piece| (index + 1, piece)));
        if end.is_some() { break }
    }
    out
}

/// The whole readable text of the pages, up to the reference list: what a podcast is written from.
pub fn body(pages: &[String]) -> String {
    let mut out: Vec<String> = vec![];
    for page in pages {
        let lines: Vec<&str> = page.lines().collect();
        let end = lines.iter().position(|line| references_heading(line));
        let text = readable(&lines[..end.unwrap_or(lines.len())].join("\n"));
        if !text.is_empty() { out.push(text) }
        if end.is_some() { break }
    }
    out.join(" ")
}

/// The sound as it arrives, bytes to samples: a sample whose two bytes come in two reads is kept whole.
#[derive(Default)]
pub struct Pcm {
    odd: Option<u8>,
}

impl Pcm {
    /// The samples in `bytes` (and the byte left over from the last time), in -1 to 1.
    pub fn samples(&mut self, bytes: &[u8]) -> Vec<f32> {
        let mut all = Vec::with_capacity(bytes.len() + 1);
        all.extend(self.odd.take());
        all.extend_from_slice(bytes);
        if all.len() % 2 == 1 { self.odd = all.pop() }
        all.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.).collect()
    }
}

/// How long `samples` sound.
pub fn seconds(samples: usize) -> f32 {
    samples as f32 / SAMPLE_RATE as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_at_the_hosts_root() {
        assert_eq!(endpoint("https://api.deepseek.com"), "https://api.deepseek.com/v1/tools/tts");
        assert_eq!(endpoint("https://api.deepseek.com/v1/"), "https://api.deepseek.com/v1/tools/tts");
        assert_eq!(endpoint("https://api.deepseek.com/anthropic"), "https://api.deepseek.com/v1/tools/tts");
        assert_eq!(endpoint("http://127.0.0.1:8080/v1"), "http://127.0.0.1:8080/v1/tools/tts");
        assert!(is_service("https://api.deepseek.com/v1") && !is_service("https://api.moonshot.cn/v1"));
    }

    #[test]
    fn a_request_asks_for_pcm_in_a_known_voice() {
        let asked = request("https://api.deepseek.com", " sk-1 ", "", "tide", "Hello.").unwrap();
        assert_eq!(asked.body, json!({"text": "Hello.", "voice_id": "tide", "format": "pcm", "filter_markdown": false}));
        assert!(asked.headers.contains(&("authorization".into(), "Bearer sk-1".into())));
        assert_eq!(request("https://api.deepseek.com", "k", "", "nobody", "x").unwrap().body["voice_id"], "stella");
        assert!(request("https://api.deepseek.com", "", "", "", "x").unwrap_err().contains("API Key"));
        assert!(request("https://api.deepseek.com", "k", "", "", "  ").is_err());
    }

    #[test]
    fn chinese_counts_twice() {
        assert_eq!(billed("ab 1"), 4);
        assert_eq!(billed("细胞分裂"), 8);
        assert_eq!(billed("细胞，cell"), 10);
    }

    #[test]
    fn lines_are_joined_as_they_are_read() {
        let page = "The exam-\nple shows that cells [3]\ndivide [12, 14–16].\n7\nSee https://x.org/a for COVID-\n19 data.\n细胞\n分裂［1］很快。";
        assert_eq!(readable(page), "The example shows that cells divide. See for COVID-19 data.细胞分裂很快。");
        // Brackets with words in them are part of the text.
        assert_eq!(readable("the [Ca2+] level and [see above]"), "the [Ca2+] level and [see above]");
        assert_eq!(readable("an unclosed [3 bracket"), "an unclosed [3 bracket");
    }

    #[test]
    fn pieces_end_with_sentences_and_keep_to_the_budget() {
        let text = "One two three. Four five six. Seven eight nine. e.g. ten";
        assert_eq!(pieces(text, 30), ["One two three. Four five six.", "Seven eight nine. e.g. ten"]);
        let chinese = "细胞分裂。".repeat(10);
        let cut = pieces(&chinese, 20);
        assert!(cut.iter().all(|piece| billed(piece) <= 20 && piece.ends_with('。')), "{cut:?}");
        assert_eq!(cut.concat(), chinese);
        // A sentence longer than a piece is cut too, and nothing of it is lost.
        let long = "word ".repeat(100);
        let cut = pieces(&long, 64);
        assert!(cut.iter().all(|piece| billed(piece) <= 64));
        assert_eq!(cut.join(" ").split_whitespace().count(), 100);
        assert!(pieces("  ", 64).is_empty());
    }

    #[test]
    fn reading_starts_at_a_page_and_stops_at_the_references() {
        let pages = vec!["Abstract.\nFirst page.".to_string(), "Second page.".to_string(), "Last words.\n\nReferences\n1. Smith J. A paper.".to_string(), "More references.".to_string()];
        assert_eq!(from_pages(&pages, 2), [(2, "Second page.".to_string()), (3, "Last words.".to_string())]);
        assert_eq!(from_pages(&pages, 0)[0], (1, "Abstract. First page.".to_string()));
        assert!(references_heading("  5. References: ") && references_heading("参考文献") && !references_heading("References are given below."));
        assert!(from_pages(&pages, 9).is_empty());
    }

    #[test]
    fn samples_split_across_reads_stay_whole() {
        let mut pcm = Pcm::default();
        let bytes: Vec<u8> = [0i16, 16384, -32768].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut got = pcm.samples(&bytes[..3]);
        got.extend(pcm.samples(&bytes[3..]));
        assert_eq!(got, [0., 0.5, -1.]);
        assert_eq!(seconds(48_000), 2.);
    }
}
