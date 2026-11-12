//! 写作's candidates, asked for: the shortcut (or the button) searches for
//! the selected text or the paragraph at the caret, the find box for its
//! words on Enter. What is written is first made into a few groups of
//! search words by the translation model, in the language of the library's
//! sentences, each group ranked on its own by `find_sentences` and the
//! lists merged. Only the list of how each paper opens the part comes by
//! itself, while nothing has been asked.

use crate::app::{Library, Page, Workspace, Writing};
use gpui::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use wisp_core::ai;
use wisp_core::retrieval::{self, Group};
use wisp_core::settings::Task;

/// The sentences one search shows.
const SHOWN: usize = 30;

/// What a search was for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub section: String,
    /// Empty for how each paper opens the part.
    pub text: String,
    /// The words go as written (the find box, in the sentences' own script).
    pub literal: bool,
}

/// The state of the candidates beside the manuscript.
#[derive(Default)]
pub struct CandidateSearch {
    /// The search the candidates on show (or on their way) are for.
    pub asked: Option<Asked>,
    /// What `find_sentences` gave.
    pub candidates: Option<Result<Value, String>>,
    pub searching: bool,
    round: u64,
    /// The groups of words the search on show was made with.
    pub groups: Vec<Group>,
    /// Why the words went as written, when a model should have made them.
    pub note: String,
    /// The groups a model made of a text, by language and text: a text is asked about once.
    made: HashMap<String, Result<Vec<Group>, String>>,
    /// Only the papers with this tag, or (also) only the starred ones.
    pub tag: String,
    pub starred: bool,
    /// The cards that show the sentences around theirs, by card id.
    pub unfolded: HashSet<String>,
}

impl Writing {
    /// The kind of part being written: a key of `structure::SECTIONS`.
    pub fn kind(&self) -> &str {
        &self.section
    }
}

impl Library {
    /// The language the library's sentences are in, for the model to make
    /// search words in. The full texts are taken to be English.
    pub fn sentences_language(&self) -> &'static str {
        "英文"
    }
}

/// Whether search words can go to the sentences as written: every letter
/// in them is a plain Latin one, as the sentences' are.
fn plain_latin(text: &str) -> bool {
    text.chars().all(|c| !c.is_alphabetic() || c.is_ascii())
}

impl Workspace {
    /// The shortcut or the button: search for the selected text, or the
    /// paragraph the caret is in. The find box's Enter: its words.
    pub fn write_find(&mut self, from_box: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut().filter(|l| l.page == Page::Write) else { return };
        let writing = &lib.writing;
        let text = if from_box {
            writing.find.read(cx).text().trim().to_string()
        } else {
            let editor = writing.editor.read(cx);
            let selected = editor.selected().trim();
            if selected.is_empty() { retrieval::paragraph_at(editor.text(), editor.caret()) } else { selected.to_string() }
        };
        let literal = from_box && plain_latin(&text);
        let asked = Asked { section: writing.kind().to_string(), text, literal };
        self.write_search(asked, cx);
    }

    /// How each paper opens the part: shown while nothing is asked.
    pub fn write_openers(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut().filter(|l| l.page == Page::Write) else { return };
        let section = lib.writing.kind().to_string();
        if lib.writing.search.asked.as_ref().is_some_and(|asked| asked.section == section) { return }
        self.write_search(Asked { section, text: String::new(), literal: true }, cx);
    }

    /// The texts have changed (read, sorted anew): the opening sentences are
    /// found again; a search asked for stays as it is until the next one.
    pub fn write_texts_changed(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut().filter(|l| l.page == Page::Write) else { return };
        let search = &mut lib.writing.search;
        if search.asked.as_ref().is_some_and(|asked| !asked.text.is_empty()) { return }
        if let Some(asked) = search.asked.clone() { self.write_search(asked, cx) } else { self.write_openers(cx) }
    }

    /// The same search again, over other papers.
    pub fn write_narrow(&mut self, tag: String, starred: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let search = &mut lib.writing.search;
        (search.tag, search.starred) = (tag, starred);
        if let Some(asked) = search.asked.clone() { self.write_search(asked, cx) }
    }

    fn write_search(&mut self, asked: Asked, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Translate);
        let Some(lib) = self.lib.as_mut() else { return };
        let language = lib.sentences_language();
        let (library, protocol) = (lib.info.id.clone(), profile.protocol);
        let search = &mut lib.writing.search;
        search.round += 1;
        search.asked = Some(asked.clone());
        search.searching = true;
        search.note.clear();
        search.groups.clear();
        let round = search.round;
        if asked.text.is_empty() || asked.literal { return self.write_rank(round, vec![], cx) }
        let key = format!("{language}\n{}", asked.text);
        match search.made.get(&key) {
            Some(Ok(groups)) => {
                let groups = groups.clone();
                return self.write_rank(round, groups, cx);
            }
            Some(Err(error)) => {
                search.note = format!("模型没有给出检索词（{error}），按原文的词查找。");
                return self.write_rank(round, vec![], cx);
            }
            None => {}
        }
        // Without a model the words go as written, and find what they can.
        let request = if profile.key().is_empty() { Err(String::new()) } else { ai::plain_request(&profile, &retrieval::queries_system(language), &asked.text) };
        let Ok(request) = request else {
            search.note = "没有配置模型，按原文的词查找：写的不是英文时几乎找不到。配置模型后，会先把这段变成几组英文检索词。".into();
            return self.write_rank(round, vec![], cx);
        };
        cx.notify();
        let work = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol) });
        cx.spawn(async move |this, cx| {
            let reply = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return };
                // Words in another script than the sentences' would find nothing (a model may slip into the person's language).
                let usable = |mut groups: Vec<Group>| {
                    if language == "英文" { groups.retain(|group| plain_latin(&group.words)) }
                    Some(groups).filter(|groups| !groups.is_empty())
                };
                let made = reply.and_then(|reply| usable(retrieval::parse_groups(&reply)).ok_or_else(|| "回答里没有检索词".to_string()));
                let search = &mut lib.writing.search;
                search.made.insert(key, made.clone());
                if search.round != round { return }
                match made {
                    Ok(groups) => this.write_rank(round, groups, cx),
                    Err(error) => {
                        search.note = format!("模型没有给出检索词（{error}），按原文的词查找。");
                        this.write_rank(round, vec![], cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ask `find_sentences` for the search of `round`, with the groups made for it (none: the words as written).
    fn write_rank(&mut self, round: u64, groups: Vec<Group>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let search = &mut lib.writing.search;
        let Some(asked) = search.asked.clone() else { return };
        let mut args = json!({"section": asked.section, "limit": SHOWN, "context": true});
        if groups.is_empty() { args["query"] = json!(asked.text) } else { args["queries"] = json!(groups.iter().map(|group| group.words.clone()).collect::<Vec<_>>()) }
        if !search.tag.is_empty() { args["tag"] = json!(search.tag) }
        if search.starred { args["starred"] = json!(true) }
        search.groups = groups;
        self.library_tool("find_sentences", args, cx, move |this, result, _| {
            let Some(lib) = this.lib.as_mut().filter(|l| l.writing.search.round == round) else { return };
            let search = &mut lib.writing.search;
            search.searching = false;
            search.candidates = Some(result);
            search.unfolded.clear();
            lib.writing.scroll.set_offset(point(px(0.), px(0.)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::plain_latin;

    #[test]
    fn words_that_go_as_written() {
        assert!(plain_latin("remains unclear (2019)") && plain_latin(""));
        assert!(!plain_latin("根系 soil") && !plain_latin("système racinaire") && !plain_latin("корни"));
    }
}
