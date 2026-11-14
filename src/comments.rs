//! 批注 on the writing page, and manuscripts that start from a file. The
//! panel beside the manuscript shows the comments on the section being
//! written in place of the candidates: those a Word file came with, the
//! person's own, and the assistant's, which reviews the section (or what is
//! selected of it) when asked and may propose words for a place. The words
//! a comment is about are tinted in the manuscript. Only the person takes a
//! proposal; the comment then stands on the new words, which is what an
//! export to Word shows its reader. The rules are `wisp_core::comment`'s,
//! the Word file's reading and writing `wisp_core::docx`'s.

use crate::app::{Cmd, Modal, Page, Side, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::settings::SettingsPage;
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use std::path::{Path, PathBuf};
use wisp_core::ai;
use wisp_core::comment::{self, Comment};
use wisp_core::docx;
use wisp_core::headings::Sign;
use wisp_core::manuscript::{self, Draft, Source};
use wisp_core::settings::Task;

/// The comments of the manuscript on show, and the assistant's review of it.
pub struct Commenting {
    pub list: Vec<Comment>,
    /// The file the manuscript on show was started from.
    pub source: Option<Source>,
    /// What the assistant is to look at; empty for a review of everything.
    pub wish: Entity<TextInput>,
    /// The person's own comment as it is written.
    pub draft: Entity<TextInput>,
    /// The words that comment is about; empty words: the section as a whole.
    pub drafting: Option<String>,
    pub busy: bool,
    pub error: String,
    /// What came of the last review.
    pub note: String,
    pub model: String,
    /// Counts the reviews: an answer to one that was stopped is not kept.
    generation: u64,
    /// Those dealt with are listed too.
    pub show_done: bool,
    /// The comment last pointed at, from its words or its marker in the manuscript: its card stands out.
    pub focused: Option<String>,
    /// That comment's card is to be scrolled to once the list has been drawn (`comment_reveal`).
    reveal: Option<String>,
    /// The list of cards was drawn in the last frame, so its scroll handle knows where they are.
    list_drawn: bool,
    pub scroll: ScrollHandle,
}

/// What can be done about comments, and with files a manuscript starts from.
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// Have the assistant review the section, or what is selected of it; stop it.
    Review,
    Stop,
    /// Start a comment of the person's own on what is selected; keep it; let it go.
    Write,
    Save,
    Cancel,
    /// Take the assistant's proposal of this comment into the text; all that wait in this section; take one back.
    Take(String),
    TakeAll,
    Back(String),
    /// Remove a comment of the person's or the assistant's.
    Drop(String),
    /// Mark one as dealt with, or as open again.
    Done(String, bool),
    /// Select the words a comment is about.
    Show(String),
    ShowDone(bool),
    /// Start a manuscript from a Word or Markdown file: pick one.
    Import,
    /// In the dialog of a Word file without heading styles: tick or untick the n-th heading guessed; all of them, or none.
    Heading(usize),
    Headings(bool),
    /// Make a manuscript of the sections a review wrote, by its conversation.
    FromReview(String),
}

impl Commenting {
    pub fn new(list: Vec<Comment>, source: Option<Source>, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let wish = cx.new(|cx| TextInput::new("想让助手看什么，可不填。例如：补充方法细节", cx));
        let draft = cx.new(|cx| {
            let mut input = TextInput::new("写下批注，回车保存", cx).multiline(2);
            input.submit_on_enter = true;
            input
        });
        let subscriptions = vec![
            cx.subscribe(&wish, |this, _, event: &InputEvent, cx| if *event == InputEvent::Submit { this.comment_review(cx) }),
            cx.subscribe(&draft, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.comment_save(cx),
                InputEvent::Cancel => this.comment_cancel(cx),
                _ => {}
            }),
        ];
        (Commenting { list, source, wish, draft, drafting: None, busy: false, error: String::new(), note: String::new(), model: String::new(), generation: 0, show_done: false, focused: None, reveal: None, list_drawn: false, scroll: ScrollHandle::new() }, subscriptions)
    }

    /// The comments on a section that are still to be dealt with.
    pub fn open_in(&self, section: &str) -> usize {
        self.list.iter().filter(|comment| comment.section_id == section && comment.state != comment::DONE).count()
    }
}

/// What a file makes of a manuscript, read off the main thread.
pub(crate) struct Taken {
    draft: Draft,
    kind: &'static str,
    /// A Word file itself, to be kept beside the library.
    bytes: Option<Vec<u8>>,
    /// What was read of a Word file, to make the draft again at the headings the person chose.
    read: Option<docx::Read>,
}

/// A Word file without heading styles, waiting for the person to say which
/// of the paragraphs that look like headings start a section (`Modal::Headings`).
pub(crate) struct HeadingsForm {
    library: String,
    name: String,
    taken: Taken,
    /// Beside each of `taken.draft.headings`.
    ticked: Vec<bool>,
    /// Files of the same drop that are to be asked about next.
    waiting: Vec<(String, Taken)>,
}

/// What made a paragraph look like a heading, as the dialog says it.
fn sign_label(sign: Sign) -> &'static str {
    match sign {
        Sign::Named => t("常见章节名"),
        Sign::Numbered => t("有编号"),
        Sign::Figure => t("以图号结尾"),
        Sign::Bold => t("加粗"),
        Sign::Alone => t("短句单独成行"),
    }
}

fn is_word(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx"))
}

/// A file a manuscript can start from, by its name.
pub fn is_manuscript_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["docx", "md", "markdown"].iter().any(|known| e.eq_ignore_ascii_case(known)))
}

fn take_file(path: &Path) -> Result<Taken, String> {
    let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("doc")) { return Err(t("旧的 .doc 文件读不了：请先在 Word 里另存为 .docx。").to_string()) }
    let bytes = std::fs::read(path).map_err(|err| tf!("无法读取 {name}：{err}", "Cannot read {name}: {err}"))?;
    if is_word(path) {
        let read = wisp_core::docx::read(&bytes).map_err(|err| format!("{err:#}"))?;
        return Ok(Taken { draft: manuscript::from_word(&read, &name), kind: "docx", bytes: Some(bytes), read: Some(read) });
    }
    Ok(Taken { draft: manuscript::from_markdown(&String::from_utf8_lossy(&bytes), &name), kind: "markdown", bytes: None, read: None })
}

impl Workspace {
    // ---- the manuscript's comments

    /// Read the comments and the source of the manuscript on show.
    pub(crate) fn comments_load(&mut self) {
        let Some(lib) = self.lib.as_mut() else { return };
        let id = lib.writing.manuscript.clone();
        lib.writing.comments.list = lib.store.comments(&id).unwrap_or_default();
        lib.writing.comments.source = lib.store.manuscript_source(&id).ok().flatten();
    }

    /// The words comments are about, in the section being written: where they stand now, with the tint each is drawn on, and the comment.
    pub(crate) fn comment_marks(&self, text: &str) -> Vec<(Range<usize>, Hsla, &Comment)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let writing = &lib.writing;
        writing.comments.list.iter().filter(|comment| comment.section_id == writing.section_id && comment.state != comment::DONE).filter_map(|comment| Some((comment::locate(text, &comment.quote, None)?, if comment.proposes() { Theme::note_strong() } else { Theme::note() }, comment))).collect()
    }

    /// The comments of the section being written in the order the panel lists them: as their words stand in the text,
    /// those whose words are gone last; with where their words begin.
    fn comments_in_order<'a>(&'a self, text: &str) -> Vec<(Option<usize>, &'a Comment)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let writing = &lib.writing;
        let mut shown: Vec<(Option<usize>, &Comment)> = writing.comments.list.iter().filter(|comment| comment.section_id == writing.section_id).map(|comment| (comment::locate(text, &comment.quote, None).map(|range| range.start), comment)).collect();
        shown.sort_by_key(|(at, _)| at.unwrap_or(usize::MAX));
        shown
    }

    /// The comment's card on show beside the manuscript, standing out, and scrolled to.
    pub(crate) fn comment_point(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.writing.side = Side::Comments;
        let comments = &mut lib.writing.comments;
        (comments.focused, comments.reveal) = (Some(id.to_string()), Some(id.to_string()));
        cx.notify();
    }

    /// Before the panel is drawn: scroll to the card asked for, once the list was drawn in the last frame;
    /// a list that was not has no bounds to scroll by, so that waits a frame.
    pub(crate) fn comment_reveal(&mut self, window: &Window, cx: &App) {
        let Some(lib) = self.lib.as_ref() else { return };
        let comments = &lib.writing.comments;
        let drawn = lib.writing.side == Side::Comments && !lib.writing.reading;
        let Some(id) = comments.reveal.clone().filter(|_| drawn) else {
            if let Some(lib) = self.lib.as_mut() { lib.writing.comments.list_drawn = drawn }
            return;
        };
        if !comments.list_drawn {
            if let Some(lib) = self.lib.as_mut() { lib.writing.comments.list_drawn = true }
            return window.request_animation_frame();
        }
        let text = lib.writing.editor.read(cx).text().to_string();
        let show_done = comments.show_done;
        let at = self.comments_in_order(&text).into_iter().filter(|(_, comment)| show_done || comment.state != comment::DONE).position(|(_, comment)| comment.id == id);
        let Some(lib) = self.lib.as_mut() else { return };
        if let Some(at) = at { lib.writing.comments.scroll.scroll_to_item(at) }
        lib.writing.comments.reveal = None;
    }

    /// A click on words a comment is about: its card is shown, the caret stays where it was put.
    pub(crate) fn comment_mark_clicked(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let text = lib.writing.editor.read(cx).text().to_string();
        let Some(id) = self.comment_marks(&text).get(n).map(|(_, _, comment)| comment.id.clone()) else { return };
        self.comment_point(&id, cx);
    }

    pub(crate) fn run_comment(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Review => self.comment_review(cx),
            Do::Stop => {
                if let Some(comments) = self.lib.as_mut().map(|lib| &mut lib.writing.comments) {
                    comments.generation += 1;
                    comments.busy = false;
                    comments.note = t("已停止。").into();
                }
            }
            Do::Write => {
                let Some(lib) = self.lib.as_mut() else { return };
                let writing = &mut lib.writing;
                let (text, selection) = {
                    let editor = writing.editor.read(cx);
                    (editor.text().to_string(), editor.selection())
                };
                // A comment is about words of one paragraph: of a selection over several, the first.
                let picked = text[selection].lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_string();
                writing.comments.drafting = Some(picked);
                writing.side = Side::Comments;
                writing.comments.draft.update(cx, |input, cx| input.set_text("", cx));
                writing.comments.draft.read(cx).focus(window);
            }
            Do::Save => self.comment_save(cx),
            Do::Cancel => self.comment_cancel(cx),
            Do::Take(id) => {
                self.comment_take(&id, cx);
            }
            Do::TakeAll => {
                let waiting: Vec<String> = self.lib.as_ref().map(|lib| lib.writing.comments.list.iter().filter(|comment| comment.section_id == lib.writing.section_id && comment.proposes()).map(|comment| comment.id.clone()).collect()).unwrap_or_default();
                let taken = waiting.iter().filter(|id| self.comment_take(id, cx)).count();
                if taken > 0 { self.say(tf!("已采纳 {taken} 处修改；每一处都可以在批注上撤回。", "Applied {taken} changes; each can be undone from its comment."), cx) }
            }
            Do::Back(id) => self.comment_back(&id, cx),
            Do::Drop(id) => {
                let Some(lib) = self.lib.as_mut() else { return };
                if let Err(err) = lib.store.remove_comment(&id) { return self.fail(format!("{err:#}"), cx) }
                lib.writing.comments.list.retain(|comment| comment.id != id);
            }
            Do::Done(id, done) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(found) = lib.writing.comments.list.iter_mut().find(|comment| comment.id == id) else { return };
                // One whose proposal was taken goes back to that, not to waiting.
                found.state = if done { comment::DONE.into() } else if found.replaced.is_empty() { comment::OPEN.into() } else { comment::APPLIED.into() };
                let kept = found.clone();
                if let Err(err) = lib.store.save_comment(&kept) { self.fail(format!("{err:#}"), cx) }
            }
            Do::Show(id) => self.comment_show(&id, window, cx),
            Do::ShowDone(show) => {
                if let Some(lib) = self.lib.as_mut() { lib.writing.comments.show_done = show }
            }
            Do::Import => self.pick_manuscript_file(cx),
            Do::Heading(n) => {
                if let Some(Modal::Headings(form)) = self.modal.as_mut() && let Some(ticked) = form.ticked.get_mut(n) { *ticked = !*ticked }
            }
            Do::Headings(all) => {
                if let Some(Modal::Headings(form)) = self.modal.as_mut() { form.ticked.iter_mut().for_each(|ticked| *ticked = all) }
            }
            Do::FromReview(conversation) => self.review_into_manuscript(conversation, window, cx),
        }
        cx.notify();
    }

    fn comment_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(lib) = self.lib.as_mut() { lib.writing.comments.drafting = None }
        cx.notify();
    }

    /// Keep the comment the person wrote on the words picked.
    fn comment_save(&mut self, cx: &mut Context<Self>) {
        let author = self.settings.author_name();
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(quote) = writing.comments.drafting.clone() else { return };
        let body = writing.comments.draft.read(cx).text().trim().to_string();
        if body.is_empty() { return }
        match lib.store.add_comment(&writing.manuscript, &writing.section_id, &quote, &body, &author, comment::PERSON) {
            Ok(made) => {
                writing.comments.list.push(made);
                writing.comments.drafting = None;
                writing.comments.draft.update(cx, |input, cx| input.set_text("", cx));
            }
            Err(err) => self.fail(format!("{err:#}"), cx),
        }
        cx.notify();
    }

    /// Put the assistant's proposal into the text, as one step the editor's Undo takes back. Whether it was.
    fn comment_take(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(lib) = self.lib.as_mut() else { return false };
        let writing = &mut lib.writing;
        let Some(found) = writing.comments.list.iter().find(|comment| comment.id == id && comment.proposes() && comment.section_id == writing.section_id).cloned() else { return false };
        let text = writing.editor.read(cx).text().to_string();
        let Some(range) = comment::locate(&text, &found.quote, None) else {
            self.fail(t("这条批注所指的文字已经改动，找不到了：可以照它的建议自己改，再删掉批注。"), cx);
            return false;
        };
        let taken = found.taken();
        if let Err(err) = lib.store.save_comment(&taken) {
            self.fail(format!("{err:#}"), cx);
            return false;
        }
        writing.editor.update(cx, |input, cx| input.replace_range(range, &found.suggestion, cx));
        if let Some(kept) = writing.comments.list.iter_mut().find(|comment| comment.id == id) { *kept = taken }
        true
    }

    /// Put back the words a proposal replaced; the proposal waits again.
    fn comment_back(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(found) = writing.comments.list.iter().find(|comment| comment.id == id && comment.state == comment::APPLIED && comment.section_id == writing.section_id).cloned() else { return };
        let text = writing.editor.read(cx).text().to_string();
        let Some(range) = comment::locate(&text, &found.quote, None) else { return self.fail(t("改后的文字又被改动过，找不到了：可以在「历史版本」里找回原来的文字。"), cx) };
        let back = found.taken_back();
        if let Err(err) = lib.store.save_comment(&back) { return self.fail(format!("{err:#}"), cx) }
        writing.editor.update(cx, |input, cx| input.replace_range(range, &found.replaced, cx));
        if let Some(kept) = writing.comments.list.iter_mut().find(|comment| comment.id == id) { *kept = back }
    }

    /// Select the words a comment is about and bring them into view, with its card beside them.
    fn comment_show(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_point(id, cx);
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(found) = writing.comments.list.iter().find(|comment| comment.id == id) else { return };
        let text = writing.editor.read(cx).text().to_string();
        let Some(range) = comment::locate(&text, &found.quote, None) else { return };
        let top = writing.editor.read(cx).top_of(range.start);
        writing.editor.update(cx, |input, cx| input.select_range(range, cx));
        writing.editor.read(cx).focus(window);
        // A few lines above it stay in view, so it is read in its place.
        if let Some(top) = top { writing.editor_scroll.set_offset(point(px(0.), -(top - px(70.)).max(px(0.)))) }
    }

    /// A change the model made to a paragraph of a manuscript that came from a Word file is said where it was made, as in Word's review:
    /// a comment on the new words that keeps the old. One on the words replaced gives way to it.
    pub(crate) fn comment_on_change(&mut self, original: &str, result: &str, why: &str) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        if !writing.comments.source.as_ref().is_some_and(|source| source.kind == "docx") || original.trim() == result.trim() { return }
        // Changed a second time, it still says what the file had.
        let earlier = writing.comments.list.iter().position(|comment| comment.origin == comment::ASSISTANT && comment.state == comment::APPLIED && comment.section_id == writing.section_id && comment.quote == original.trim());
        let replaced = earlier.map_or(original.trim().to_string(), |at| writing.comments.list[at].replaced.clone());
        if let Some(at) = earlier {
            let gone = writing.comments.list.remove(at);
            let _ = lib.store.remove_comment(&gone.id);
        }
        // Words of one paragraph: of a result over several, the first.
        let quote = result.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_string();
        let Ok(mut made) = lib.store.add_comment(&writing.manuscript, &writing.section_id, &quote, why, comment::ASSISTANT_NAME, comment::ASSISTANT) else { return };
        (made.replaced, made.state) = (replaced, comment::APPLIED.into());
        if lib.store.save_comment(&made).is_ok() { writing.comments.list.push(made) }
    }

    /// Ask the assistant to review the section being written, or what is selected of it.
    pub(crate) fn comment_review(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let language = self.manuscript_language();
        let reply_in = if crate::i18n::is_english() { "英文" } else { "中文" };
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        if writing.comments.busy { return }
        let (text, selection) = {
            let editor = writing.editor.read(cx);
            (editor.text().to_string(), editor.selection())
        };
        let whole = selection.is_empty();
        let scope = if whole { text } else { text[selection].to_string() };
        let scope_text = scope.clone();
        let (manuscript_id, section_id) = (writing.manuscript.clone(), writing.section_id.clone());
        let name = writing.section().map(|section| section.name.clone()).unwrap_or_default();
        let wish = writing.comments.wish.read(cx).text().trim().to_string();
        // What others said about this text, for the assistant to answer first.
        let theirs: Vec<(String, String, String)> = writing.comments.list.iter().filter(|comment| comment.origin != comment::ASSISTANT && comment.state == comment::OPEN && comment.section_id == section_id && (comment.quote.is_empty() || comment::locate(&scope, &comment.quote, None).is_some())).map(|comment| (comment.quote.clone(), comment.author.clone(), comment.body.clone())).collect();
        writing.side = Side::Comments;
        let comments = &mut writing.comments;
        (comments.error, comments.note) = (String::new(), String::new());
        let request = comment::review_prompt(&language, &name, &scope, &wish, &theirs, reply_in).and_then(|(system, message)| ai::plain_request(&profile, &system, &message));
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                comments.error = error;
                return cx.notify();
            }
        };
        comments.generation += 1;
        (comments.busy, comments.model) = (true, profile.labelled());
        let (generation, library, protocol) = (comments.generation, lib.info.id.clone(), profile.protocol);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol) }).await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|lib| lib.info.id == library && lib.writing.comments.generation == generation) else { return };
                let writing = &mut lib.writing;
                writing.comments.busy = false;
                cx.notify();
                // The section as it stands now: it may have been written on meanwhile.
                let Some(text) = writing.manuscripts.iter().flat_map(|m| &m.sections).find(|section| section.id == section_id).map(|section| section.body.clone()) else { return };
                let (remarks, lost) = match reply.and_then(|reply| comment::parse_review(&reply, &text)) {
                    Ok(read) => read,
                    Err(error) => return writing.comments.error = error,
                };
                // A new review of the same text takes the place of the last one's remarks that still wait: those taken or marked as dealt with stay.
                let superseded: Vec<String> = writing.comments.list.iter().filter(|comment| comment.origin == comment::ASSISTANT && comment.state == comment::OPEN && comment.section_id == section_id && (whole || comment::locate(&scope_text, &comment.quote, None).is_some())).map(|comment| comment.id.clone()).collect();
                for id in &superseded { let _ = lib.store.remove_comment(id); }
                writing.comments.list.retain(|comment| !superseded.contains(&comment.id));
                let fresh = remarks;
                match lib.store.add_remarks(&manuscript_id, &section_id, &fresh) {
                    Ok(made) => {
                        let count = made.len();
                        if writing.manuscript == manuscript_id { writing.comments.list.extend(made) }
                        writing.comments.note = match (count, lost) {
                            (0, 0) => t("助手没有要说的。").to_string(),
                            (count, 0) => tf!("助手留了 {count} 条批注。", "The assistant left {count} comments."),
                            (count, lost) => tf!("助手留了 {count} 条批注；另有 {lost} 条引的原文在稿件里找不到，没有留下。", "The assistant left {count} comments; {lost} more quoted text not found in the manuscript and were dropped."),
                        };
                    }
                    Err(err) => writing.comments.error = format!("{err:#}"),
                }
            })
            .ok();
        })
        .detach();
    }

    // ---- a manuscript from a file, or from what the assistant wrote

    fn pick_manuscript_file(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(t("导入").into()), extensions: Some(vec!["docx".into(), "md".into(), "markdown".into(), "txt".into()]) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.import_manuscript_files(paths, cx)).ok();
            }
        })
        .detach();
    }

    /// Start a manuscript from each of these files (Word, Markdown or plain text), read off the main thread.
    pub(crate) fn import_manuscript_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(library) = self.lib.as_ref().map(|lib| lib.info.id.clone()) else { return };
        cx.spawn(async move |this, cx| {
            for path in paths {
                let reading = path.clone();
                let taken = cx.background_executor().spawn(async move { take_file(&reading) }).await;
                this.update(cx, |this, cx| this.manuscript_taken(&library, &path, taken, cx)).ok();
            }
        })
        .detach();
    }

    fn manuscript_taken(&mut self, library: &str, path: &Path, taken: Result<Taken, String>, cx: &mut Context<Self>) {
        let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
        let taken = match taken {
            Ok(taken) => taken,
            Err(error) => return self.fail(tf!("「{name}」没有导入：{error}", "“{name}” not imported: {error}"), cx),
        };
        // A Word file without heading styles is cut only where the person says.
        if !taken.draft.headings.is_empty() {
            if let Some(Modal::Headings(form)) = self.modal.as_mut() {
                form.waiting.push((name, taken));
            } else {
                self.headings_ask(library.to_string(), name, taken, vec![], cx);
            }
            return;
        }
        self.manuscript_import(library, &name, taken, cx);
    }

    /// Ask which paragraphs of a Word file without heading styles start a section.
    fn headings_ask(&mut self, library: String, name: String, taken: Taken, waiting: Vec<(String, Taken)>, cx: &mut Context<Self>) {
        let ticked = taken.draft.headings.iter().map(|guess| guess.likely).collect();
        self.modal_error.clear();
        self.modal = Some(Modal::Headings(Box::new(HeadingsForm { library, name, taken, ticked, waiting })));
        cx.notify();
    }

    /// The headings dialog is done with, by import (`chosen`) or not: the next file waiting is asked about.
    pub(crate) fn headings_done(&mut self, import: bool, cx: &mut Context<Self>) {
        let Some(Modal::Headings(form)) = self.modal.take() else { return };
        let HeadingsForm { library, name, mut taken, ticked, mut waiting } = *form;
        if import {
            let chosen: Vec<usize> = taken.draft.headings.iter().zip(&ticked).filter(|(_, on)| **on).map(|(guess, _)| guess.paragraph).collect();
            if let Some(read) = taken.read.as_ref() { taken.draft = manuscript::from_word_with(read, &name, Some(&chosen)) }
            self.manuscript_import(&library, &name, taken, cx);
        }
        if !waiting.is_empty() {
            let (next, taken) = waiting.remove(0);
            self.headings_ask(library, next, taken, waiting, cx);
        }
    }

    fn manuscript_import(&mut self, library: &str, name: &str, taken: Taken, cx: &mut Context<Self>) {
        let name = name.to_string();
        self.write_snapshot("leave");
        let Some(lib) = self.lib.as_mut().filter(|lib| lib.info.id == library) else { return };
        let made = match lib.store.import_manuscript(&taken.draft, taken.kind, &name, taken.bytes.as_deref()) {
            Ok(made) => made,
            Err(err) => return self.fail(tf!("「{name}」没有导入：{err:#}", "“{name}” not imported: {err:#}"), cx),
        };
        let draft = &taken.draft;
        // Opened where there is something to do: the first section with a comment, else the first under a heading that has text.
        let commented = lib.store.comments(&made.id).unwrap_or_default().first().map(|comment| comment.section_id.clone());
        let written = made.sections.iter().find(|section| !section.bare && !section.body.trim().is_empty()).or(made.sections.iter().find(|section| !section.body.trim().is_empty())).or(made.sections.first()).map(|section| section.id.clone());
        let (id, section) = (made.id.clone(), commented.or(written).unwrap_or_default());
        let mut said = tf!("已导入「{name}」：{} 节", "Imported “{name}”: {} sections", made.sections.len());
        if !draft.comments.is_empty() { said.push_str(&tf!("，{} 条批注", ", {} comments", draft.comments.len())) }
        if draft.tables + draft.pictures > 0 { said.push_str(&tf!("。{} 个表格、{} 张图留在 Word 文件里，导出时还在原处", ". {} tables and {} pictures stay in the Word file and return on export", draft.tables, draft.pictures)) }
        if draft.tracked { said.push_str(t("。文件里未接受的修订按接受后的文字读入")) }
        lib.writing.manuscripts.push(made);
        lib.writing.reading = false;
        lib.writing.side = if draft.comments.is_empty() { Side::Candidates } else { Side::Comments };
        lib.writing.structures = lib.store.structures().unwrap_or_default();
        lib.page = Page::Write;
        self.write_show(id, section, cx);
        self.write_texts_changed(cx);
        self.say(said, cx);
    }

    /// The reviews the assistant wrote sections for, for the manuscripts' menu: (conversation, title, sections).
    pub(crate) fn reviews_written(&self) -> Vec<(String, String, usize)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        lib.store.reviews_written().unwrap_or_default().into_iter().map(|(conversation, review)| (conversation, review.title.clone(), review.sections.iter().filter(|section| !section.body.trim().is_empty()).count())).collect()
    }

    /// A manuscript of the sections a review wrote in a conversation with the assistant, to go on with in 写作.
    fn review_into_manuscript(&mut self, conversation: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let found = lib.store.review_for_conversation(&conversation).ok().flatten().zip(lib.store.conversation(&conversation).ok().flatten());
        let Some((review, kept)) = found else { return self.fail(t("这份综述已经不在了。"), cx) };
        let sections: Vec<(String, String)> = review.sections.into_iter().filter(|section| !section.body.trim().is_empty()).map(|section| (section.name, section.body)).collect();
        self.draft_to_manuscript(review.title, sections, kept.transcript.sources, window, cx);
    }

    // ---- render

    /// The paragraphs of a Word file without heading styles that look like headings: the person ticks those that start a section.
    pub(crate) fn render_headings_dialog(&self, cx: &mut Context<Self>) -> Option<Div> {
        let Some(Modal::Headings(form)) = self.modal.as_ref() else { return None };
        let paragraphs = form.taken.read.as_ref().map_or(&[][..], |read| read.paragraphs.as_slice());
        let rows: Vec<Stateful<Div>> = form
            .taken
            .draft
            .headings
            .iter()
            .zip(&form.ticked)
            .enumerate()
            .map(|(n, (guess, on))| {
                // What follows it, so the person sees what the section would start with.
                let next: String = paragraphs.get(guess.paragraph + 1).map_or(String::new(), |para| para.text.chars().take(60).collect());
                div()
                    .id(("heading-guess", n))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .py(px(6.))
                    .pr(px(8.))
                    .pl(px(8. + 18. * (guess.level.saturating_sub(1).min(3)) as f32))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .on_click(self.on(Cmd::Comment(Do::Heading(n)), cx))
                    .child(div().mt(px(2.)).child(crate::extract::tick_box(*on, true)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().flex().items_baseline().gap(px(8.)).child(div().flex_1().min_w_0().truncate().text_color(if *on { Theme::text() } else { Theme::text_secondary() }).when(*on, |d| d.font_weight(FontWeight::MEDIUM)).child(guess.text.clone())).child(div().flex_none().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(sign_label(guess.sign))))
                            .when(!next.is_empty(), |d| d.child(div().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(next))),
                    )
            })
            .collect();
        let count = form.ticked.iter().filter(|on| **on).count();
        let more = if form.waiting.is_empty() { String::new() } else { tf!("之后还有 {} 个文件。", "{} more files after this one.", form.waiting.len()) };
        Some(
            ui::dialog(560.)
                .child(ui::dialog_title(t("按哪些段落分节？")))
                .child(
                    div()
                        .id("modal-body")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .gap(px(10.))
                        .px(px(20.))
                        .py(px(8.))
                        .child(div().text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_tertiary()).child(tf!("「{}」没有用 Word 的标题样式。下面这些段落看上去像标题：勾选的每一段开始新的一节，其余留在正文里。导出回 Word 时，它们仍是文件里原来的段落。{more}", "“{}” uses no Word heading styles. These paragraphs look like headings: each one ticked starts a section, the rest stay in the text. Exported back to Word they are the paragraphs they were. {more}", form.name)))
                        .child(div().flex().gap(px(4.)).child(ui::ghost_button("headings-all", None, Some(t("全选").into())).h(text_px(24.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::Headings(true)), cx))).child(ui::ghost_button("headings-none", None, Some(t("全不选").into())).h(text_px(24.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::Headings(false)), cx))))
                        .child(div().flex().flex_col().gap(px(2.)).children(rows))
                        .child(div().px(px(8.)).text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_tertiary()).child(t("导入后也可以在写作里把光标所在的段落设为标题，或从那里拆成新的一节。")))
                        .child(ui::error_line(&self.modal_error)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .gap(px(8.))
                        .px(px(20.))
                        .py(px(14.))
                        .child(ui::secondary_button("modal-cancel", t("不导入")).on_click(self.on(Cmd::CloseModal, cx)))
                        .child(ui::primary_button("modal-ok", if count == 0 { t("不分节导入").to_string() } else { tf!("按 {count} 个标题导入", "Import at {count} headings") }).on_click(self.on(Cmd::SubmitModal, cx))),
                ),
        )
    }

    /// Over the panel beside the manuscript: the candidates, the comments, or the reference list.
    pub(crate) fn render_side_switch(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let shown = lib.writing.side;
        let waiting = lib.writing.comments.open_in(&lib.writing.section_id);
        // What the list on that page cites, and whether a citation of it names no paper.
        let (cited, unresolved) = (lib.citing.listed.shown(lib.citing.whole).len(), lib.citing.listed.issues(lib.citing.whole, &lib.writing.section_id).next().is_some());
        let tab = |id: &'static str, name: String, side: Side, dot: bool, this: &Self, cx: &mut Context<Self>| {
            let on = shown == side;
            div().id(id).flex().flex_1().items_center().justify_center().gap(px(5.)).h(text_px(26.)).rounded(px(6.)).text_size(text_px(12.)).cursor_pointer().when(on, |d| d.bg(Theme::segment()).text_color(Theme::text()).font_weight(FontWeight::MEDIUM)).when(!on, |d| d.text_color(Theme::text_secondary()).hover(|s| s.text_color(Theme::text()))).on_click(this.on(Cmd::WriteSide(side), cx)).child(name).when(dot, |d| d.child(div().flex_none().size(px(6.)).rounded(px(3.)).bg(Theme::danger())))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .mx(px(14.))
            .mt(px(14.))
            .p(px(2.))
            .rounded(px(8.))
            .bg(Theme::groove())
            .child(tab("write-side-candidates", t("候选句").into(), Side::Candidates, false, self, cx))
            .child(tab("write-side-comments", if waiting > 0 { tf!("批注 {waiting}", "Comments {waiting}") } else { t("批注").into() }, Side::Comments, false, self, cx))
            .child(tab("write-side-references", if cited > 0 { tf!("参考文献 {cited}", "References {cited}") } else { t("参考文献").into() }, Side::References, unresolved, self, cx))
    }

    /// The comments on the section being written, beside the manuscript.
    pub(crate) fn render_comments(&self, width: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let comments = &writing.comments;
        let text = writing.editor.read(cx).text().to_string();
        let selected = !writing.editor.read(cx).selection().is_empty();
        let configured = self.settings.profile_for_or_default(Task::Chat).missing().is_none();
        let shown = self.comments_in_order(&text);
        let done = shown.iter().filter(|(_, comment)| comment.state == comment::DONE).count();
        let proposals = shown.iter().filter(|(_, comment)| comment.proposes()).count();
        let elsewhere = comments.list.iter().filter(|comment| comment.section_id != writing.section_id && comment.state != comment::DONE).count();
        let cards: Vec<AnyElement> = shown.iter().filter(|(_, comment)| comments.show_done || comment.state != comment::DONE).map(|(at, comment)| self.render_comment(comment, at.is_some(), width, cx)).collect();
        // The icon is added after the label: the row is turned round so that it leads.
        let small = |id: &'static str, icon: Option<&'static str>, name: SharedString| ui::secondary_button(id, name).h(text_px(28.)).px(px(10.)).gap(px(5.)).text_size(text_px(12.)).when_some(icon, |d, icon| d.flex_row_reverse().child(ui::icon(icon, 13., Theme::accent())));
        let source = comments.source.as_ref().filter(|source| source.kind == "docx");
        let empty = cards.is_empty() && comments.drafting.is_none();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.))
                    .px(px(14.))
                    .pt(px(12.))
                    .pb(px(10.))
                    .child(div().w(px(width - 28.)).child(ui::field(&comments.wish, window, cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(6.))
                            .when(configured && !comments.busy, |d| d.child(small("comment-review", Some("sparkle"), if selected { t("让助手审阅选中的文字").into() } else { t("让助手审阅这一节").into() }).on_click(self.on(Cmd::Comment(Do::Review), cx))))
                            .when(!configured, |d| d.child(ui::ghost_button("comment-settings", Some("settings-gear"), Some(t("配置模型后可以让助手审阅").into())).h(text_px(28.)).text_size(text_px(12.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
                            .when(comments.busy, |d| d.child(div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::text_secondary()).child(ui::spinner("comment-busy", 12., Theme::accent())).child(t("助手正在审阅…"))).child(small("comment-stop", None, t("停止").into()).on_click(self.on(Cmd::Comment(Do::Stop), cx))))
                            .child(small("comment-write", None, if selected { t("给选中的文字写批注").into() } else { t("写批注").into() }).on_click(self.on(Cmd::Comment(Do::Write), cx))),
                    )
                    .when(comments.busy && !comments.model.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).truncate().child(comments.model.clone())))
                    .when(!comments.note.is_empty(), |d| d.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_secondary()).child(comments.note.clone())))
                    .when(!comments.error.is_empty(), |d| d.child(ui::error_line(t(&comments.error))))
                    .when(proposals > 1, |d| d.child(div().flex().child(ui::ghost_button("comment-take-all", Some("check"), Some(tf!("采纳全部 {proposals} 处修改", "Apply all {proposals} changes").into())).h(text_px(24.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::TakeAll), cx))))),
            )
            .children(comments.drafting.as_ref().map(|quote| {
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(width - 20.))
                    .gap(px(8.))
                    .mx(px(10.))
                    .mb(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(Theme::accent().opacity(0.45))
                    .bg(Theme::bg_body())
                    .child(if quote.is_empty() { div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("没有选中文字：这条批注是对这一节说的。")) } else { quoted(quote, true) })
                    .child(div().w(px(width - 46.)).child(ui::field(&comments.draft, window, cx)))
                    .child(div().flex().justify_end().gap(px(6.)).child(ui::ghost_button("comment-cancel", None, Some(t("取消").into())).h(text_px(26.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::Cancel), cx))).child(ui::primary_button("comment-save", t("保存")).h(text_px(26.)).px(px(10.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::Save), cx))))
            }))
            .child(if empty {
                let hint = match source {
                    Some(source) => tf!("这份稿件来自「{}」。让助手审阅，或选中一段文字写批注；导出为 Word 时，改动和批注都写回原文件。", "This manuscript came from “{}”. Ask the assistant to review it, or select text and comment; a Word export writes the changes and comments back into that file.", source.name),
                    None => t("让助手审阅这一节，或选中一段文字写批注。导出为 Word 时，批注会写进文件，在 Word 的审阅里能看到。").to_string(),
                };
                div().flex().flex_col().flex_1().min_h_0().px(px(14.)).child(ui::empty_state("message-square", t("这一节还没有批注"), &hint)).into_any_element()
            } else {
                div().id("write-comments").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&comments.scroll).px(px(10.)).pb(px(12.)).gap(px(6.)).children(cards).into_any_element()
            })
            .when(done > 0 || elsewhere > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(Theme::line())
                        .text_size(text_px(11.))
                        .text_color(Theme::text_tertiary())
                        .when(elsewhere > 0, |d| d.child(tf!("其他各节还有 {elsewhere} 条", "{elsewhere} more in other sections")))
                        .child(div().flex_1())
                        .when(done > 0, |d| d.child(ui::ghost_button("comment-show-done", None, Some(if comments.show_done { tf!("收起已处理的 {done} 条", "Hide {done} resolved") } else { tf!("已处理 {done} 条", "{done} resolved") }.into())).h(text_px(22.)).text_size(text_px(11.)).on_click(self.on(Cmd::Comment(Do::ShowDone(!comments.show_done)), cx)))),
                )
            })
    }

    /// In the manuscript's right margin, a marker on each line where commented words begin: hovered, it says
    /// the comments; clicked, it selects the first one's words and shows its card. `tops` are where the marks
    /// were last drawn, one per mark; until they are, there is no marker.
    pub(crate) fn render_comment_markers(&self, marks: &[(Range<usize>, Hsla, &Comment)], tops: &[Pixels], cx: &mut Context<Self>) -> Vec<AnyElement> {
        if marks.is_empty() || tops.len() != marks.len() { return vec![] }
        let focused = self.lib.as_ref().and_then(|lib| lib.writing.comments.focused.clone());
        // The marks of one line share its marker, in the order they stand.
        let mut lines: Vec<(Pixels, Vec<&Comment>)> = vec![];
        let mut order: Vec<usize> = (0..marks.len()).collect();
        order.sort_by_key(|&n| (tops[n], marks[n].0.start));
        for n in order {
            match lines.last_mut() {
                Some((top, comments)) if (*top - tops[n]).abs() < px(1.) => comments.push(marks[n].2),
                _ => lines.push((tops[n], vec![marks[n].2])),
            }
        }
        lines
            .into_iter()
            .map(|(top, comments)| {
                let first = comments[0].id.clone();
                let on = comments.iter().any(|comment| focused.as_deref() == Some(comment.id.as_str()));
                let proposes = comments.iter().any(|comment| comment.proposes());
                let tip = comments.iter().map(|comment| comment_tip(comment)).collect::<Vec<_>>().join("\n\n");
                div()
                    .id(SharedString::from(format!("comment-marker-{first}")))
                    .absolute()
                    // The text's top padding, then the line, centred on its 26px.
                    .top(px(14.) + top + px(3.))
                    .right(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .min_w(px(20.))
                    .h(text_px(20.))
                    .px(px(4.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if on { Theme::note_line() } else { Theme::note_line().opacity(0.55) })
                    .bg(if on || proposes { Theme::note_strong() } else { Theme::note() })
                    .cursor_pointer()
                    .occlude()
                    .hover(|s| s.bg(Theme::note_strong()).border_color(Theme::note_line()))
                    .on_click(self.on(Cmd::Comment(Do::Show(first)), cx))
                    .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(tip.clone())).into())
                    .child(ui::icon("message-square", 11., Theme::text_secondary()))
                    .when(comments.len() > 1, |d| d.child(div().text_size(text_px(10.)).line_height(text_px(12.)).text_color(Theme::text_secondary()).child(comments.len().to_string())))
                    .into_any_element()
            })
            .collect()
    }

    /// One comment: who said it, the words it is about, what it says, and what can be done about it.
    fn render_comment(&self, comment: &Comment, anchored: bool, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let id = |part: &str| SharedString::from(format!("comment-{part}-{}", comment.id));
        let small = |part: &str, name: &'static str| ui::ghost_button(id(part), None, Some(name.into())).h(text_px(24.)).px(px(7.)).text_size(text_px(12.));
        let who = author(comment);
        let tint = match comment.origin.as_str() {
            comment::ASSISTANT => Theme::accent(),
            comment::FILE => Theme::warning(),
            _ => Theme::text_secondary(),
        };
        let done = comment.state == comment::DONE;
        let focused = self.lib.as_ref().is_some_and(|lib| lib.writing.comments.focused.as_deref() == Some(comment.id.as_str()));
        let when = wisp_core::time::short_time(&comment.created_at);
        let mut about = vec![];
        if comment.origin == comment::FILE { about.push(t("来自 Word 文件").to_string()) }
        if !when.is_empty() { about.push(when) }
        // What a proposal would change, word by word; what a taken one replaced.
        let change = comment.proposes().then(|| match ai::word_diff(&comment.quote, &comment.suggestion) {
            Some((_, after)) => crate::write_assist::marked(&after),
            None => StyledText::new(comment.suggestion.clone()),
        });
        let card = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width - 20.))
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(if focused { Theme::note_line() } else { Theme::line() })
            .bg(Theme::bg_body())
            .when(focused, |d| d.shadow(crate::theme::shadow_panel()))
            .when(done, |d| d.opacity(0.6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_none().size(px(7.)).rounded(px(4.)).bg(tint))
                    .child(div().flex_none().max_w(px(width * 0.45)).truncate().text_size(text_px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text()).child(who))
                    .child(div().flex_1().min_w_0().truncate().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(about.join(" · ")))
                    .when(comment.origin != comment::FILE, |d| d.child(ui::ghost_button(id("drop"), Some("x"), None).size(px(22.)).min_w(px(22.)).on_click(self.on(Cmd::Comment(Do::Drop(comment.id.clone())), cx)))),
            )
            .when(!comment.quote.is_empty(), |d| d.child(div().id(id("quote")).cursor_pointer().on_click(self.on(Cmd::Comment(Do::Show(comment.id.clone())), cx)).child(quoted(&comment.quote, anchored))))
            .when(!anchored && !comment.quote.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::warning()).child(t("这段文字已经改动，批注不再指向稿件里的位置。"))))
            .when(!comment.body.is_empty(), |d| d.child(div().text_size(text_px(13.)).line_height(text_px(21.)).text_color(Theme::text()).child(comment.body.clone())))
            .when_some(change, |d, change| d.child(div().flex().flex_col().gap(px(3.)).child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("建议改为"))).child(div().px(px(10.)).py(px(7.)).rounded(px(8.)).bg(Theme::bg_sunken()).text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text()).child(change))))
            .when(comment.state == comment::APPLIED && !comment.replaced.is_empty(), |d| d.child(div().flex().flex_col().gap(px(3.)).child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("已修改，原文是"))).child(div().px(px(10.)).py(px(7.)).rounded(px(8.)).bg(Theme::bg_sunken()).text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_secondary()).child(clipped(&comment.replaced, 260)))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(2.))
                    .when(comment.proposes() && anchored, |d| d.child(ui::primary_button(id("take"), t("采纳修改")).h(text_px(24.)).px(px(9.)).text_size(text_px(12.)).on_click(self.on(Cmd::Comment(Do::Take(comment.id.clone())), cx))))
                    .when(comment.proposes(), |d| d.child(small("ignore", t("忽略")).on_click(self.on(Cmd::Comment(Do::Drop(comment.id.clone())), cx))))
                    .when(comment.state == comment::APPLIED && !comment.replaced.is_empty() && anchored, |d| d.child(small("back", t("撤回修改")).on_click(self.on(Cmd::Comment(Do::Back(comment.id.clone())), cx))))
                    .child(div().flex_1())
                    .when(!comment.proposes(), |d| d.child(small("done", if done { t("重新打开") } else { t("标为已处理") }).on_click(self.on(Cmd::Comment(Do::Done(comment.id.clone(), !done)), cx)))),
            );
        ui::fade_in(id("card"), card).into_any_element()
    }
}

/// Who made a comment, as its card names them.
fn author(comment: &Comment) -> String {
    match comment.origin.as_str() {
        comment::ASSISTANT => t("助手").to_string(),
        comment::FILE => if comment.author.is_empty() { "Word".to_string() } else { comment.author.clone() },
        _ => if comment.author.is_empty() { t("我").to_string() } else { comment.author.clone() },
    }
}

/// A comment in a few lines, for a marker's hover: who, what it says, and what it would change.
fn comment_tip(comment: &Comment) -> String {
    let mut lines = vec![if comment.body.is_empty() { author(comment) } else { format!("{}{}{}", author(comment), if crate::i18n::is_english() { ": " } else { "：" }, clipped(&comment.body, 140)) }];
    if comment.proposes() { lines.push(tf!("建议改为：{}", "Suggests: {}", clipped(&comment.suggestion, 100))) }
    lines.join("\n")
}

fn clipped(text: &str, most: usize) -> String {
    let short: String = text.chars().take(most).collect();
    if short.len() < text.len() { format!("{short}…") } else { short }
}

/// The words a comment is about, as a line by its side.
fn quoted(quote: &str, anchored: bool) -> Div {
    div().pl(px(8.)).border_l_2().border_color(if anchored { Theme::note_line() } else { Theme::line_strong() }).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(clipped(quote, 160))
}

#[cfg(test)]
mod tests {
    use super::{clipped, is_manuscript_file, take_file};
    use std::path::Path;

    #[test]
    fn files_a_manuscript_can_start_from() {
        assert!(is_manuscript_file(Path::new("/a/Draft v2.DOCX")) && is_manuscript_file(Path::new("notes.md")));
        assert!(!is_manuscript_file(Path::new("refs.txt")) && !is_manuscript_file(Path::new("paper.pdf")));
        assert!(take_file(Path::new("/nowhere/old.doc")).is_err());
        assert_eq!((clipped("短", 5), clipped("一二三四五六", 3)), ("短".to_string(), "一二三…".to_string()));
        let dir = std::env::temp_dir().join(format!("wisp-take-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("根.md"), "# 根的综述\n\n## 引言\n\n根向下生长，吸收水分和养分。\n").unwrap();
        let taken = take_file(&dir.join("根.md")).unwrap();
        assert_eq!((taken.kind, taken.bytes.is_none(), taken.draft.title.as_str(), taken.draft.sections[0].name.as_str()), ("markdown", true, "根的综述", "引言"));
        std::fs::write(dir.join("坏.docx"), b"not a zip").unwrap();
        assert!(take_file(&dir.join("坏.docx")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
