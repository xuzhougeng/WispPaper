//! 改稿: the assistant changes the section being written as the person
//! tells it to, in the box under the manuscript. The model answers with
//! blocks, each the words to find in the section and the words to put in
//! their place, so a change can be made the moment its block is complete
//! and the window can show where the next one is going while it is still
//! being written. Nothing here touches a store: `read` reads a reply as far
//! as it has come, `place` finds where a change goes in the text as it now
//! stands, `follow` keeps a change's place while the text around it changes,
//! and `added` is the words to light up.

use std::ops::Range;

/// The longest section the assistant is given to change at once.
pub const LIMIT: usize = 36_000;

const OPEN: &str = "<<<<<<<";
const MIDDLE: &str = "=======";
const CLOSE: &str = ">>>>>>>";

/// One change the model asks for: the words to find, and what to put there.
/// No words to find means the end of the section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub old: String,
    pub new: String,
}

/// A reply as far as it has come.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// The blocks that are complete, in the order written.
    pub edits: Vec<Edit>,
    /// The block being written: its words to find are complete, its new words are those so far.
    pub pending: Option<Edit>,
    /// What the model says beside the blocks.
    pub said: String,
}

/// Read a reply, complete or still streaming.
pub fn read(reply: &str) -> Reply {
    enum At {
        Outside,
        Old(Vec<String>),
        New(String, Vec<String>),
    }
    let mut out = Reply::default();
    let mut said: Vec<&str> = vec![];
    let mut at = At::Outside;
    for line in reply.lines() {
        let mark = line.trim();
        at = match at {
            // What follows the opening marker is its label; anything else there is the first of the words to find,
            // written on the marker's line.
            At::Outside if mark.starts_with(OPEN) => At::Old(opening(mark).into_iter().collect()),
            At::Outside => {
                // A fence the model put round a block, or a marker that is still arriving, is not something said.
                let noise = mark.starts_with("```") || (!mark.is_empty() && mark.chars().all(|c| c == '<'));
                if !noise { said.push(line) }
                At::Outside
            }
            At::Old(lines) if mark.starts_with(MIDDLE) && mark.chars().all(|c| c == '=') => At::New(lines.join("\n"), vec![]),
            // Closed before its new words began: not a change.
            At::Old(_) if mark.starts_with(CLOSE) => At::Outside,
            At::Old(mut lines) => {
                lines.push(line.to_string());
                At::Old(lines)
            }
            At::New(old, lines) if mark.starts_with(CLOSE) => {
                out.edits.push(Edit { old, new: lines.join("\n") });
                At::Outside
            }
            // The next block opened without this one being closed.
            At::New(old, lines) if mark.starts_with(OPEN) => {
                out.edits.push(Edit { old, new: lines.join("\n") });
                At::Old(opening(mark).into_iter().collect())
            }
            At::New(old, mut lines) => {
                lines.push(line.to_string());
                At::New(old, lines)
            }
        };
    }
    if let At::New(old, mut lines) = at {
        // The last line may be the closing marker on its way.
        if lines.last().is_some_and(|line| !line.is_empty() && line.trim().chars().all(|c| c == '>')) { lines.pop(); }
        out.pending = Some(Edit { old, new: lines.join("\n") });
    }
    out.said = said.join("\n").trim().to_string();
    out
}

/// The words on an opening marker's line that are not its label: some models begin the words to find there.
fn opening(mark: &str) -> Option<String> {
    let rest = mark.trim_start_matches('<').trim();
    let label = rest.trim_end_matches([':', '：']).to_lowercase();
    (!["", "search", "original", "old", "原文"].contains(&label.as_str())).then(|| rest.to_string())
}

/// Where a change goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placed {
    /// These bytes of the text become these words.
    At(Range<usize>, String),
    /// Its words are not in the text.
    Missing,
    /// It would change nothing.
    Same,
}

/// Where the words `old` stand in `text`, and whether as written: the place nearest to `near` when there are
/// several, else as `comment::locate` finds them, so a line break or a curled quotation mark written
/// differently does not lose the change. Without words to find it is the end of the text.
pub fn find(text: &str, old: &str, near: Option<usize>) -> Option<(Range<usize>, bool)> {
    if old.trim().is_empty() { return Some((text.trim_end().len()..text.len(), true)) }
    let exact = text.match_indices(old).map(|(at, _)| at).min_by_key(|at| near.map_or(*at, |near| at.abs_diff(near)));
    match exact {
        Some(at) => Some((at..at + old.len(), true)),
        None => crate::comment::locate(text, old, near).map(|range| (range, false)),
    }
}

/// Where `edit` goes in `text` as it stands (`find`), and the words that go there. At the end of the text
/// they open a paragraph of their own; a paragraph taken out takes the empty line after it along.
pub fn place(text: &str, edit: &Edit, near: Option<usize>) -> Placed {
    let Some((mut range, exact)) = find(text, &edit.old, near) else { return Placed::Missing };
    if edit.old.trim().is_empty() {
        let new = edit.new.trim();
        if new.is_empty() { return Placed::Same }
        return Placed::At(range.clone(), if range.start == 0 { new.to_string() } else { format!("\n\n{new}") });
    }
    let new = if exact { edit.new.clone() } else { edit.new.trim().to_string() };
    if text[range.clone()] == new { return Placed::Same }
    if new.trim().is_empty() {
        let alone = (range.start == 0 || text[..range.start].ends_with('\n')) && (range.end == text.len() || text[range.end..].starts_with('\n'));
        if alone {
            let after = text[range.end..].len() - text[range.end..].trim_start_matches('\n').len();
            if after > 0 { range.end += after } else { range.start = text[..range.start].trim_end_matches('\n').len() }
        }
        return Placed::At(range, String::new());
    }
    Placed::At(range, new)
}

/// Where `range` of `before` stands in `after`, when one stretch of the text was changed between the two
/// (a key typed, a paste, an undo, another change): as it was before that stretch, moved along after it,
/// and nowhere once the stretch reaches into it.
pub fn follow(range: &Range<usize>, before: &str, after: &str) -> Option<Range<usize>> {
    if before == after { return Some(range.clone()) }
    let (a, b) = (before.as_bytes(), after.as_bytes());
    let mut head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !before.is_char_boundary(head) { head -= 1 }
    let most = a.len().min(b.len()) - head;
    let mut tail = a.iter().rev().zip(b.iter().rev()).take(most).take_while(|(x, y)| x == y).count();
    while !before.is_char_boundary(a.len() - tail) { tail -= 1 }
    let changed = head..a.len() - tail;
    if range.end <= changed.start { return Some(range.clone()) }
    if range.start >= changed.end {
        let moved = |at: usize| at + b.len() - a.len();
        return Some(moved(range.start)..moved(range.end));
    }
    None
}

/// The words of `new` that `old` did not have, as bytes of `new`: what to light up where a change was made.
/// All of it when the two are too unlike to compare, or when words were only taken out.
pub fn added(old: &str, new: &str) -> Vec<Range<usize>> {
    // Without the spaces and line breaks at either end of a stretch.
    let tidy = |ranges: Vec<Range<usize>>| -> Vec<Range<usize>> { ranges.into_iter().map(|range| range.start + (new[range.clone()].len() - new[range.clone()].trim_start().len())..range.start + new[range.clone()].trim_end().len()).filter(|range| range.start < range.end).collect() };
    let whole = || tidy(std::iter::once(0..new.len()).collect());
    if old.trim().is_empty() { return whole() }
    let Some((_, pieces)) = crate::ai::word_diff(old, new) else { return whole() };
    let (mut at, mut out): (usize, Vec<Range<usize>>) = (0, vec![]);
    for (change, piece) in &pieces {
        let range = at..at + piece.len();
        at = range.end;
        if *change != crate::ai::Change::Added { continue }
        match out.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => out.push(range),
        }
    }
    let out = tidy(out);
    if at != new.len() || out.is_empty() { whole() } else { out }
}

/// What the assistant is asked about.
pub struct Asked<'a> {
    /// The manuscript's language, its title, and the names of its sections in order.
    pub language: &'a str,
    pub title: &'a str,
    pub outline: &'a [String],
    /// The section being changed: its name and its text.
    pub section: &'a str,
    pub text: &'a str,
    /// What is selected in it, when the wish is about that.
    pub selection: &'a str,
    /// What the person wants done.
    pub wish: &'a str,
    /// The language to speak to the person in.
    pub reply_in: &'a str,
}

/// What the assistant is told and asked: (system, message).
pub fn prompt(asked: &Asked) -> Result<(String, String), String> {
    let wish = asked.wish.trim();
    if wish.is_empty() { return Err("先写下想让助手怎么改。".into()) }
    if asked.text.len() > LIMIT { return Err("这一节太长，一次改不完：先拆成几节，或用上面的按钮一段一段改。".into()) }
    let (language, reply_in) = (crate::ai::language_name(asked.language), asked.reply_in);
    let system = format!(
        "你是改稿助手，按用户的要求直接修改他正在写的论文稿件中的一节。稿件语言是{language}，正文是 Markdown。\n\
回答的格式：\n\
1. 每处修改写成一个 SEARCH/REPLACE 块。三行标记各自单独占一行，标记所在的行不写别的内容：\n\
<<<<<<< SEARCH\n\
从这一节里逐字摘出的原文\n\
=======\n\
改成的文字\n\
>>>>>>> REPLACE\n\
2. 原文必须从这一节逐字摘出，一个字、一个标点都不改，尽量短但能唯一定位（通常是一句，最多一段）。一处修改一个块，按它们在稿件里的先后排列，互不重叠。\n\
3. 插入新内容：把插入位置前面的那一句作为原文，改成的文字里先原样写出这一句，再接上新内容。删除：改成的文字留空。写在这一节的末尾，或这一节还没有文字：原文留空。\n\
4. 只改用户要求改的地方，别处一字不动；改成的文字用{language}写。已有的引用（如 [@Zhang2024]、(Smith et al., 2020)、[3]）保持原样，不要编造文献、数据或引用。\n\
5. 块之外用一两句{reply_in}说明改了什么。不需要修改或无法照办时，只用一两句话说明原因，不写块。不要把整节重抄一遍，也不要用代码围栏。"
    );
    let mut message = String::new();
    if !asked.title.trim().is_empty() { message.push_str(&format!("稿件：《{}》\n", asked.title.trim())) }
    if asked.outline.len() > 1 { message.push_str(&format!("各节依次是：{}\n", asked.outline.join("、"))) }
    message.push_str(&format!("正在改的一节：{}\n\n用户的要求：{wish}\n", asked.section.trim()));
    let selection = asked.selection.trim();
    if !selection.is_empty() { message.push_str(&format!("\n用户选中了这段文字，要求是针对它的：\n{selection}\n")) }
    message.push_str("\n这一节的全文：\n");
    message.push_str(if asked.text.trim().is_empty() { "（这一节还没有文字）" } else { asked.text });
    Ok((system, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit { old: old.into(), new: new.into() }
    }

    #[test]
    fn a_reply_is_read_block_by_block() {
        let reply = "我把第一句改短了。\n<<<<<<< SEARCH\nRoots grow.\n=======\nRoots grow fast.\n>>>>>>> REPLACE\n\n```\n<<<<<<<\n\n=======\nA new paragraph.\n\nAnd another.\n>>>>>>>\n```\n就这两处。";
        let read = read(reply);
        assert_eq!(read.edits, vec![edit("Roots grow.", "Roots grow fast."), edit("", "A new paragraph.\n\nAnd another.")]);
        assert_eq!(read.pending, None);
        assert_eq!(read.said, "我把第一句改短了。\n\n就这两处。");
    }

    #[test]
    fn a_reply_still_arriving_says_where_the_next_change_goes() {
        let whole = "好的。\n<<<<<<< 原文\nRoots grow.\n=======\nRoots grow fast,\nand deep.\n>>>>>>> 改后\n";
        // Its words to find still arriving: nothing to point at yet.
        let early = read(&whole[..whole.find("grow.").unwrap()]);
        assert_eq!((early.edits.len(), early.pending, early.said.as_str()), (0, None, "好的。"));
        // A marker on its way is not something said.
        assert_eq!(read("好的。\n<<<<").said, "好的。");
        let middle = read(&whole[..whole.find("and deep").unwrap() + 3]);
        assert_eq!(middle.pending, Some(edit("Roots grow.", "Roots grow fast,\nand")));
        let closing = read(&whole[..whole.find(">>>>>>>").unwrap() + 3]);
        assert_eq!(closing.pending, Some(edit("Roots grow.", "Roots grow fast,\nand deep.")));
        assert_eq!(read(whole).edits, vec![edit("Roots grow.", "Roots grow fast,\nand deep.")]);
    }

    #[test]
    fn a_reply_written_loosely_is_still_read() {
        // The words to find begun on the marker's line, as a smaller model wrote them; a label there is not words.
        let read_ = read("<<<<<<< Roots grow.\n\nShoots too.\n=======\nBoth grow.\n>>>>>>> 改后\n<<<<<<< 原文：\nA.\n=======\nB.\n>>>>>>>");
        assert_eq!(read_.edits, vec![edit("Roots grow.\n\nShoots too.", "Both grow."), edit("A.", "B.")]);
        // A block not closed before the next opens; one closed before its new words is no change.
        let read_ = read("<<<<<<< SEARCH\nA.\n=======\nB.\n<<<<<<< SEARCH\nC.\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nD.\n=======\n>>>>>>> REPLACE");
        assert_eq!(read_.edits, vec![edit("A.", "B."), edit("D.", "")]);
    }

    #[test]
    fn a_change_goes_where_its_words_are() {
        let text = "Roots grow. Shoots grow.\n\nRoots grow. The end.";
        assert_eq!(place(text, &edit("Roots grow.", "Roots branch."), None), Placed::At(0..11, "Roots branch.".into()));
        // The place nearest to where the last change was made.
        assert_eq!(place(text, &edit("Roots grow.", "Roots branch."), Some(30)), Placed::At(26..37, "Roots branch.".into()));
        assert_eq!(place(text, &edit("Leaves grow.", "x"), None), Placed::Missing);
        assert_eq!(place(text, &edit("The end.", "The end."), None), Placed::Same);
        // Written with another line break and quotation marks: found by its characters.
        assert_eq!(place("He said “roots\ngrow” here.", &edit("said \"roots grow\"", "wrote"), None), Placed::At(3..24, "wrote".into()));
    }

    #[test]
    fn without_words_to_find_a_change_goes_to_the_end() {
        assert_eq!(place("", &edit("", "First."), None), Placed::At(0..0, "First.".into()));
        assert_eq!(place("One.\n\n", &edit("\n", "Two.\n"), None), Placed::At(4..6, "\n\nTwo.".into()));
        assert_eq!(place("One.", &edit("", " "), None), Placed::Same);
    }

    #[test]
    fn a_paragraph_taken_out_takes_its_empty_line_along() {
        let text = "One.\n\nTwo.\n\nThree.";
        let Placed::At(range, new) = place(text, &edit("Two.", ""), None) else { panic!() };
        assert_eq!(format!("{}{new}{}", &text[..range.start], &text[range.end..]), "One.\n\nThree.");
        let Placed::At(range, _) = place(text, &edit("Three.", ""), None) else { panic!() };
        assert_eq!(&text[..range.start], "One.\n\nTwo.");
        // Words inside a paragraph leave the lines alone.
        assert_eq!(place("One two.\n\nThree.", &edit(" two", ""), None), Placed::At(3..7, String::new()));
    }

    #[test]
    fn a_place_follows_the_text_around_it() {
        let before = "One. Two. Three.";
        let two = 5..9;
        assert_eq!(follow(&two, before, "One. Two. Three. Four."), Some(5..9));
        assert_eq!(follow(&two, before, "Zero. One. Two. Three."), Some(11..15));
        assert_eq!(follow(&two, before, "One. Three."), None);
        assert_eq!(follow(&two, before, "One. Twoo. Three."), None);
        // Typed right after it: not part of it. Right before it: it moves.
        assert_eq!(follow(&two, before, "One. Two.! Three."), Some(5..9));
        assert_eq!(follow(&two, before, "One. !Two. Three."), Some(6..10));
        // Letters of several bytes are not cut.
        assert_eq!(follow(&(6..9), "根。生长。", "根系。生长。"), Some(9..12));
        // A place without words (something taken out) stays where it is when words are typed at it.
        assert_eq!(follow(&(5..5), before, "One. !Two. Three."), Some(5..5));
    }

    #[test]
    fn the_new_words_are_what_lights_up() {
        let new = "Roots grow fast and deep in soil.";
        let lit: Vec<&str> = added("Roots grow in soil.", new).into_iter().map(|range| &new[range]).collect();
        assert_eq!(lit, vec!["fast and deep"]);
        assert_eq!(added("", "All new."), vec![0..8]);
        // Put at the end of a text: the words, not the empty line before them.
        assert_eq!(added("\n", "\n\nAll new."), vec![2..10]);
        // Words only taken out: the sentence they were in.
        assert_eq!(added("Roots grow very fast.", "Roots grow fast."), vec![0..16]);
        assert_eq!(added("Roots.", ""), Vec::<Range<usize>>::new());
    }

    #[test]
    fn the_prompt_carries_the_section_and_the_wish() {
        let outline = vec!["Abstract".to_string(), "Introduction".to_string()];
        let asked = Asked { language: "en", title: "Roots", outline: &outline, section: "Introduction", text: "Roots grow.", selection: "grow", wish: " 写得更具体 ", reply_in: "中文" };
        let (system, message) = prompt(&asked).unwrap();
        assert!(system.contains("改稿助手") && system.contains("稿件语言是英文") && system.contains("一两句中文"));
        assert!(message.contains("用户的要求：写得更具体\n") && message.contains("选中了这段文字") && message.ends_with("这一节的全文：\nRoots grow."));
        assert!(prompt(&Asked { text: "", selection: "", ..asked }).unwrap().1.ends_with("（这一节还没有文字）"));
        assert!(prompt(&Asked { wish: " ", text: "x", selection: "", language: "en", title: "", outline: &[], section: "", reply_in: "中文" }).is_err());
        assert!(prompt(&Asked { wish: "x", text: &"长".repeat(LIMIT), selection: "", language: "en", title: "", outline: &[], section: "", reply_in: "中文" }).is_err());
    }
}
