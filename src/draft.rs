//! 先聊，后建库: the assistant on the home page. A conversation started there is a library that has not been
//! put on the shelf (`LibraryInfo::draft`): its own folder and database, so the conversation, the papers kept
//! in it, a manuscript handed in for review and its comments are all a library's already, and 建成文献库 only
//! names it and clears the flag (`libraries::promote`). Nothing is made before the first question is sent:
//! the home page's box (`Home`) holds the words and files until then (`draft_send`).
//!
//! A draft is drawn as the window with only the conversation (`render_draft`): no sidebar and no library
//! navigation, the page 助手 (`Page::Ask`, `Talk::Library`), 「← 首页」 at the top left. A Word file handed in
//! is the draft's manuscript, at the right of the conversation with its comments (审稿); the assistant leaves
//! its remarks there with the tool `comment_manuscript`.

use crate::app::{Cmd, Modal, Side, Talk, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::path::{Path, PathBuf};
use wisp_core::libraries;
use wisp_core::settings::Task;
use wisp_core::time;

/// What the home page's box says while it is empty.
const PLACEHOLDER: &str = "有个想法，或者一份稿子？直接说";
/// How wide a column of comments beside a manuscript under review is, and the page it needs beside the other two.
const COMMENTS_W: f32 = 320.;
const ROOM_FOR_COMMENTS: f32 = 1120.;
/// A page narrower than this puts the manuscript over the conversation.
const STACK_UNDER: f32 = 640.;

/// The quiet starts under the box: an icon, the words on the line, and what they put in the box. The second and
/// the fourth pick a Word file first: a manuscript is what they are about.
const STARTS: [(&str, &str, &str, bool); 4] = [
    ("search", "调研一个想法", "帮我调研一下这个想法：", false),
    ("file-text", "审一份稿件", "帮我审一下这份稿件：逻辑、论据和表述，意见作为批注留在稿件上。", true),
    ("library", "找几篇文献", "帮我找几篇文献：", false),
    ("list", "从 Word 建文献库", "把这份 Word 文件参考文献表里的文献都收进来，建成一个文献库。", true),
];
/// The start whose Word file is read for the works it cites (`import_docx_references`), not taken in as a manuscript to review.
const COLLECT: usize = 3;

/// The home page's box: what the first question will be, and the files that go with it.
pub struct Home {
    pub input: Entity<TextInput>,
    /// A Word file to review, a PDF to talk about: taken into the conversation when the question is sent.
    pub files: Vec<PathBuf>,
    /// The box's Word files are for the works they cite (the start 从 Word 建文献库): they go with the question as files
    /// the assistant reads, not as a manuscript to review.
    pub collect: bool,
    /// The conversation just made, and how many of its files are still being taken in: its first question waits for them.
    pub waiting: Option<(String, usize)>,
    /// The box is to have the keyboard when it is next drawn (a file was dropped, which brings no window along).
    pub focus: bool,
    /// The assistant's page is on show in place of the home page: the box, the starts and 最近对话 (`render_start_page`).
    /// No library is open meanwhile; what is in the box stays while the app runs.
    pub open: bool,
    /// Inside a conversation, 历史 lists the other conversations in its place.
    pub listing: bool,
    /// 稍后 on the card that offers 建成文献库: that conversation, and how many papers it held then. The card is back once more come in.
    pub later: Option<(String, usize)>,
    _heard: Subscription,
}

impl Home {
    pub fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Home {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(PLACEHOLDER, cx).multiline(1);
            input.submit_on_enter = true;
            input
        });
        let heard = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| if *event == InputEvent::Submit { this.draft_send(window, cx) });
        Home { input, files: vec![], collect: false, waiting: None, focus: false, open: false, listing: false, later: None, _heard: heard }
    }
}

/// What the home page's assistant and a conversation not yet made a library can be told.
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// The assistant's page with nothing said yet: the home page's corner button, the palette, the shortcut `ask` on home.
    Begin,
    /// 新对话 inside a conversation: out of it, to that page.
    New,
    /// 历史 inside a conversation: the conversations listed in its place, or the conversation again.
    History,
    /// Send what the box holds: the conversation is made here.
    Send,
    /// A start line under the box, by its place in `STARTS`.
    Start(usize),
    /// Pick a Word file or a PDF for the box.
    Pick,
    RemoveFile(usize),
    /// Take a conversation up again from 最近对话.
    Open(String),
    /// The dialog that names it anew.
    Rename(String),
    /// 建成文献库: the new-library dialog, set for this conversation.
    Promote(String),
    /// 稍后 on the card under the conversation that offers 建成文献库.
    Later,
    /// The sparkle by that dialog's name and topic: the model proposes both from the conversation.
    Propose,
    /// The manuscript beside the conversation: shown (稿件 in the conversation's header), or closed (its ×) so the conversation has the page.
    Manuscript(bool),
}

/// Whether a file can start a conversation: a Word file (a manuscript to review) or a PDF (a paper to talk about).
pub fn starts_one(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("docx") || e.eq_ignore_ascii_case("pdf"))
}

impl Workspace {
    pub(crate) fn run_draft(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Begin => {
                self.home.open = true;
                self.home.input.read(cx).focus(window);
            }
            Do::New => {
                self.run(Cmd::GoHome, window, cx);
                self.home.open = true;
                self.home.input.read(cx).focus(window);
            }
            Do::History => self.home.listing = !self.home.listing,
            Do::Send => self.draft_send(window, cx),
            Do::Start(n) => {
                let Some((_, _, words, pick)) = STARTS.get(n) else { return };
                self.home.collect = n == COLLECT;
                if *pick && !self.home.files.iter().any(|path| crate::comments::is_word(path)) { return self.draft_pick(Some(words), cx) }
                self.home.input.update(cx, |input, cx| input.set_text(t(words), cx));
                self.home.input.read(cx).focus(window);
            }
            Do::Pick => self.draft_pick(None, cx),
            Do::RemoveFile(at) => {
                if at < self.home.files.len() { self.home.files.remove(at); }
            }
            Do::Open(id) => {
                self.home.listing = false;
                self.open_library(&id, window, cx);
                if let Some(lib) = self.lib.as_ref().filter(|lib| lib.info.id == id) { lib.library_chat.input.read(cx).focus(window) }
            }
            Do::Rename(id) => {
                let Some(info) = self.libraries.iter().find(|l| l.id == id).cloned() else { return };
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                let name = self.modal_input("对话的名称", &info.name, false, window, cx);
                name.read(cx).focus(window);
                self.modal = Some(Modal::Rename { id, name });
            }
            Do::Promote(id) => {
                // The window is that conversation when the dialog closes: it becomes the library in place.
                if self.lib.as_ref().is_none_or(|lib| lib.info.id != id) { self.open_library(&id, window, cx) }
                if self.lib.as_ref().is_some_and(|lib| lib.info.id == id) { self.run(Cmd::EditLibrary(id), window, cx) }
            }
            Do::Propose => self.draft_propose(cx),
            Do::Later => self.home.later = self.lib.as_ref().map(|lib| (lib.info.id.clone(), lib.papers.len())),
            Do::Manuscript(show) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.writing.away = !show;
                if show { lib.writing.editor.read(cx).focus(window) } else { lib.library_chat.input.read(cx).focus(window) }
            }
        }
        cx.notify();
    }

    /// The conversations of the home page, latest first.
    pub(crate) fn conversations(&self) -> impl Iterator<Item = &wisp_core::libraries::LibraryInfo> {
        self.libraries.iter().filter(|info| info.draft)
    }

    /// Whether the open conversation not yet made a library holds a manuscript with text: one to stand beside it.
    pub(crate) fn draft_manuscript(&self) -> bool {
        self.lib.as_ref().is_some_and(|lib| lib.info.draft && lib.writing.current().is_some_and(|m| m.sections.iter().any(|s| !s.body.trim().is_empty())))
    }

    /// Whether the open library is a conversation not yet made one.
    pub(crate) fn in_draft(&self) -> bool {
        self.lib.as_ref().is_some_and(|lib| lib.info.draft)
    }

    /// Files for the first question: dropped on the home page or picked. A box without words is given the
    /// words that say what the file is for; nothing is sent by itself.
    pub(crate) fn home_attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|path| starts_one(path)).collect();
        if paths.is_empty() { return self.fail(t("首页的助手收 Word 文件（.docx）和 PDF。其他文件请拖进一个文献库。"), cx) }
        let word = paths.iter().any(|path| crate::comments::is_word(path));
        for path in paths {
            if !self.home.files.contains(&path) { self.home.files.push(path) }
        }
        if self.home.input.read(cx).text().trim().is_empty() {
            let words = if word { STARTS[1].2 } else { "这篇文献讲了什么？方法和主要结论是什么？" };
            self.home.input.update(cx, |input, cx| input.set_text(t(words), cx));
        }
        self.home.focus = true;
        self.home.open = true;
        cx.notify();
    }

    fn draft_pick(&mut self, words: Option<&'static str>, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(t("选择 Word 文件或 PDF").into()), extensions: Some(vec!["docx".into(), "pdf".into()]) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            this.update(cx, |this, cx| {
                for path in paths.into_iter().filter(|path| starts_one(path)) {
                    if !this.home.files.contains(&path) { this.home.files.push(path) }
                }
                this.home.focus = true;
                // A start line's words go with the file it picked; words already typed stay.
                if let Some(words) = words.filter(|_| this.home.input.read(cx).text().trim().is_empty()) { this.home.input.update(cx, |input, cx| input.set_text(t(words), cx)) }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The first question: the conversation is made, opened and asked in. Its files are taken in first (a Word
    /// file as the manuscript to review, a PDF as a paper), and the question waits for them.
    pub(crate) fn draft_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.home.input.read(cx).text().trim().to_string();
        let files = self.home.files.clone();
        if text.is_empty() && files.is_empty() { return }
        if let Some(missing) = self.settings.profile_for_or_default(Task::Chat).missing() {
            return self.fail(tf!("提问需要一个模型服务（{}）。在设置 › AI 里配置后再问。", "Asking needs a model service ({}). Set one up in Settings › AI first.", t(missing)), cx);
        }
        let (words, papers): (Vec<PathBuf>, Vec<PathBuf>) = files.iter().cloned().partition(|path| crate::comments::is_word(path));
        // Under 从 Word 建文献库 the Word files are read for what they cite: the conversation is named after the first of them.
        let (cited, words) = if std::mem::take(&mut self.home.collect) { (words, vec![]) } else { (vec![], words) };
        let stem = |path: &PathBuf| path.file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default();
        let about = match cited.first() {
            Some(path) => tf!("{} 的参考文献", "References of {}", stem(path)),
            None if text.is_empty() => stem(&files[0]),
            None => text.clone(),
        };
        let info = match libraries::create_draft(&self.root, &about) {
            Ok(info) => info,
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        self.libraries = libraries::list(&self.root);
        self.open_library(&info.id, window, cx);
        let Some(lib) = self.lib.as_mut().filter(|lib| lib.info.id == info.id) else { return };
        lib.library_chat.input.update(cx, |input, cx| input.set_text(&text, cx));
        lib.library_chat.input.read(cx).focus(window);
        lib.library_chat.documents = cited;
        self.home.input.update(cx, |input, cx| input.set_text("", cx));
        self.home.files.clear();
        if words.is_empty() && papers.is_empty() { return self.ask_library(cx) }
        self.home.waiting = Some((info.id.clone(), words.len() + papers.len()));
        if !words.is_empty() { self.import_manuscript_files(words, cx) }
        for path in papers { self.draft_take_pdf(info.id.clone(), path, cx) }
        cx.notify();
    }

    /// A PDF of the first question becomes the conversation's paper, read off the main thread as an import reads it.
    fn draft_take_pdf(&mut self, library: String, path: PathBuf, cx: &mut Context<Self>) {
        let file = path.clone();
        let work = cx.background_executor().spawn(async move {
            let pages = wisp_core::fulltext::file_pages(&file);
            let doi = pages.as_ref().ok().and_then(|pages| wisp_core::lookup::find_doi(&pages.iter().take(2).cloned().collect::<Vec<_>>().join("\n")));
            let found = doi.and_then(|doi| wisp_net::lookup_paper(&wisp_core::lookup::Identifier::Doi(doi)).ok());
            (pages, found)
        });
        cx.spawn(async move |this, cx| {
            let (pages, found) = work.await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|lib| lib.info.id != library) { return }
                this.add_pdf(&path, &[], pages, found, cx);
                this.draft_file_done(&library, cx);
            })
            .ok();
        })
        .detach();
    }

    /// One file of the first question is in (or could not be): once all are, the question goes.
    pub(crate) fn draft_file_done(&mut self, library: &str, cx: &mut Context<Self>) {
        let Some((_, left)) = self.home.waiting.as_mut().filter(|(id, _)| id == library) else { return };
        *left = left.saturating_sub(1);
        if *left > 0 { return cx.notify() }
        self.home.waiting = None;
        if self.lib.as_ref().is_some_and(|lib| lib.info.id == library) { self.ask_library(cx) }
        cx.notify();
    }

    /// The sparkle of 建成文献库: `Task::Chat`'s model proposes a name and a topic from what was said.
    fn draft_propose(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(Modal::Library { editing: Some(id), proposing, .. }) = self.modal.as_mut() else { return };
        if *proposing || *id != lib.info.id { return }
        let said: String = lib.library_chat.messages.iter().filter_map(|item| match item {
            crate::app::ChatItem::User { text, .. } => Some(format!("问：{text}")),
            crate::app::ChatItem::Answer(text) if !text.is_empty() => Some(format!("答：{text}")),
            _ => None,
        }).collect::<Vec<_>>().join("\n\n");
        let titles: Vec<String> = lib.papers.iter().take(30).map(|paper| format!("- {}", paper.title)).collect();
        let said = if titles.is_empty() { said } else { format!("{said}\n\n收进的文献：\n{}", titles.join("\n")) };
        if said.trim().is_empty() { return self.modal_error = t("还没有可依据的对话。").into() }
        let (system, prompt) = libraries::proposal_prompt(&said);
        let request = match wisp_core::ai::plain_request(&profile, &system, &prompt) {
            Ok(request) => request,
            Err(err) => return self.modal_error = err,
        };
        *proposing = true;
        self.modal_error.clear();
        let (library, protocol) = (lib.info.id.clone(), profile.protocol);
        let work = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol).and_then(|reply| libraries::parse_proposal(&reply)) });
        cx.spawn(async move |this, cx| {
            let proposed = work.await;
            this.update(cx, |this, cx| {
                // Another conversation, or the dialog closed meanwhile: the proposal is for nobody.
                if this.lib.as_ref().is_none_or(|lib| lib.info.id != library) { return }
                let Some(Modal::Library { editing: Some(id), name, topic, proposing, .. }) = this.modal.as_mut() else { return };
                if *id != library { return }
                *proposing = false;
                match proposed {
                    Ok((proposed_name, proposed_topic)) => {
                        let (name, topic) = (name.clone(), topic.clone());
                        name.update(cx, |input, cx| input.replace_range(0..input.text().len(), &proposed_name, cx));
                        topic.update(cx, |input, cx| input.replace_range(0..input.text().len(), &proposed_topic, cx));
                    }
                    Err(err) => this.modal_error = err,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- the assistant's page, before anything is said

    /// The whole window, one level under the home page: 「← 首页」 at the top left, and in the reading column the box of
    /// the first question, the three quiet starts and 最近对话.
    pub(crate) fn render_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let composer = self.render_home_assistant(window, cx);
        let rows = self.recent_rows(cx);
        let recent = (!rows.is_empty()).then(|| div().flex().flex_col().gap(px(2.)).child(ui::caption(t("最近对话")).pb(px(4.))).child(div().flex().flex_col().ml(px(-8.)).mr(px(-8.)).children(rows)));
        let back = ui::title_strip()
            .when(!ui::custom_titlebar(), |d| d.h(text_px(28.)))
            .pl(px(10. + ui::traffic_lights_width()))
            .child(ui::ghost_button("back-home", Some("arrow-left"), Some(t("首页").into())).occlude().on_click(self.on(Cmd::GoHome, cx)));
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(Theme::bg_body())
            .child(back.mt(px(Self::corner_top())))
            .child(
                div()
                    .id("start-page")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .pt(px(96.))
                    .pb(px(32.))
                    .child(crate::chat::reading_column(div().flex().flex_col().gap(px(36.)).child(composer).children(recent), 0.)),
            )
            .into_any_element()
    }

    /// The box of a first question, and the quiet starts under it.
    fn render_home_assistant(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if std::mem::take(&mut self.home.focus) { self.home.input.read(cx).focus(window) }
        let input = &self.home.input;
        let focused = input.focus_handle(cx).is_focused(window);
        let ready = !input.read(cx).text().trim().is_empty() || !self.home.files.is_empty();
        let model = self.settings.profile_for_or_default(Task::Chat).title();
        let send = div().id("home-send").flex().flex_none().items_center().justify_center().size(px(30.)).rounded(px(15.)).bg(if ready { Theme::accent() } else { Theme::groove() }).cursor_pointer().hover(|s| s.opacity(0.88)).child(ui::icon("send", 15., if ready { Theme::on_accent() } else { Theme::text_tertiary() })).on_click(self.on(Cmd::Draft(Do::Send), cx));
        let card = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(8.))
            .rounded(px(16.))
            .border_1()
            .border_color(if focused { Theme::accent() } else { Theme::line_strong() })
            .bg(Theme::bg_body())
            .shadow(theme::shadow_card())
            .when(!self.home.files.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(6.)).px(px(2.)).pt(px(2.)).children(self.home.files.iter().enumerate().map(|(n, path)| {
                    crate::chat::document_chip(&path.display().to_string()).pr(px(2.)).child(ui::ghost_button(SharedString::from(format!("home-file-remove-{n}")), Some("x"), None).h(text_px(20.)).min_w(px(20.)).on_click(self.on(Cmd::Draft(Do::RemoveFile(n)), cx)))
                })))
            })
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(6.))
                    .child(ui::ghost_button("home-attach", Some("plus"), None).h(text_px(30.)).min_w(px(30.)).rounded(px(15.)).tooltip(ui::tip(t("添加 Word 文件或 PDF"))).on_click(self.on(Cmd::Draft(Do::Pick), cx)))
                    .child(div().flex().flex_1().min_w_0().py(px(5.)).max_h(px(170.)).overflow_hidden().text_size(text_px(13.)).line_height(text_px(20.)).text_color(Theme::text()).child(input.clone()))
                    .child(send),
            );
        let starts = STARTS.iter().enumerate().map(|(n, (icon, label, ..))| {
            div().id(("home-start", n)).flex().flex_none().items_center().gap(px(6.)).h(text_px(28.)).px(px(8.)).rounded(px(6.)).text_size(text_px(12.)).text_color(Theme::text_secondary()).cursor_pointer().hover(|s| s.bg(Theme::hover()).text_color(Theme::text())).active(|s| s.bg(Theme::pressed())).on_click(self.on(Cmd::Draft(Do::Start(n)), cx)).child(ui::icon(icon, 13., Theme::text_tertiary())).child(t(label))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(card)
            .child(div().flex().items_center().gap(px(8.)).px(px(4.)).text_size(text_px(11.)).text_color(Theme::text_placeholder()).child(div().flex_1().min_w_0().truncate().child(t("Enter 发送，Shift+Enter 换行 · 回答可能有误，请对照原文"))).child(div().flex_none().max_w(relative(0.4)).truncate().child(model)))
            // The starts stand where the box's text begins, not at the card's rounded edge.
            .child(div().flex().flex_wrap().items_center().gap(px(2.)).ml(px(-8.)).children(starts))
    }

    /// A library left while one of its conversations was answering (`Workspace::parked`): a mark for its row or card that says
    /// it is still at work, or waits for the person's yes to a tool. None for one not answering off screen.
    pub(crate) fn off_screen_mark(&self, id: &str) -> Option<Div> {
        let lib = self.parked.iter().find(|lib| lib.info.id == id)?;
        Some(Self::at_work_mark(SharedString::from(format!("off-screen-{id}")), lib.waits_for_yes()))
    }

    /// 回答中 with a spinner, or 等你同意: said of a conversation at work that is not on show.
    pub(crate) fn at_work_mark(id: SharedString, waits: bool) -> Div {
        let mark = div().flex().flex_none().items_center().gap(px(5.)).text_size(text_px(12.));
        if waits {
            mark.text_color(Theme::accent()).child(ui::icon("info", 12., Theme::accent())).child(t("等你同意"))
        } else {
            mark.text_color(Theme::text_secondary()).child(ui::spinner(id, 11., Theme::accent())).child(t("回答中"))
        }
    }

    /// The conversations, latest first, as plain rows: the title, at the right the papers kept (when there are any) and
    /// the date; a right click renames, makes a library or removes. On the assistant's page under 最近对话, and in a
    /// conversation's 历史.
    pub(crate) fn recent_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let open = self.lib.as_ref().map(|lib| lib.info.id.clone());
        self.conversations()
            .map(|info| {
                let here = open.as_deref() == Some(info.id.as_str());
                div()
                    .id(SharedString::from(format!("conversation-{}", info.id)))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .h(text_px(32.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(here, |d| d.bg(Theme::accent_soft()))
                    .when(!here, |d| d.hover(|s| s.bg(Theme::hover())))
                    .on_click(self.on(Cmd::Draft(Do::Open(info.id.clone())), cx))
                    .on_mouse_down(MouseButton::Right, self.on_context(crate::app::MenuKind::Draft(info.id.clone()), cx))
                    .child(ui::icon("message-square", 14., Theme::text_tertiary()))
                    .child(div().flex_1().min_w_0().truncate().child(ui::one_line(&info.name)))
                    // Left while it was answering, it goes on: said here until the answer is in.
                    .children(self.off_screen_mark(&info.id))
                    .when(info.paper_count > 0, |d| d.child(div().flex_none().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(tf!("{} 篇", "{} papers", info.paper_count))))
                    .child(div().flex_none().w(px(84.)).text_right().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(time::date_text(&info.opened_at)))
                    .into_any_element()
            })
            .collect()
    }

    /// 历史 inside a conversation: the conversations in its place, in the reading column.
    pub(crate) fn render_draft_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.recent_rows(cx);
        div().id("draft-history").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().px(px(16.)).py(px(14.)).child(crate::chat::reading_column(div().flex().flex_col().ml(px(-8.)).mr(px(-8.)).children(rows), 0.)).into_any_element()
    }

    // ---- a conversation not yet made a library

    /// The page of a conversation not yet made a library: the conversation alone, or, once a manuscript was
    /// handed in, the conversation with the manuscript at its right and, where the page has room, its comments.
    pub(crate) fn render_draft(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // A manuscript closed with its × leaves the conversation the page; 稿件 in the conversation's header brings it back.
        let reviewing = self.draft_manuscript() && self.lib.as_ref().is_some_and(|lib| !lib.writing.away);
        if !reviewing { return self.render_chat(Talk::Library, window, cx) }
        let comments_width = if width >= ROOM_FOR_COMMENTS { COMMENTS_W } else { 0. };
        let rest = width - comments_width;
        let stacked = rest < STACK_UNDER;
        let manuscript_width = if stacked { rest } else { self.agent_manuscript_width(rest) };
        let chat = self.render_chat(Talk::Library, window, cx);
        let manuscript = self.render_manuscript_column(manuscript_width, true, stacked, window, cx);
        // Without room for the column, a comment pointed at (a marker clicked, `comment_point`) opens the comments as a
        // drawer over the manuscript's right edge; its close button, or another marker, says when.
        let drawer = comments_width == 0. && self.lib.as_ref().is_some_and(|lib| lib.writing.side == Side::Comments && lib.writing.side_open && lib.writing.comments.focused.is_some());
        let drawer_width = COMMENTS_W.min(manuscript_width - 40.);
        let comments = if comments_width > 0. { Some(self.render_draft_comments(comments_width, false, window, cx)) } else if drawer { Some(self.render_draft_comments(drawer_width, true, window, cx)) } else { None };
        // The column stands beside the manuscript; the drawer over it.
        let (over, beside) = if drawer { (comments.map(|drawer| drawer.absolute().top_0().right_0().h_full()), None) } else { (None, comments) };
        if stacked {
            return div().id("draft-page").relative().flex().flex_col().size_full()
                .child(div().flex().flex_none().w(px(rest)).h(relative(0.46)).overflow_hidden().border_b_1().border_color(Theme::line()).child(manuscript))
                .child(div().flex_1().min_h_0().w(px(rest)).overflow_hidden().child(chat))
                .children(over)
                .into_any_element();
        }
        div()
            .id("draft-page")
            .flex()
            .size_full()
            // Widths in pixels for all: a conversation that shares the row is measured wider than it is drawn.
            .child(div().flex_none().w(px(rest - manuscript_width)).h_full().overflow_hidden().child(chat))
            .child(div().relative().flex().flex_none().w(px(manuscript_width)).h_full().border_l_1().border_color(Theme::line()).child(ui::strip_cap(Theme::bg_body())).child(manuscript).children(over))
            .children(beside)
            .into_any_element()
    }

    /// The comments of the manuscript under review: a column of the page, or (`drawer`) the same over the manuscript, with a way to close it.
    fn render_draft_comments(&self, width: f32, drawer: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let waiting = self.lib.as_ref().map_or(0, |lib| lib.writing.comments.list.iter().filter(|c| c.state != wisp_core::comment::DONE).count());
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .border_l_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .when(!drawer, |d| d.child(ui::strip_cap(Theme::bg_sunken())))
            // Floating over the manuscript it is lifted off it, and hears nothing through it.
            .when(drawer, |d| d.occlude().shadow(theme::shadow_panel()))
            .child(
                ui::column_head()
                    .px(px(14.))
                    .child(div().flex_1().min_w_0().truncate().text_size(text_px(ui::HEAD_TITLE)).font_weight(FontWeight::SEMIBOLD).child(if waiting > 0 { tf!("批注 {waiting}", "Comments {waiting}") } else { t("批注").to_string() }))
                    .when(drawer, |d| d.child(ui::ghost_button("draft-comments-close", Some("x"), None).tooltip(ui::tip(t("收起批注"))).on_click(self.on(Cmd::WriteSide(Side::Comments), cx)))),
            )
            .child(self.render_comments(width, window, cx))
    }

    /// What a draft's conversation shows before anything is said in it: its files still being taken in, or a line on what it is.
    pub(crate) fn render_draft_start(&self) -> AnyElement {
        let waiting = self.lib.as_ref().is_some_and(|lib| self.home.waiting.as_ref().is_some_and(|(id, _)| *id == lib.info.id));
        if waiting {
            return div().flex().flex_1().items_center().justify_center().gap(px(8.)).text_color(Theme::text_secondary()).child(ui::spinner("draft-taking", 14., Theme::accent())).child(t("正在读入文件，读完就把问题交给助手…")).into_any_element();
        }
        ui::empty_state("sparkle", t("说说你的想法"), t("还没有文献库也可以：助手会到 PubMed、Europe PMC 等文献数据库里找文献。拖进一份 Word 稿件，它会把审稿意见作为批注留在稿件上。聊出眉目后，点上方的「建成文献库」。")).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::starts_one;

    #[test]
    fn a_conversation_starts_from_a_word_file_or_a_pdf() {
        let dir = std::env::temp_dir().join(format!("wisp-draft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.docx", "b.PDF", "c.md"] { std::fs::write(dir.join(name), b"x").unwrap() }
        assert!(starts_one(&dir.join("a.docx")) && starts_one(&dir.join("b.PDF")));
        assert!(!starts_one(&dir.join("c.md")) && !starts_one(&dir.join("missing.pdf")) && !starts_one(&dir));
        std::fs::remove_dir_all(dir).ok();
    }
}
