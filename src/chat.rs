//! 提问: the conversation with the library (the current list) or with the
//! selected paper. The answer cites labels like [P3 p.5]; the chips under
//! it open the paper, or its PDF at that page. Between the pieces of an
//! answer stand the tools the assistant called to find it, folded into one
//! line once they are done. Under the conversation is the box the next
//! question is written in, with its pictures; over it, while a slash
//! command is being typed, the commands that match. The history lists the
//! library's past conversations in place of the one in hand.

use crate::app::{Chat, ChatImage, ChatItem, Cmd, Consent, MenuKind, Workspace};
use crate::settings::SettingsPage;
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::{ai, time};

/// The sources shown under an answer before the rest is asked for.
const SOURCES_SHOWN: usize = 8;

/// The shell of a chip under an answer: a source, or the one that shows the rest.
fn source_chip(id: SharedString, max_width: f32) -> Stateful<Div> {
    // In a narrow column a chip gives way rather than run out of it.
    div().id(id).flex().items_center().gap(px(4.)).min_w_0().max_w(px(max_width)).h(px(24.)).px(px(8.)).rounded(px(6.)).border_1().border_color(Theme::line()).text_size(px(12.)).cursor_pointer().hover(|s| s.bg(Theme::hover()).border_color(Theme::accent()))
}

/// A picture of a conversation, cropped to a square.
fn thumbnail(picture: &ChatImage, size: f32) -> Div {
    div().size(px(size)).flex_none().rounded(px(10.)).border_1().border_color(Theme::line()).overflow_hidden().child(img(picture.image.clone()).size_full().object_fit(ObjectFit::Cover))
}

/// How long an answer has been awaited: `8s`, `1m 05s`.
fn elapsed(chat: &Chat) -> String {
    let seconds = chat.started.map_or(0, |at| at.elapsed().as_secs());
    if seconds < 60 { format!("{seconds}s") } else { format!("{}m {:02}s", seconds / 60, seconds % 60) }
}

/// The tools of a run of calls in a few words: 「检索全文 ×2 · 读取全文」.
fn tools_summary(calls: &[&str]) -> String {
    let mut counted: Vec<(&str, usize)> = vec![];
    for call in calls {
        // A call is worded 「检索全文：chromium」; what stands before the colon is the tool.
        let title = call.split('：').next().unwrap_or(call);
        match counted.iter_mut().find(|(known, _)| *known == title) {
            Some((_, count)) => *count += 1,
            None => counted.push((title, 1)),
        }
    }
    counted.into_iter().map(|(title, count)| if count > 1 { format!("{title} ×{count}") } else { title.to_string() }).collect::<Vec<_>>().join(" · ")
}

impl Workspace {
    pub fn render_chat(&mut self, about_paper: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(lib) = self.lib.as_mut() { lib.check_quotes(about_paper) }
        let lib = self.lib.as_ref().unwrap();
        let chat = lib.chat(about_paper);
        let profile = self.settings.profile_for_or_default(wisp_core::settings::Task::Chat);
        let configured = !profile.key().is_empty();
        let model = profile.title();
        // Ids of the two conversations' elements must differ: both can be on screen.
        let side = if about_paper { "paper" } else { "library" };

        // The papers in scope that have a PDF no text came of: said, so that nobody takes them for read.
        let textless = lib.shown.iter().filter(|&&i| lib.papers[i].fulltext == "textless").count();
        let textless_note = if textless > 0 && !about_paper { format!("；其中 {textless} 篇的 PDF 没读出文字，只有记录和摘要") } else { String::new() };
        // What the model has been given, or will be given with the first question.
        let scope = match &chat.ctx {
            Some(ctx) if about_paper => {
                let mut line = if ctx.pages > 0 { format!("已提供这篇文献的记录和 {} 页全文", ctx.pages) } else { "这篇文献没有可读取的全文，只提供了记录和摘要".to_string() };
                if ctx.pages_omitted > 0 { line.push_str(&format!("（另有 {} 页超出长度未提供）", ctx.pages_omitted)) }
                line
            }
            Some(ctx) => {
                let mut line = format!("已提供 {} 篇文献的记录", ctx.full);
                if ctx.titles_only > 0 { line.push_str(&format!("，{} 篇仅题目", ctx.titles_only)) }
                if ctx.omitted > 0 { line.push_str(&format!("，{} 篇未提供", ctx.omitted)) }
                if ctx.pages > 0 { line.push_str(&format!("，{} 页相关全文", ctx.pages)) }
                line + &textless_note
            }
            None if !chat.messages.is_empty() => "历史对话：接着提问时，会按文献库现在的内容重新提供材料".to_string(),
            None if about_paper => {
                let id = lib.selected.clone().unwrap_or_default();
                match lib.pdf_file(&id).map(|f| lib.store.fulltext_pages(&f.id)) {
                    Some(Ok(Some(Ok(pages)))) => format!("将提供这篇文献的记录和 {} 页全文", pages.len()),
                    Some(Ok(Some(Err(error)))) => format!("全文无法读取（{error}），将只提供记录和摘要"),
                    Some(_) => "全文的文字还在提取，现在提问只会用到记录和摘要".to_string(),
                    None => "这篇文献没有 PDF，将只提供记录和摘要".to_string(),
                }
            }
            None => format!("范围：当前列表的 {} 篇文献（受左侧视图、搜索和筛选影响），以及其中已存 PDF 的相关页面{textless_note}", lib.shown.len()),
        };

        // What was asked before can be read without a model; only asking needs one.
        let body = if let Some(history) = &chat.history {
            self.render_history(about_paper, chat, history, cx)
        } else if !configured && chat.messages.is_empty() {
            ui::empty_state("sparkle", "还没有配置模型", "提问需要一个模型服务：Claude API，或任何 OpenAI 兼容的服务。API Key 只保存在本机。以前的对话在右上角的「历史」里，不配置也能看。").child(ui::primary_button(SharedString::from(format!("chat-settings-{side}")), "打开设置").on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))).into_any_element()
        } else if chat.messages.is_empty() && chat.error.is_empty() {
            let suggestions: &[&str] = if about_paper { &["这篇文献的研究问题、方法和主要结论是什么？", "作者的结论有哪些证据支持？有什么局限？", "用三句话向同行介绍这篇文章。"] } else { &["这些文献主要在研究哪些问题？按主题归类。", "哪些文献的结论相互支持或矛盾？", "我还没读的文献里，哪几篇最值得先读？为什么？"] };
            ui::empty_state("sparkle", if about_paper { "就这篇文献提问" } else { "向文献库提问" }, "回答依据文献库里的记录、笔记和已存 PDF 的全文，并标出出处。需要时助手会自己检索和翻阅全文。")
                .children(suggestions.iter().enumerate().map(|(i, text)| ui::secondary_button(SharedString::from(format!("suggest-{side}-{i}")), *text).h(px(30.)).on_click(self.on(Cmd::Suggest(about_paper, text.to_string()), cx))))
                .into_any_element()
        } else {
            div().id(SharedString::from(format!("chat-{side}"))).flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&chat.scroll).px(px(16.)).py(px(14.)).gap(px(14.)).children(self.render_messages(about_paper, chat, cx)).into_any_element()
        };

        let listing = chat.history.is_some();
        // The papers behind the scope line can be looked through: in a conversation whose material was built, or before the first question.
        let listable = !about_paper && !listing && (chat.ctx.is_some() || chat.messages.is_empty());
        let materials = (listable && chat.materials_open).then(|| self.render_materials(chat, cx));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .px(px(16.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .when(!about_paper, |d| d.child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(if listing { "文献库的历史对话" } else { "向文献库提问" })))
                            // The paper's panel has its title above; its history says whose it is.
                            .when(about_paper && listing, |d| d.child(div().font_weight(FontWeight::MEDIUM).child("这篇文献的历史对话")))
                            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(match (listing, about_paper) {
                                (true, false) => "向整个文献库提过的问题和回答；点开一次对话可以接着问。单篇文献的对话在各自的「提问」里".to_string(),
                                (true, true) => "就这篇文献提过的问题和回答；点开一次对话可以接着问".to_string(),
                                (false, _) => scope,
                            })),
                    )
                    .children(self.render_float_switch(about_paper, window, cx))
                    .when(listable, |d| d.child(ui::ghost_button("chat-materials", Some(if chat.materials_open { "chevron-up" } else { "list" }), Some(if chat.materials_open { "收起".into() } else { "文献".into() })).on_click(self.on(Cmd::ToggleMaterials, cx))))
                    .child(ui::ghost_button(SharedString::from(format!("chat-history-{side}")), Some(if listing { "arrow-left" } else { "history" }), Some(if listing { "返回".into() } else { "历史".into() })).on_click(self.on(Cmd::ToggleHistory(about_paper), cx)))
                    .when(!chat.messages.is_empty() && !listing, |d| d.child(ui::ghost_button(SharedString::from(format!("chat-new-{side}")), Some("plus"), Some("新对话".into())).on_click(self.on(Cmd::NewChat(about_paper), cx)))),
            )
            .children(materials)
            // A short panel (the floating window) scrolls what is above the box rather than push the box out of sight.
            .child(div().id(SharedString::from(format!("chat-body-{side}"))).flex().flex_col().flex_1().min_h_0().overflow_y_scroll().child(body))
            .when(configured, |d| d.child(self.render_composer(about_paper, chat, &model, window, cx)))
            // An old conversation on show without a model: it can be read, and says what asking on needs.
            .when(!configured && !listing && !chat.messages.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .px(px(16.))
                        .py(px(10.))
                        .border_t_1()
                        .border_color(Theme::line())
                        .text_size(px(12.))
                        .text_color(Theme::text_tertiary())
                        .child(div().flex_1().min_w_0().child("接着问需要一个模型服务。"))
                        .child(ui::secondary_button(SharedString::from(format!("chat-settings-{side}")), "打开设置").h(px(28.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))),
                )
            })
            .into_any_element()
    }

    /// The papers the library's questions are put to, to look through
    /// without leaving the page: with the labels of this conversation and
    /// what each was handed over as once the material is built; the papers
    /// of the current list before that. A row opens the paper beside the
    /// conversation; the scope stays what it is.
    fn render_materials(&self, chat: &Chat, cx: &mut Context<Self>) -> AnyElement {
        /// The rows drawn; a library of thousands is not a list to scroll through here.
        const LISTED: usize = 300;
        let lib = self.lib.as_ref().unwrap();
        // (paper, its label in this conversation, how its record was given, pages of its text given)
        let rows: Vec<(&wisp_core::model::Paper, String, &str, usize)> = match &chat.ctx {
            Some(ctx) => ctx.given.iter().filter_map(|given| Some((lib.paper(&given.paper_id)?, ctx.sources.iter().find(|source| source.paper_id == given.paper_id).map(|source| source.label.clone()).unwrap_or_default(), given.record, given.pages))).collect(),
            None => lib.shown.iter().map(|&i| (&lib.papers[i], String::new(), "", 0)).collect(),
        };
        let total = rows.len();
        let note = if chat.ctx.is_some() {
            "这次对话开始时交给模型的材料。助手还能用工具查到库里的其他文献；在这里点开一篇不会改变范围。"
        } else {
            "第一个问题会带上这些文献：范围就是文献列表现在的样子，由视图、搜索和筛选决定。点开一篇不会改变范围。"
        };
        let listed = rows.into_iter().take(LISTED).enumerate().map(|(n, (paper, label, record, pages))| {
            let by = [paper.authors.split([';', ',']).next().unwrap_or("").trim().to_string(), paper.year.map(|year| year.to_string()).unwrap_or_default()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
            // What the model has of it: the record, and how much of the text.
            let mut state = vec![];
            match record {
                "full" => state.push("完整记录".to_string()),
                "title" => state.push("仅题目".to_string()),
                "omitted" => state.push("未提供".to_string()),
                _ => {}
            }
            if pages > 0 { state.push(format!("{pages} 页全文")) }
            let (text, color) = match paper.fulltext.as_str() {
                "textless" => ("PDF 无文字", Theme::warning()),
                "stored" => ("有全文", Theme::text_tertiary()),
                _ => ("无全文", Theme::text_placeholder()),
            };
            let selected = lib.selected.as_deref() == Some(paper.id.as_str());
            div()
                .id(("material", n))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(selected, |d| d.bg(Theme::accent_soft()))
                .when(!selected, |d| d.hover(|s| s.bg(Theme::hover())))
                .on_click(self.on(Cmd::Select(paper.id.clone()), cx))
                .when(!label.is_empty(), |d| d.child(div().flex_none().w(px(34.)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::accent()).child(label)))
                .child(div().flex_1().min_w_0().truncate().child(paper.title.clone()))
                .when(!by.is_empty(), |d| d.child(div().flex_none().max_w(px(150.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(by)))
                .when(!state.is_empty(), |d| d.child(div().flex_none().text_size(px(12.)).text_color(if record == "full" { Theme::text_secondary() } else { Theme::warning() }).child(state.join(" · "))))
                .child(div().flex_none().w(px(68.)).text_right().text_size(px(12.)).text_color(color).child(text))
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .max_h(relative(0.45))
            .border_b_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .child(div().flex_none().px(px(16.)).pt(px(8.)).pb(px(4.)).text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(note))
            .child(
                div()
                    .id("materials")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(8.))
                    .children(listed)
                    .when(total > LISTED, |d| d.child(div().px(px(8.)).py(px(6.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(format!("另有 {} 篇没有列出；在文献列表里可以看到全部。", total - LISTED)))),
            )
            .into_any_element()
    }

    /// What was said, in order: questions, the assistant's tool calls
    /// folded by run, the pieces of its answers with their sources.
    fn render_messages(&self, about_paper: bool, chat: &Chat, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let side = if about_paper { "paper" } else { "library" };
        let mut items: Vec<AnyElement> = vec![];
        // A citation clicked where it stands in the text.
        let workspace = cx.entity().downgrade();
        let cite: crate::markdown::OnCite = std::rc::Rc::new(move |citation, window, cx| {
            workspace.update(cx, |this, cx| this.run(Cmd::GotoCited(about_paper, citation.label.clone(), citation.page, citation.quote.clone()), window, cx)).ok();
        });
        let picker = cx.entity().downgrade();
        let mut i = 0;
        while i < chat.messages.len() {
            let content = match &chat.messages[i] {
                ChatItem::User { text, images, .. } => {
                    items.push(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap(px(6.))
                            .when(!images.is_empty(), |d| d.child(div().flex().flex_wrap().justify_end().gap(px(6.)).max_w(relative(0.85)).children(images.iter().map(|picture| thumbnail(picture, 96.)))))
                            .when(!text.is_empty(), |d| d.child(self.selectable(format!("asked-{side}-{i}"), text.clone(), cx).max_w(relative(0.85)).px(px(14.)).py(px(9.)).rounded(px(16.)).bg(Theme::groove()).line_height(px(21.))))
                            .into_any_element(),
                    );
                    i += 1;
                    continue;
                }
                // What the assistant did between two pieces of its answer: one line for the run of calls, the calls themselves on request.
                ChatItem::Tool { .. } => {
                    let first = i;
                    let mut run: Vec<(&str, &Option<(String, bool)>)> = vec![];
                    while let Some(ChatItem::Tool { call, result }) = chat.messages.get(i) {
                        run.push((call.as_str(), result));
                        i += 1;
                    }
                    items.push(self.render_tools(about_paper, chat, first, &run, cx));
                    continue;
                }
                ChatItem::Answer(text) => text,
            };
            // The last piece, still without text: the model is reading or thinking.
            if content.is_empty() {
                items.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(22.))
                        .text_color(Theme::text_tertiary())
                        .child(ui::spinner(SharedString::from(format!("thinking-{side}")), 14., Theme::accent()))
                        .child(if chat.steps > 1 { "正在整理查到的内容…" } else { "正在阅读文献并思考…" })
                        .child(div().text_size(px(12.)).text_color(Theme::text_placeholder()).child(elapsed(chat)))
                        .into_any_element(),
                );
                i += 1;
                continue;
            }
            // What is selected in this answer, and the pointer's way of saying where it is.
            let picked = chat.picked_range().filter(|(at, ..)| *at == i).map(|(_, from, to)| (from, to));
            let holding = chat.picked.is_some_and(|(.., held)| held);
            let pick = picker.clone();
            let selecting = crate::markdown::Selecting {
                selected: picked,
                pointer: std::rc::Rc::new(move |pointer, place, window, cx| {
                    pick.update(cx, |this, cx| {
                        this.run(Cmd::PickText(about_paper, i, pointer, place), window, cx);
                        cx.notify();
                    })
                    .ok();
                }),
            };
            // Each cited label, once, in the order the answer uses it.
            let cited: Vec<(&ai::Source, Option<usize>, String)> = ai::citations(content).into_iter().filter_map(|cited| chat.sources().iter().find(|s| s.label == cited.label).map(|s| (s, cited.page, cited.quote))).collect();
            // An answer that surveys the library cites dozens of papers: the first few are shown, the rest on request.
            let (total, open) = (cited.len(), chat.sources_open.contains(&i));
            let shown = if open || total <= SOURCES_SHOWN + 2 { total } else { SOURCES_SHOWN };
            let mut chips: Vec<AnyElement> = cited
                .into_iter()
                .take(shown)
                .enumerate()
                .map(|(n, (source, page, quote))| {
                    let text = match page {
                        // A quoted sentence is what the click will look for in the PDF.
                        _ if !quote.is_empty() => format!("{}{} · “{quote}”", source.label, page.map(|page| format!(" p.{page}")).unwrap_or_default()),
                        Some(page) => format!("{} p.{page} · {}", source.label, source.title),
                        None => format!("{} · {}", source.label, source.title),
                    };
                    // What looking the quotation up in the paper's text came to: said on the chip, not left to trust.
                    let check = chat.checks.get(&(source.label.clone(), page, quote.clone()));
                    let (mark, verdict, color) = match check {
                        Some(ai::Quoted::Found) => ("check", "原文一致".to_string(), Theme::success()),
                        Some(ai::Quoted::Elsewhere(at)) => ("info", format!("原文在 p.{at}"), Theme::warning()),
                        Some(ai::Quoted::Begins(_, share)) => ("info", format!("只对上开头的 {:.0}%", share * 100.), Theme::warning()),
                        Some(ai::Quoted::Missing) => ("x", "原文里没有这句".to_string(), Theme::danger()),
                        Some(ai::Quoted::NoText) => ("info", "没有全文可核对".to_string(), Theme::text_tertiary()),
                        None => ("file-text", String::new(), Theme::accent()),
                    };
                    // The click leads to where the sentence really is.
                    let page = match check {
                        Some(ai::Quoted::Elsewhere(at) | ai::Quoted::Begins(at, _)) => Some(*at),
                        _ => page,
                    };
                    source_chip(SharedString::from(format!("cite-{side}-{i}-{n}")), if quote.is_empty() { 300. } else { 520. })
                        .text_color(Theme::text_secondary())
                        .on_click(self.on(Cmd::GotoSource(source.paper_id.clone(), page, quote), cx))
                        .child(ui::icon(mark, 12., color))
                        .child(div().min_w_0().truncate().child(text))
                        .when(!verdict.is_empty(), |d| d.child(div().flex_none().text_color(color).child(verdict)))
                        .into_any_element()
                })
                .collect();
            if shown < total || open {
                let label = if open { "收起".to_string() } else { format!("另外 {} 个出处", total - shown) };
                chips.push(source_chip(SharedString::from(format!("cite-more-{side}-{i}")), 300.).text_color(Theme::accent()).on_click(self.on(Cmd::ToggleSources(about_paper, i), cx)).child(label).into_any_element());
            }
            // Under the last piece of a finished answer: a button that copies the whole of it, pieces between tool calls included.
            let last = i + 1 == chat.messages.len();
            let whole = ((last && !chat.busy) || matches!(chat.messages.get(i + 1), Some(ChatItem::User { .. }))).then(|| {
                let from = chat.messages[..i].iter().rposition(|item| matches!(item, ChatItem::User { .. })).map_or(0, |at| at + 1);
                let question = match from.checked_sub(1).and_then(|at| chat.messages.get(at)) {
                    Some(ChatItem::User { text, .. }) => text.clone(),
                    _ => String::new(),
                };
                (question, chat.messages[from..=i].iter().filter_map(|item| if let ChatItem::Answer(text) = item { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n"))
            });
            items.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(crate::markdown::render(&format!("answer-{side}-{i}"), content, Some(cite.clone()), Some(selecting)).on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Answer(about_paper, content.to_string()), cx)))
                    // What is selected in this answer: copied, or taken into the box as a quotation to ask about.
                    .when(picked.is_some() && !holding, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .child(ui::ghost_button(SharedString::from(format!("picked-copy-{side}-{i}")), Some("copy"), Some("复制所选".into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::CopyPicked(about_paper), cx)))
                                .child(ui::ghost_button(SharedString::from(format!("picked-quote-{side}-{i}")), Some("pen-line"), Some("引用到输入框".into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::QuotePicked(about_paper), cx))),
                        )
                    })
                    .when(!chips.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(chips)))
                    .when_some(whole, |d, (question, whole)| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .child(ui::ghost_button(SharedString::from(format!("copy-{side}-{i}")), Some("copy"), None).h(px(24.)).min_w(px(24.)).opacity(0.7).on_click(self.on(Cmd::CopyText(whole.clone()), cx)))
                                // An answer about one paper can join that paper's notes; one about the library names many.
                                .when(about_paper, |d| d.child(ui::ghost_button(SharedString::from(format!("note-{side}-{i}")), Some("pen-line"), Some("存为笔记".into())).h(px(24.)).text_size(px(12.)).opacity(0.7).on_click(self.on(Cmd::AnswerToNote(question, whole), cx)))),
                        )
                    })
                    .into_any_element(),
            );
            i += 1;
        }
        // Text has arrived and more is on its way (a tool call being written, the next piece): the mark keeps turning under it.
        if chat.busy && chat.pending.is_none() && matches!(chat.messages.last(), Some(ChatItem::Answer(text)) if !text.is_empty()) {
            items.push(div().flex().items_center().gap(px(8.)).h(px(18.)).child(ui::spinner(SharedString::from(format!("working-{side}")), 13., Theme::accent())).child(div().text_size(px(12.)).text_color(Theme::text_placeholder()).child(elapsed(chat))).into_any_element());
        }
        if let Some(pending) = &chat.pending {
            let button = |name: &str, label: &'static str, consent: Consent, this: &Self, cx: &mut Context<Self>| {
                let id = SharedString::from(format!("consent-{name}-{side}"));
                let shell = if consent == Consent::Once { ui::primary_button(id, label) } else { ui::secondary_button(id, label) };
                shell.h(px(28.)).on_click(this.on(Cmd::AnswerTools(about_paper, consent), cx))
            };
            items.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(Theme::line_strong())
                    .bg(Theme::bg_sunken())
                    .child(div().flex().items_center().gap(px(6.)).font_weight(FontWeight::MEDIUM).child(ui::icon("info", 14., Theme::accent())).child("助手想改动文献库，等你同意"))
                    .children(pending.calls.iter().filter(|call| call.confirm).map(|call| div().pl(px(20.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(call.line.clone())))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .pl(px(20.))
                            .child(button("once", "允许", Consent::Once, self, cx))
                            .child(button("always", "这次对话里都允许", Consent::Always, self, cx))
                            .child(button("deny", "不允许", Consent::Deny, self, cx)),
                    )
                    .into_any_element(),
            );
        }
        if !chat.note.is_empty() {
            items.push(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(chat.note.clone()).into_any_element());
        }
        if !chat.error.is_empty() {
            items.push(ui::error_line(&chat.error).into_any_element());
        }
        items
    }

    /// A run of tool calls: those done as one line that unfolds, those
    /// still running each with a turning mark.
    fn render_tools(&self, about_paper: bool, chat: &Chat, first: usize, run: &[(&str, &Option<(String, bool)>)], cx: &mut Context<Self>) -> AnyElement {
        let side = if about_paper { "paper" } else { "library" };
        let done: Vec<(&str, &(String, bool))> = run.iter().filter_map(|(call, result)| result.as_ref().map(|result| (*call, result))).collect();
        let failed = done.iter().filter(|(_, (_, failed))| *failed).count();
        let open = chat.tools_open.contains(&first);
        let waiting = chat.pending.is_some();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(12.))
            .when(!done.is_empty(), |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("tools-{side}-{first}")))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .h(px(22.))
                        .cursor_pointer()
                        .text_color(Theme::text_tertiary())
                        .hover(|s| s.text_color(Theme::text_secondary()))
                        .on_click(self.on(Cmd::ToggleTools(about_paper, first), cx))
                        .child(div().min_w_0().truncate().child(tools_summary(&done.iter().map(|(call, _)| *call).collect::<Vec<_>>())))
                        .when(failed > 0, |d| d.child(div().flex_none().text_color(Theme::danger()).child(format!("· {failed} 项没有成功"))))
                        .child(ui::icon(if open { "chevron-down" } else { "chevron-right" }, 12., Theme::text_tertiary())),
                )
                .when(open, |d| {
                    d.child(div().flex().flex_col().gap(px(3.)).pl(px(10.)).ml(px(3.)).border_l_1().border_color(Theme::line()).children(done.iter().map(|(call, (outcome, failed))| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(20.))
                            .child(div().flex_none().max_w(relative(0.62)).truncate().text_color(Theme::text_secondary()).child(call.to_string()))
                            .child(div().min_w_0().truncate().text_color(if *failed { Theme::danger() } else { Theme::text_tertiary() }).child(outcome.clone()))
                    })))
                })
            })
            // The calls still running; those waiting for consent are named on the card that asks for it.
            .children(run.iter().enumerate().filter(|(_, (_, result))| result.is_none() && !waiting).map(|(n, (call, _))| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(22.))
                    .text_color(Theme::text_secondary())
                    .child(ui::spinner(SharedString::from(format!("tool-{side}-{}", first + n)), 13., Theme::accent()))
                    .child(div().min_w_0().truncate().child(call.to_string()))
                    .child(div().flex_none().text_color(Theme::text_placeholder()).child(elapsed(chat)))
            }))
            .into_any_element()
    }

    /// The library's past conversations (or the paper's), the latest first.
    fn render_history(&self, about_paper: bool, chat: &Chat, history: &[wisp_core::store::ConversationInfo], cx: &mut Context<Self>) -> AnyElement {
        let side = if about_paper { "paper" } else { "library" };
        if history.is_empty() {
            return ui::empty_state("history", "还没有历史对话", "提过的问题和回答会留在这个文献库里，之后可以在这里找到，接着问下去。").into_any_element();
        }
        div()
            .id(SharedString::from(format!("history-{side}")))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(10.))
            .py(px(8.))
            .gap(px(2.))
            .children(history.iter().map(|conversation| {
                let current = conversation.id == chat.id;
                let group = SharedString::from(format!("history-row-{}", conversation.id));
                div()
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pr(px(4.))
                    .rounded(px(8.))
                    .hover(|s| s.bg(Theme::hover()))
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Conversation(about_paper, conversation.id.clone()), cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("history-{side}-{}", conversation.id)))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .px(px(8.))
                            .py(px(7.))
                            .cursor_pointer()
                            .on_click(self.on(Cmd::OpenChat(about_paper, conversation.id.clone()), cx))
                            .child(div().truncate().child(conversation.title.clone()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(12.))
                                    .text_color(Theme::text_tertiary())
                                    .child(format!("{} · {} 个问题", time::short_time(&conversation.updated_at), conversation.questions))
                                    .when(current, |d| d.child(div().text_color(Theme::accent()).child("正在进行"))),
                            ),
                    )
                    .child(ui::ghost_button(SharedString::from(format!("history-remove-{side}-{}", conversation.id)), Some("bin"), None).invisible().group_hover(group, |s| s.visible()).on_click(self.on(Cmd::AskRemoveChat(about_paper, conversation.id.clone()), cx)))
            }))
            .into_any_element()
    }

    /// The box the next question is written in: its pictures, a button to
    /// add more, the text, and the button that sends it or stops the answer.
    /// Over it, the slash commands that match what is being typed.
    fn render_composer(&self, about_paper: bool, chat: &Chat, model: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let side = if about_paper { "paper" } else { "library" };
        let focused = chat.input.focus_handle(cx).is_focused(window);
        let commands = if focused { chat.slash_list(&self.settings.commands, cx) } else { vec![] };
        let ready = !chat.input.read(cx).text().trim().is_empty() || !chat.attachments.is_empty() || !chat.quotes.is_empty();
        let lib = self.lib.as_ref().unwrap();
        let round = |id: String, icon_name: &'static str, lit: bool| {
            div().id(SharedString::from(id)).flex().flex_none().items_center().justify_center().size(px(30.)).rounded(px(15.)).bg(if lit { Theme::accent() } else { Theme::groove() }).cursor_pointer().hover(|s| s.opacity(0.88)).child(ui::icon(icon_name, 15., if lit { Theme::on_accent() } else { Theme::text_tertiary() }))
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(5.))
            .px(px(16.))
            .pb(px(10.))
            .pt(px(6.))
            .when(!commands.is_empty(), |d| {
                d.child(
                    ui::popover()
                        .absolute()
                        .left(px(16.))
                        .bottom_full()
                        .min_w(px(340.))
                        .max_w(px(520.))
                        .children(commands.iter().enumerate().map(|(n, command)| {
                            div()
                                .id(SharedString::from(format!("slash-{side}-{}", command.name)))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .h(px(32.))
                                .px(px(8.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .when(n == chat.slash, |d| d.bg(Theme::hover()))
                                .hover(|s| s.bg(Theme::hover()))
                                .on_click(self.on(Cmd::PickSlash(about_paper, n), cx))
                                .child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(Theme::accent()).child(format!("/{}", command.name)))
                                .when(!command.hint.is_empty(), |d| d.child(div().flex_none().text_color(Theme::text_placeholder()).child(command.hint.clone())))
                                .child(div().flex_1().min_w_0().truncate().text_right().text_color(Theme::text_secondary()).child(command.about.clone()))
                                // One the person wrote says so: what it asks is theirs, not the app's.
                                .when(command.custom, |d| d.child(div().flex_none().px(px(6.)).rounded(px(5.)).bg(Theme::groove()).text_size(px(11.)).text_color(Theme::text_tertiary()).child("自定义")))
                        }))
                        .child(div().px(px(8.)).pt(px(5.)).pb(px(2.)).mt(px(3.)).border_t_1().border_color(Theme::line()).text_size(px(11.)).text_color(Theme::text_placeholder()).child("↑↓ 选择 · Enter 或 Tab 确认 · Esc 关闭")),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(8.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(if focused { Theme::accent() } else { Theme::line_strong() })
                    .bg(Theme::bg_body())
                    .shadow(theme::shadow_card())
                    // The passages selected in a PDF that go with the question: where each is from, its opening, and a way to take it back.
                    .when(!chat.quotes.is_empty(), |d| {
                        d.child(div().flex().flex_col().gap(px(4.)).px(px(2.)).pt(px(2.)).children(chat.quotes.iter().enumerate().map(|(n, quote)| {
                            let title = lib.paper(&quote.paper_id).map(|paper| paper.title.clone()).unwrap_or_default();
                            div()
                                .flex()
                                .items_start()
                                .gap(px(8.))
                                .pl(px(10.))
                                .pr(px(4.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .border_l_2()
                                .border_color(Theme::accent())
                                .bg(Theme::bg_sunken())
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(1.))
                                        .child(div().line_clamp(2).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(quote.text.clone()))
                                        .child(div().truncate().text_size(px(11.)).text_color(Theme::text_tertiary()).child(format!("原文 · 第 {} 页 · {title}", quote.page))),
                                )
                                .child(ui::ghost_button(SharedString::from(format!("quote-remove-{side}-{n}")), Some("x"), None).h(px(20.)).min_w(px(20.)).on_click(self.on(Cmd::RemoveQuote(about_paper, n), cx)))
                        })))
                    })
                    .when(!chat.attachments.is_empty() || chat.attaching > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(8.))
                                .px(px(2.))
                                .pt(px(2.))
                                .children(chat.attachments.iter().enumerate().map(|(n, picture)| {
                                    div().relative().child(thumbnail(picture, 64.)).child(
                                        div()
                                            .id(SharedString::from(format!("attachment-remove-{side}-{n}")))
                                            .absolute()
                                            .top(px(-5.))
                                            .right(px(-5.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .size(px(18.))
                                            .rounded(px(9.))
                                            .bg(Theme::text())
                                            .cursor_pointer()
                                            .hover(|s| s.opacity(0.85))
                                            .on_click(self.on(Cmd::RemoveAttachment(about_paper, n), cx))
                                            .child(ui::icon("x", 11., Theme::bg_body())),
                                    )
                                }))
                                .children((0..chat.attaching).map(|n| div().flex().flex_none().items_center().justify_center().size(px(64.)).rounded(px(10.)).bg(Theme::bg_sunken()).child(ui::spinner(SharedString::from(format!("attaching-{side}-{n}")), 16., Theme::text_tertiary())))),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(6.))
                            .child(ui::ghost_button(SharedString::from(format!("chat-attach-{side}")), Some("plus"), None).h(px(30.)).min_w(px(30.)).rounded(px(15.)).on_click(self.on(Cmd::AttachImages(about_paper), cx)))
                            .child(div().flex().flex_1().min_w_0().py(px(5.)).max_h(px(170.)).overflow_hidden().text_size(px(13.)).line_height(px(20.)).text_color(Theme::text()).child(chat.input.clone()))
                            .child(if chat.busy { round(format!("chat-stop-{side}"), "square", true).on_click(self.on(Cmd::StopChat(about_paper), cx)) } else { round(format!("chat-send-{side}"), "send", ready).on_click(self.on(Cmd::Ask(about_paper), cx)) }),
                    ),
            )
            .child(div().flex().gap(px(8.)).px(px(4.)).text_size(px(11.)).text_color(Theme::text_placeholder()).child(div().flex_1().min_w_0().truncate().child(if about_paper { "Enter 发送 · 回答可能有误，请对照原文" } else { "Enter 发送，Shift+Enter 换行 · 回答可能有误，请对照原文" })).child(div().flex_none().max_w(relative(0.4)).truncate().child(model.to_string())))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::tools_summary;

    #[test]
    fn tool_runs_in_a_line() {
        assert_eq!(tools_summary(&["检索全文：chromium", "查看概况", "检索全文：10x · 第 2 页起"]), "检索全文 ×2 · 查看概况");
        assert_eq!(tools_summary(&[]), "");
    }
}
