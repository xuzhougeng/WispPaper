//! 播客: a paper talked over by two hosts, a woman and a man, in DeepSeek's two voices. A model writes
//! the conversation from the paper's text (`request`), each line marked with who says it; `script`
//! reads that back into turns, each spoken in its host's voice (`speech::request`). What is decided
//! here: what the model is given and told, how its script is read, and the WAV file the sound is kept in.

use crate::ai::{self, HttpRequest, Profile};
use crate::model::Paper;
use crate::speech;

/// The two hosts: what a line of the script begins with, the host's name, and the voice (`speech::VOICES`).
pub const HOSTS: [(&str, &str, &str); 2] = [("女", "小星", "stella"), ("男", "阿潮", "tide")];

/// How much of the paper the model is given, in characters: a long paper's body, short of its tail.
const SOURCE: usize = 60_000;

/// The silence between two turns, in samples: a breath, so one host does not run into the other.
pub const PAUSE: usize = speech::SAMPLE_RATE as usize * 35 / 100;

/// What the model is told. `{language}` is the language the hosts speak.
pub const PROMPT: &str = "你是一档学术播客的编剧。把用户给的论文写成两位主持人的对话稿：女主持「小星」，男主持「阿潮」。\n\
要求：\n\
- 用{language}对话，像两个懂行的朋友聊天，口语、自然，有来有回：一个人提问、追问、打比方，另一个人解释；不要一人讲很长一段。\n\
- 开头一两句寒暄并点出论文的题目和它要回答的问题；然后依次聊研究背景、用了什么方法（讲清楚，不必讲细节）、主要发现（带上关键数字）、局限和还没解决的问题、这项研究为什么重要；最后简短收尾。\n\
- 只讲论文里写了的内容，不要编造数据、结论或作者没说过的话；拿不准的地方就说论文没有讲。\n\
- 写给耳朵听的：不要 Markdown、不要列表、不要括号里的舞台说明、不要引用编号和网址；公式和符号用话说出来；专业术语第一次出现时顺口解释一下。\n\
- 全稿 30 到 50 轮，每轮一到三句话，总长约 3000 到 5000 字。\n\
输出格式：每一行是一轮发言，女主持的以「女：」开头，男主持的以「男：」开头，行与行之间不要空行，除了对话稿不要输出任何别的文字。";

/// The instructions for a podcast in `language` (`ai::language_name`'s words: 中文, 英文).
pub fn instructions(language: &str) -> String {
    PROMPT.replace("{language}", language)
}

/// What the model is given: the paper's details and as much of its body as fits, up to the references.
pub fn source(paper: &Paper, pages: &[String]) -> String {
    let mut out = format!("题目：{}\n", paper.title.trim());
    if !paper.authors.trim().is_empty() { out.push_str(&format!("作者：{}\n", paper.authors.trim())) }
    let place = [paper.journal.trim().to_string(), paper.year.map(|y| y.to_string()).unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("，");
    if !place.is_empty() { out.push_str(&format!("发表：{place}\n")) }
    if !paper.abstract_text.trim().is_empty() { out.push_str(&format!("\n摘要：\n{}\n", paper.abstract_text.trim())) }
    let body = speech::body(pages);
    if !body.is_empty() {
        out.push_str("\n正文：\n");
        let cut: String = body.chars().take(SOURCE).collect();
        out.push_str(&cut);
        if cut.len() < body.len() { out.push_str("\n（正文到此截断）") }
    }
    out
}

/// The request for the script of a paper's podcast.
pub fn request(profile: &Profile, language: &str, paper: &Paper, pages: &[String]) -> Result<HttpRequest, String> {
    if paper.title.trim().is_empty() && paper.abstract_text.trim().is_empty() && pages.iter().all(|page| page.trim().is_empty()) { return Err("这篇文献没有可以讲的文字：没有摘要，也读不出全文。".into()) }
    ai::plain_request(profile, &instructions(language), &source(paper, pages))
}

/// One turn of the conversation: which host says it (an index into `HOSTS`) and what is said.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub host: usize,
    pub text: String,
}

/// Which host a line is marked for, and the rest of the line: `女：`, `**男**:`, `小星：`, `- 阿潮:`.
fn marked(line: &str) -> Option<(usize, &str)> {
    let line = line.trim().trim_start_matches(['-', '*', '•', '>', ' ']).trim_start();
    for (host, (mark, name, _)) in HOSTS.iter().enumerate() {
        for label in [*mark, *name, &format!("{mark}主持"), &format!("{name}（{mark}）"), &format!("{mark}（{name}）")] {
            let Some(rest) = line.strip_prefix(label) else { continue };
            let rest = rest.trim_start_matches(['*', ' ']);
            if let Some(said) = rest.strip_prefix('：').or_else(|| rest.strip_prefix(':')) { return Some((host, said.trim())) }
        }
    }
    None
}

/// What a line says as it is heard: emphasis marks and stage directions in brackets taken out.
fn spoken(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '（' | '(' if text.contains(['）', ')']) => depth += 1,
            '）' | ')' if depth > 0 => depth -= 1,
            '*' | '#' | '`' => {}
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The model's script read back into turns as it streams in. A line marked for no host goes on the turn
/// before it; text before the first marked line (a title, a word to the reader) is left out, as are
/// empty turns. A turn is whole once the next one begins, or the script ends.
#[derive(Default)]
pub struct Reader {
    /// The line still coming in.
    partial: String,
    current: Option<Turn>,
}

impl Reader {
    /// More of the script; the turns it made whole.
    pub fn push(&mut self, text: &str) -> Vec<Turn> {
        self.partial.push_str(text);
        let mut whole = vec![];
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            whole.extend(self.line(&line));
        }
        whole
    }

    /// The script ended: the last turn.
    pub fn finish(&mut self) -> Vec<Turn> {
        let rest = std::mem::take(&mut self.partial);
        let mut whole: Vec<Turn> = self.line(&rest).into_iter().collect();
        whole.extend(self.current.take().and_then(Self::said));
        whole
    }

    fn line(&mut self, line: &str) -> Option<Turn> {
        let line = line.trim();
        if line.is_empty() { return None }
        match marked(line) {
            Some((host, said)) => self.current.replace(Turn { host, text: said.to_string() }).and_then(Self::said),
            None => {
                if let Some(current) = self.current.as_mut() {
                    if !current.text.is_empty() { current.text.push(' ') }
                    current.text.push_str(line);
                }
                None
            }
        }
    }

    fn said(turn: Turn) -> Option<Turn> {
        let text = spoken(&turn.text);
        (!text.is_empty()).then_some(Turn { text, ..turn })
    }
}

/// What a script says when no line of it is marked for a host: that was no script.
pub const NO_SCRIPT: &str = "模型写的稿子里没有「女：」「男：」开头的对话。";

/// A whole script read into turns (`Reader`); errs when no line is marked for a host.
pub fn script(text: &str) -> Result<Vec<Turn>, String> {
    let mut reader = Reader::default();
    let mut turns = reader.push(text);
    turns.extend(reader.finish());
    if turns.is_empty() { return Err(NO_SCRIPT.into()) }
    Ok(turns)
}

/// The script as it is kept beside the sound: one turn a line, under the host's name.
pub fn transcript(title: &str, turns: &[Turn]) -> String {
    let mut out = format!("# 播客：{}\n\n", title.trim());
    for turn in turns { out.push_str(&format!("**{}**：{}\n\n", HOSTS[turn.host].1, turn.text)) }
    out
}

/// What is asked of the speech service for a turn: its host's voice, and the turn cut into pieces it takes.
pub fn pieces(turn: &Turn) -> (&'static str, Vec<String>) {
    (HOSTS[turn.host].2, speech::pieces(&turn.text, speech::PIECE))
}

/// A WAV file of 16-bit mono PCM at the service's rate, around `pcm` (its little-endian bytes).
pub fn wav(pcm: &[u8]) -> Vec<u8> {
    let data = pcm.len() as u32 & !1;
    let rate = speech::SAMPLE_RATE;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(&pcm[..data as usize]);
    out
}

/// A name for the kept files: the title, short and without what a file system refuses.
pub fn file_name(title: &str) -> String {
    let title: String = title.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c }).collect();
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
    if title.is_empty() { "播客".into() } else { format!("播客 - {}", title.trim_end_matches(['.', ' '])) }
}

/// Where each turn of a kept podcast stands in its sound of `samples`, as the sample it begins and ends at:
/// the sound keeps no marks, so the turns share it by their length (`speech::billed`) after the `PAUSE`
/// between speakers. Near enough to say the turn heard and show its words while it plays again.
pub fn timings(turns: &[Turn], samples: usize) -> Vec<(usize, usize)> {
    let pauses = turns.windows(2).filter(|pair| pair[0].host != pair[1].host).count() * PAUSE;
    let said: usize = turns.iter().map(|turn| speech::billed(&turn.text)).sum();
    let rate = samples.saturating_sub(pauses) as f64 / said.max(1) as f64;
    let mut at = 0.;
    let mut out = Vec::with_capacity(turns.len());
    for (index, turn) in turns.iter().enumerate() {
        if index > 0 && turns[index - 1].host != turn.host { at += PAUSE as f64 }
        let end = at + speech::billed(&turn.text) as f64 * rate;
        out.push((at as usize, (end as usize).min(samples)));
        at = end;
    }
    out
}

/// The PCM bytes of a WAV file `wav` made: what follows its `data` header.
pub fn pcm_of(wav: &[u8]) -> Option<&[u8]> {
    let at = wav.windows(4).position(|w| w == b"data")?;
    wav.get(at + 8..)
}

/// Whether an attachment is a kept podcast's (named by `file_name`), and which half: `Some(true)` the
/// sound, `Some(false)` the script.
pub fn kept(title: &str, format: &str) -> Option<bool> {
    if !title.starts_with("播客") { return None }
    match format { "wav" => Some(true), "md" => Some(false), _ => None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_read_into_turns() {
        let text = "播客：细胞分裂\n\n女：大家好，欢迎收听。\n**男**: 今天聊一篇论文（笑），\n关于细胞分裂。\n小星：它讲了什么？\n- 阿潮：讲了*很多*。\n男：\n女主持：再见。";
        let turns = script(text).unwrap();
        assert_eq!(turns, [
            Turn { host: 0, text: "大家好，欢迎收听。".into() },
            Turn { host: 1, text: "今天聊一篇论文， 关于细胞分裂。".into() },
            Turn { host: 0, text: "它讲了什么？".into() },
            Turn { host: 1, text: "讲了很多。".into() },
            Turn { host: 0, text: "再见。".into() },
        ]);
        assert!(script("Here is a summary of the paper.").is_err());
        // Streamed in bits, a turn is whole once the next begins.
        let mut reader = Reader::default();
        assert!(reader.push("女：你").is_empty() && reader.push("好。\n男").is_empty());
        assert_eq!(reader.push("：嗯。\n"), [Turn { host: 0, text: "你好。".into() }]);
        assert_eq!(reader.push("接着说。"), []);
        assert_eq!(reader.finish(), [Turn { host: 1, text: "嗯。 接着说。".into() }]);
        // A word in brackets that is no stage direction stays when the bracket is not closed.
        assert_eq!(script("男：p (unclosed").unwrap()[0].text, "p (unclosed");
    }

    #[test]
    fn each_turn_is_asked_for_in_its_hosts_voice() {
        let turns = [Turn { host: 0, text: "你好。".into() }, Turn { host: 1, text: "一句。".repeat(400) }];
        assert_eq!(pieces(&turns[0]), ("stella", vec!["你好。".to_string()]));
        let (voice, long) = pieces(&turns[1]);
        assert!(voice == "tide" && long.len() > 1 && long.iter().all(|piece| speech::billed(piece) <= speech::PIECE));
        assert!(transcript("T", &turns).starts_with("# 播客：T\n\n**小星**：你好。\n\n**阿潮**：一句。"));
    }

    #[test]
    fn the_model_is_given_the_paper_up_to_its_references() {
        let paper = Paper { title: "Roots".into(), authors: "Li X; Wang Y".into(), year: Some(2024), journal: "Nature".into(), abstract_text: "We look at roots.".into(), ..Default::default() };
        let pages = vec!["Intro-\nduction text [3].".to_string(), "More.\nReferences\n1. A. B.".to_string()];
        let given = source(&paper, &pages);
        assert!(given.contains("题目：Roots") && given.contains("发表：Nature，2024") && given.contains("摘要：\nWe look at roots."), "{given}");
        assert!(given.contains("Introduction text. More.") && !given.contains("1. A. B."), "{given}");
        assert!(instructions("英文").contains("用英文对话"));
        assert!(request(&Profile::default(), "中文", &Paper::default(), &[]).is_err());
    }

    #[test]
    fn the_sound_is_kept_as_wav() {
        let file = wav(&[1, 0, 2, 0, 9]);
        assert_eq!(file.len(), 48);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(file[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(file[40..44].try_into().unwrap()), 4);
        assert_eq!(file_name("A/B: c?"), "播客 - A B c");
        assert_eq!(file_name(" "), "播客");
        assert_eq!(pcm_of(&wav(&[1, 0, 2, 0])), Some(&[1u8, 0, 2, 0][..]));
        // Played again, the turns share the sound by their length after the pauses between speakers.
        let turns = [Turn { host: 0, text: "ab".into() }, Turn { host: 0, text: "cd".into() }, Turn { host: 1, text: "efgh".into() }];
        assert_eq!(timings(&turns, 800 + PAUSE), [(0, 200), (200, 400), (400 + PAUSE, 800 + PAUSE)]);
        assert_eq!((kept("播客 - A.wav", "wav"), kept("播客 - A.md", "md"), kept("A.wav", "wav"), kept("播客.pdf", "pdf")), (Some(true), Some(false), None, None));
    }
}
