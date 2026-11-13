//! 提问: the conversation with the library (the current list) or with the
//! selected paper. The answer cites labels like [P3 p.5]; the chips under
//! it open the paper, or its PDF at that page. Between the pieces of an
//! answer stand the tools the assistant called to find it, folded into one
//! line once they are done. Under the conversation is the box the next
//! question is written in, with its pictures; over it, while a slash
//! command is being typed, the commands that match. The history lists the
//! library's past conversations in place of the one in hand.

use crate::i18n::t;
use crate::app::{Chat, ChatImage, ChatItem, Cmd, Consent, MenuKind, ScopeDo, Workspace};
use crate::input::TextInput;
use crate::settings::SettingsPage;
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::{ai, time};

/// The sources shown under an answer before the rest is asked for.
const SOURCES_SHOWN: usize = 8;

/// The papers the first question is kept to that are named over the question box; the rest are counted.
const PINNED_SHOWN: usize = 8;

/// A paper's row of the scope panel on its way into the conversation: what
/// is held, and the card drawn under the pointer meanwhile.
#[derive(Clone)]
pub struct PaperDrag {
    pub id: String,
    title: String,
    /// Where in its row the pointer took hold of it.
    held: Point<Pixels>,
}

impl Render for PaperDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // What is dragged is drawn from the row's corner, and the row is as wide as the panel: the card is moved to the pointer.
        div().pl((self.held.x - px(16.)).max(px(0.))).pt((self.held.y - px(12.)).max(px(0.))).child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .w(px(280.))
                .h(px(30.))
                .px(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(Theme::accent())
                .bg(Theme::bg_body())
                .shadow(theme::shadow_panel())
                .font_family(theme::UI_FONT)
                .text_size(px(12.))
                .text_color(Theme::text())
                .child(ui::icon("file-text", 13., Theme::accent()))
                .child(div().w(px(238.)).truncate().child(self.title.clone())),
        )
    }
}

/// The dialog for a claim and its source; nothing is written until it is saved.
pub struct EvidenceForm {
    /// The row being changed; empty for a new one.
    pub id: String,
    /// The section the claim stands in, by name; empty for a claim on its own.
    pub section: String,
    pub claim: Entity<TextInput>,
    pub paper_query: Entity<TextInput>,
    pub paper_id: String,
    pub page: Entity<TextInput>,
    pub quote: Entity<TextInput>,
    pub note: Entity<TextInput>,
    pub source: String,
}

fn evidence_source_name(source: &wisp_core::review::EvidenceSource) -> &'static str {
    use wisp_core::review::EvidenceSource::*;
    match source { Unknown => "未说明", Record => "题录", Abstract => "摘要", FullText => "全文" }
}

/// The person's judgments of a source, in the order their buttons stand.
const JUDGMENTS: [(wisp_core::review::EvidenceJudgment, &str); 4] = {
    use wisp_core::review::EvidenceJudgment::*;
    [(Supports, "支持"), (Partial, "部分支持"), (Contradicts, "不支持"), (Insufficient, "无法判断")]
};

/// What looking a quotation up in its paper came to: the icon, the words and the colour that say so.
fn quote_verdict(check: Option<&ai::Quoted>) -> Option<(&'static str, String, Hsla)> {
    Some(match check? {
        ai::Quoted::Found => ("check", t("原文一致").to_string(), Theme::success()),
        ai::Quoted::Elsewhere(at) => ("info", tf!("原文在 p.{at}", "Quote found on p.{at}"), Theme::warning()),
        ai::Quoted::Begins(_, share) => ("info", tf!("只对上开头的 {:.0}%", "Only the first {:.0}% matches", share * 100.), Theme::warning()),
        ai::Quoted::Missing => ("x", t("原文里没有这句").to_string(), Theme::danger()),
        ai::Quoted::NoText => ("info", t("没有全文可核对").to_string(), Theme::text_tertiary()),
    })
}

/// Where a review stands, in a line.
fn review_step(review: &wisp_core::review::Review) -> &'static str {
    match review.step() {
        1 if review.requirements.is_empty() => t("第 1 步 · 把写作要求记下来"),
        1 => t("第 1 步 · 确认写作要求"),
        2 if review.framework.is_empty() => t("第 2 步 · 读已有综述，把初步框架记下来"),
        2 => t("第 2 步 · 确认初步框架"),
        3 if review.outline.is_empty() && review.framework.is_empty() => t("第 3 步 · 把大纲记下来"),
        3 if review.outline.is_empty() => t("第 3 步 · 对照初步框架重建大纲，记下来"),
        3 => t("第 3 步 · 确认大纲"),
        _ if review.completed => t("综述已标为完成"),
        _ => t("第 4 步 · 分节写作与核对"),
    }
}

/// What a question of a review's conversation tells the model about the record: where it stands and what was agreed.
pub(crate) fn review_note(review: &wisp_core::review::Review) -> String {
    let stage = match review.step() {
        1 => "写作要求尚未确认。先整理并请用户在界面确认，不要写大纲或正文。",
        2 => "写作要求已确认，初步框架尚未确认。这一步先读已有综述：找出这个主题已发表的综述（文献库里的，和 search_online 能查到的），逐篇比较它们问的核心问题、实际的章节结构、长处和盲点，标明各读到全文、摘要还是仅有题录；再综合出初步框架，写明每章要回答的问题和暂定答案。不要照搬某一篇的目录，也不要凭记忆补一份；找不到相关综述时照实说明，用户可以在界面跳过这一步。等待用户在界面确认，不要定大纲或写正文。",
        3 if !review.framework.is_empty() => "初步框架已由用户确认，大纲尚未确认。初步框架来自已有综述，还没有被证据检验：按它的每一章去读文献库里的原始研究，对照着重建大纲，逐章说明是保留、改写、合并、拆分、删去还是新增，以及是哪些文献让你这样改；文献库的证据撑不住的地方和已有综述没讲到的地方各单独列出。可检索、提出和调整大纲，等待用户在界面确认，不要写正文。",
        3 => "用户选择不参照已有综述，大纲尚未确认。可检索、提出和调整大纲，等待用户在界面确认，不要写正文。",
        _ => "要求和大纲已由用户确认。围绕大纲补充证据、按节写作或检查；每次只写用户要的那一节，以「## 节名」开头，不加寒暄。如用户要改变要求或大纲，请提示在界面修改并重新确认。",
    };
    let mut note = format!("[当前综述记录]\n{stage}");
    if !review.requirements.is_empty() { note.push_str(&format!("\n写作要求：\n{}", review.requirements)) }
    // The framework is what the outline is built against; once the outline stands, it is the outline that steers.
    if !review.framework.is_empty() && review.step() <= 3 { note.push_str(&format!("\n\n初步框架（来自已有综述）：\n{}", review.framework)) }
    if !review.outline.is_empty() { note.push_str(&format!("\n\n大纲：\n{}", review.outline)) }
    if !review.evidence.is_empty() { note.push_str(&format!("\n\n证据笔记：\n{}", review.evidence)) }
    if !review.sections.is_empty() { note.push_str(&format!("\n\n已保存的章节：{}", review.sections.iter().map(|section| section.name.as_str()).collect::<Vec<_>>().join("、"))) }
    note
}

/// A text to keep as a section: its name from a heading on its first line, else numbered, and the text under it.
fn section_of(text: &str, review: &wisp_core::review::Review) -> (String, String) {
    let trimmed = text.trim();
    let (heading, body) = match trimmed.split_once('\n') {
        Some((line, rest)) if line.starts_with('#') && !rest.trim().is_empty() => (line.trim_start_matches('#').trim().chars().take(60).collect::<String>(), rest.trim().to_string()),
        _ => (String::new(), trimmed.to_string()),
    };
    // Kept again under a name already there, it is another section: nothing is overwritten by a click.
    let base = if heading.is_empty() { tf!("正文 {}", "Section {}", review.sections.len() + 1) } else { heading };
    let mut name = base.clone();
    let mut number = 2;
    while review.sections.iter().any(|section| section.name == name) {
        name = format!("{base} ({number})");
        number += 1;
    }
    (name, body)
}

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
    /// Carry out a change to the review of the conversation with the library, and read the record again.
    pub(crate) fn review_apply(&mut self, action: wisp_core::review::ReviewAction, done: &str, cx: &mut Context<Self>) -> bool {
        let Some(lib) = self.lib.as_mut() else { return false };
        let Some((id, revision)) = lib.library_chat.review.as_ref().map(|review| (review.id.clone(), review.revision)) else {
            self.fail(t("这次对话还没有综述记录。"), cx);
            return false
        };
        let result = lib.store.apply_review(&id, revision, action).and_then(|_| lib.load_review());
        match result {
            Ok(()) => {
                if !done.is_empty() { self.say(done.to_string(), cx) }
                cx.notify();
                true
            }
            Err(err) => {
                // Changed elsewhere (the command line, another window): show what is there now.
                let _ = lib.load_review();
                self.fail(tf!("综述记录没有保存：{err:#}", "Review record not saved: {err:#}"), cx);
                false
            }
        }
    }

    /// A claim and its source in a dialog: a row of the record, or a new one for a claim selected in an answer.
    pub(crate) fn edit_review_evidence(&mut self, id: Option<String>, claim: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.lib.as_ref().and_then(|lib| lib.library_chat.review.as_ref()) else { return self.fail(t("请先开始综述流程。"), cx) };
        let row = match id.as_ref() {
            Some(id) => match review.evidence_rows.iter().find(|row| &row.id == id) {
                Some(row) => row.clone(),
                None => return self.fail(t("证据记录不存在，请重新载入。"), cx),
            },
            // A claim selected in a text that was kept as a section belongs to that section.
            None => wisp_core::review::EvidenceRow { section: review.sections.iter().find(|section| !claim.trim().is_empty() && section.body.contains(claim.trim())).map(|section| section.name.clone()).unwrap_or_default(), claim, ..Default::default() },
        };
        let section = if row.section_detached { String::new() } else { row.section.clone() };
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let claim = self.modal_input("写明这条论断", &row.claim, true, window, cx);
        let query = self.modal_input("搜索文献题目、作者或 DOI", "", false, window, cx);
        let subscription = cx.observe(&query, |_, _, cx| cx.notify());
        self.modal_subscriptions.push(subscription);
        let page = self.modal_input("PDF 页码，例如 5；可留空", &row.page.map(|n| n.to_string()).unwrap_or_default(), false, window, cx);
        let quote = self.modal_input("依据的原文一句，可留空；会和文献的文字比对", &row.quote, true, window, cx);
        let note = self.modal_input("对象、方法、限制等，可留空", &row.note, true, window, cx);
        let source = evidence_source_name(&row.source).to_string();
        self.modal = Some(crate::app::Modal::ReviewEvidence(Box::new(EvidenceForm { id: row.id, section, claim: claim.clone(), paper_query: query, paper_id: row.paper_id, page, quote, note, source })));
        claim.read(cx).focus(window);
        cx.notify();
    }

    pub(crate) fn submit_review_evidence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use wisp_core::review::{EvidenceRow, EvidenceSource, ReviewAction};
        let Some(crate::app::Modal::ReviewEvidence(form)) = self.modal.as_ref() else { return };
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_string();
        let page = value(&form.page);
        let page = if page.is_empty() { None } else { match page.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => { self.modal_error = t("页码必须是大于零的整数。").into(); return cx.notify() }
        }};
        let source = match form.source.as_str() { "题录" => EvidenceSource::Record, "摘要" => EvidenceSource::Abstract, "全文" => EvidenceSource::FullText, _ => EvidenceSource::Unknown };
        let row = EvidenceRow { id: form.id.clone(), section: form.section.clone(), claim: value(&form.claim), paper_id: form.paper_id.clone(), page, quote: value(&form.quote), note: value(&form.note), source, ..Default::default() };
        let Some(lib) = self.lib.as_mut() else { return };
        let Some((id, revision)) = lib.library_chat.review.as_ref().map(|review| (review.id.clone(), review.revision)) else { return };
        match lib.store.apply_review(&id, revision, ReviewAction::SaveEvidence(row)).and_then(|_| lib.load_review()) {
            Ok(()) => { lib.library_chat.review_open = true; self.run(Cmd::CloseModal, window, cx); self.say(t("证据已记下。它是否支持论断，由你判断。"), cx) }
            Err(err) => { self.modal_error = format!("{err:#}"); cx.notify(); }
        }
    }

    /// A text of the record in a dialog, to change by hand.
    pub(crate) fn edit_review_text(&mut self, what: crate::app::ReviewText, window: &mut Window, cx: &mut Context<Self>) {
        use crate::app::ReviewText;
        let Some(review) = self.lib.as_ref().and_then(|lib| lib.library_chat.review.as_ref()) else { return };
        let text = match &what {
            ReviewText::Requirements => review.requirements.clone(),
            ReviewText::Framework => review.framework.clone(),
            ReviewText::Outline => review.outline.clone(),
            ReviewText::Notes => review.evidence.clone(),
            ReviewText::Checks => review.checks.clone(),
            ReviewText::Section(name) => review.sections.iter().find(|section| &section.name == name).map(|section| section.body.clone()).unwrap_or_default(),
        };
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let input = self.modal_input("", &text, true, window, cx);
        input.update(cx, |input, _| input.min_lines = 12);
        input.read(cx).focus(window);
        self.modal = Some(crate::app::Modal::ReviewText { what, input });
        cx.notify();
    }

    pub(crate) fn submit_review_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::app::ReviewText;
        use wisp_core::review::ReviewAction;
        let Some(crate::app::Modal::ReviewText { what, input }) = self.modal.as_ref() else { return };
        let text = input.read(cx).text().trim().to_string();
        let sources = self.lib.as_ref().map(|lib| lib.library_chat.sources().to_vec()).unwrap_or_default();
        let action = match what {
            ReviewText::Requirements => ReviewAction::SetRequirements(text),
            ReviewText::Framework => ReviewAction::SetFramework(text),
            ReviewText::Outline => ReviewAction::SetOutline(text),
            ReviewText::Notes => ReviewAction::SetEvidence(text),
            ReviewText::Checks => ReviewAction::SetChecks(text),
            ReviewText::Section(name) => ReviewAction::SaveSection { name: name.clone(), cited: wisp_core::review::cited_evidence(name, &text, &sources), body: text },
        };
        let Some(lib) = self.lib.as_mut() else { return };
        let Some((id, revision)) = lib.library_chat.review.as_ref().map(|review| (review.id.clone(), review.revision)) else { return };
        match lib.store.apply_review(&id, revision, action).and_then(|_| lib.load_review()) {
            Ok(()) => { self.run(Cmd::CloseModal, window, cx); self.say(t("已保存"), cx) }
            Err(err) => { self.modal_error = format!("{err:#}"); cx.notify(); }
        }
    }

    pub(crate) fn render_review_text_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        use crate::app::ReviewText;
        let Some(crate::app::Modal::ReviewText { what, input }) = self.modal.as_ref() else { return None };
        let (title, hint) = match what {
            ReviewText::Requirements => (t("写作要求").to_string(), t("改动后要重新确认要求和大纲；已保存的正文和证据都留着。")),
            ReviewText::Framework => (t("初步框架").to_string(), t("从已有综述得来的框架。改动后要重新确认它和大纲；已保存的正文和证据都留着。")),
            ReviewText::Outline => (t("大纲").to_string(), t("改动后要重新确认大纲；已保存的正文和证据都留着。")),
            ReviewText::Notes => (t("证据笔记").to_string(), t("自由记下的证据情况，例如哪些文献只读到摘要、哪里还缺证据。")),
            ReviewText::Checks => (t("检查记录").to_string(), t("核对过什么、还有什么没能核对。保存后算作对当前内容的检查。")),
            ReviewText::Section(name) => (tf!("正文 · {name}", "Section · {name}"), t("引用保持 [P3 p.5 \"原句\"] 的写法：存为稿件时会转成稿件的引用，句子变了的论断要重新判断。")),
        };
        Some(ui::dialog(720.).max_w(relative(0.92)).max_h(relative(0.9))
            .child(ui::dialog_title(title))
            .child(div().id("review-text-form").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().gap(px(8.)).px(px(20.)).py(px(8.))
                .child(ui::field(input, window, cx))
                .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(hint))
                .child(ui::error_line(&self.modal_error)))
            .child(div().flex().justify_end().gap(px(8.)).px(px(20.)).py(px(14.))
                .child(ui::secondary_button("review-text-cancel", t("取消")).on_click(self.on(Cmd::CloseModal, cx)))
                .child(ui::primary_button("review-text-save", t("保存")).on_click(self.on(Cmd::SubmitModal, cx)))))
    }

    /// Start the record of a review for the conversation with the library.
    pub(crate) fn start_review(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = &lib.library_chat;
        if chat.id.is_empty() || chat.review.is_some() { return }
        let conversation = chat.id.clone();
        // The review is called what it was asked for: the first question, without the command.
        let title = chat.messages.iter().find_map(|item| match item {
            ChatItem::User { text, .. } if !text.trim().is_empty() => Some(text.trim().strip_prefix("/review").unwrap_or(text.trim()).trim().chars().take(80).collect::<String>()),
            _ => None,
        }).filter(|title| !title.is_empty()).unwrap_or_else(|| t("综述").into());
        match lib.store.create_review_for_conversation(&title, &conversation).and_then(|_| lib.load_review()) {
            Ok(()) => cx.notify(),
            Err(err) => self.fail(tf!("无法开始综述流程：{err:#}", "Cannot start review: {err:#}"), cx),
        }
    }

    /// A step of the review taken from the conversation: an answer (or what is selected in one) kept in the record, or a confirmation.
    pub(crate) fn review_do(&mut self, what: crate::app::ReviewDo, text: String, cx: &mut Context<Self>) {
        use crate::app::ReviewDo;
        use wisp_core::review::ReviewAction;
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.library_chat.busy { return self.fail(t("请先等回答结束。"), cx) }
        let Some(review) = lib.library_chat.review.as_ref() else { return self.fail(t("这次对话还没有综述记录。"), cx) };
        let (action, done) = match what {
            ReviewDo::Requirements => (ReviewAction::SetRequirements(text.trim().to_string()), t("已记为写作要求。看一遍，需要的话在「查看记录」里修改，然后确认。").to_string()),
            ReviewDo::ConfirmRequirements => (ReviewAction::ConfirmRequirements, t("写作要求已确认。下一步读已有综述，得出初步框架。").to_string()),
            ReviewDo::Framework => (ReviewAction::SetFramework(text.trim().to_string()), t("已记为初步框架。看一遍，需要的话在「查看记录」里修改，然后确认。").to_string()),
            ReviewDo::ConfirmFramework => (ReviewAction::ConfirmFramework, t("初步框架已确认。下一步对照它，按文献库的证据重建大纲。").to_string()),
            ReviewDo::SkipFramework => (ReviewAction::SkipFramework, t("不参照已有综述。下一步按文献库提出大纲。").to_string()),
            ReviewDo::Outline => (ReviewAction::SetOutline(text.trim().to_string()), t("已记为大纲。看一遍，需要的话在「查看记录」里修改，然后确认。").to_string()),
            ReviewDo::ConfirmOutline => (ReviewAction::ConfirmOutline, t("大纲已确认。可以让助手按节写正文了。").to_string()),
            ReviewDo::Section => {
                let (name, body) = section_of(&text, review);
                let cited = wisp_core::review::cited_evidence(&name, &body, lib.library_chat.sources());
                let found = cited.len();
                (ReviewAction::SaveSection { name: name.clone(), body, cited }, if found > 0 { tf!("「{name}」已保存，它的引用带来 {found} 条证据，等你判断", "“{name}” saved with {found} citation evidence records awaiting your judgment") } else { tf!("「{name}」已保存", "“{name}” saved") })
            }
            ReviewDo::Checks => (ReviewAction::SetChecks(text.trim().to_string()), t("已记为检查记录").to_string()),
            ReviewDo::Complete => (ReviewAction::Complete, t("综述已标为完成").to_string()),
        };
        self.review_apply(action, &done, cx);
    }

    pub(crate) fn render_review_evidence_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let Some(crate::app::Modal::ReviewEvidence(form)) = self.modal.as_ref() else { return None };
        let lib = self.lib.as_ref()?;
        let review = lib.library_chat.review.as_ref()?;
        let pill = |id: ElementId, label: String, on: bool, cmd: Cmd, cx: &mut Context<Self>| div().id(id).flex().flex_none().items_center().h(px(26.)).px(px(10.)).rounded(px(13.)).text_size(px(12.)).cursor_pointer().border_1()
            .when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).border_color(Theme::accent().opacity(0.35)))
            .when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
            .on_click(self.on(cmd, cx)).child(label);
        let mut sections = vec![pill("evidence-section-none".into(), t("不关联").into(), form.section.is_empty(), Cmd::PickEvidenceSection(String::new()), cx)];
        for (n, section) in review.sections.iter().enumerate() {
            sections.push(pill(("evidence-section", n).into(), section.name.clone(), form.section == section.name, Cmd::PickEvidenceSection(section.name.clone()), cx));
        }
        let mut kinds = vec![];
        for (n, label) in ["未说明", "题录", "摘要", "全文"].into_iter().enumerate() {
            kinds.push(pill(("evidence-kind", n).into(), t(label).to_string(), form.source == label, Cmd::PickEvidenceSource(label.to_string()), cx));
        }
        let query = form.paper_query.read(cx).text().trim().to_lowercase();
        // The paper chosen stands first, so it is seen whatever is typed.
        let chosen = lib.paper(&form.paper_id);
        let found = lib.papers.iter().filter(|paper| paper.id != form.paper_id && (query.is_empty() || format!("{} {} {}", paper.title, paper.authors, paper.doi).to_lowercase().contains(&query))).take(40);
        let mut papers = vec![];
        for (n, paper) in chosen.into_iter().chain(found).enumerate() {
            let active = form.paper_id == paper.id;
            let by = [paper.authors.split([';', ',']).next().unwrap_or_default().trim().to_string(), paper.year.map(|year| year.to_string()).unwrap_or_default()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
            papers.push(div().id(("evidence-paper", n)).flex().flex_none().items_center().gap(px(8.)).px(px(8.)).py(px(5.)).rounded(px(6.)).cursor_pointer().text_size(px(13.))
                .when(active, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()))
                .when(!active, |d| d.hover(|s| s.bg(Theme::hover())))
                .on_click(self.on(Cmd::PickEvidencePaper(paper.id.clone()), cx))
                .child(div().flex_1().min_w_0().truncate().child(paper.title.clone()))
                .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(by)));
        }
        let gone = !form.paper_id.is_empty() && chosen.is_none();
        Some(ui::dialog(620.).max_w(relative(0.92)).max_h(relative(0.9))
            .child(ui::dialog_title(if form.id.is_empty() { t("记下一条论断和它的依据") } else { t("论断和它的依据") }))
            .child(div().id("review-evidence-form").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().gap(px(12.)).px(px(20.)).py(px(8.))
                .child(ui::form_row(t("论断"), ui::field(&form.claim, window, cx)))
                .when(!review.sections.is_empty(), |d| d.child(ui::form_row(t("所在章节"), div().flex().flex_wrap().gap(px(6.)).children(sections))))
                .child(ui::form_row(t("来源文献"), div().flex().flex_col().gap(px(6.))
                    .child(ui::field(&form.paper_query, window, cx))
                    .when(gone, |d| d.child(div().text_size(px(12.)).text_color(Theme::warning()).child(t("原来的来源已不在文献库里，请另选一篇。"))))
                    .child(div().id("evidence-paper-list").flex().flex_col().h(px(132.)).overflow_y_scroll().rounded(px(8.)).border_1().border_color(Theme::line()).p(px(4.))
                        .when(papers.is_empty(), |d| d.child(div().p(px(8.)).text_size(px(13.)).text_color(Theme::text_tertiary()).child(t("没有匹配的文献。"))))
                        .children(papers))))
                .child(ui::form_row(t("页码"), ui::field(&form.page, window, cx)))
                .child(ui::form_row(t("原文摘录"), ui::field(&form.quote, window, cx)))
                .child(ui::form_row(t("依据"), div().flex().flex_wrap().gap(px(6.)).children(kinds)))
                .child(ui::form_row(t("备注"), ui::field(&form.note, window, cx)))
                .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("页码是 PDF 里从 1 数起的第几页。摘录会和文献保存的文字比对，结果写在这条证据上；它是否支持论断，仍由你判断。改了论断、来源、页码或摘录，原来的判断就不算数了。")))
                .child(ui::error_line(&self.modal_error)))
            .child(div().flex().justify_end().gap(px(8.)).px(px(20.)).py(px(14.))
                .child(ui::secondary_button("evidence-cancel", t("取消")).on_click(self.on(Cmd::CloseModal, cx)))
                .child(ui::primary_button("evidence-save", t("保存")).on_click(self.on(Cmd::SubmitModal, cx)))))
    }

    /// A row of a review's evidence: the claim, where it rests, what looking the quotation up came to, and the person's judgment.
    fn render_evidence(&self, review: &wisp_core::review::Review, row: &wisp_core::review::EvidenceRow, checks: &std::collections::HashMap<String, ai::Quoted>, cx: &mut Context<Self>) -> AnyElement {
        use wisp_core::review::EvidenceJudgment;
        let linked = review.linked(row);
        let judged = row.judgment != EvidenceJudgment::Unreviewed;
        // A judgment stands while the claim is where it was and the source says what it said.
        let stands = judged && linked && row.source_current;
        let check = checks.get(&row.id);
        // The click leads to where the sentence really is.
        let page = match check {
            Some(ai::Quoted::Elsewhere(at) | ai::Quoted::Begins(at, _)) => Some(*at),
            _ => row.page,
        };
        let has_pdf = self.lib.as_ref().is_some_and(|lib| lib.pdf_file(&row.paper_id).is_some());
        let basis = match row.source { wisp_core::review::EvidenceSource::Unknown | wisp_core::review::EvidenceSource::FullText => String::new(), other => tf!(" · 依据{}", " · Based on {}", t(evidence_source_name(&other))) };
        let source = format!("{}{}{basis}", row.paper_title, row.page.map(|page| format!(" · p.{page}")).unwrap_or_default());
        let warning = if !row.source_available { Some(t("来源已不在文献库里（或在回收站），恢复后才能判断。").to_string()) }
            else if !linked { Some(t("这句话已不在正文里。编辑这条证据，把它对上现在的句子，或者删除。").to_string()) }
            else if judged && !row.source_current { Some(tf!("来源的内容变了，之前判断为「{}」，请对照原文重新判断。", "The source changed. Previously judged “{}”; compare with the original again.", JUDGMENTS.iter().find(|(key, _)| *key == row.judgment).map_or("", |(_, label)| t(label)))) }
            else { None };
        let judgments = JUDGMENTS.iter().enumerate().map(|(n, (key, label))| {
            let on = stands && row.judgment == *key;
            div().id(SharedString::from(format!("evidence-judge-{}-{n}", row.id))).flex().flex_none().items_center().h(px(24.)).px(px(9.)).rounded(px(12.)).text_size(px(12.)).cursor_pointer().border_1()
                .when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).border_color(Theme::accent().opacity(0.35)))
                .when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
                .on_click(self.on(Cmd::JudgeReviewEvidence(row.id.clone(), *key), cx)).child(t(label))
        }).collect::<Vec<_>>();
        div().flex().flex_col().gap(px(6.)).p(px(12.)).rounded(px(10.)).border_1().border_color(Theme::line()).bg(Theme::bg_body())
            .child(self.selectable(format!("evidence-claim-{}", row.id), row.claim.clone(), cx).line_height(px(21.)))
            .child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::text_secondary())
                .child(ui::icon("file-text", 12., Theme::text_tertiary()))
                .child(div().min_w_0().truncate().child(source)))
            .when(!row.quote.is_empty(), |d| d.child(div().flex().flex_col().gap(px(3.)).pl(px(10.)).border_l_2().border_color(Theme::line_strong())
                .child(self.selectable(format!("evidence-quote-{}", row.id), row.quote.clone(), cx).text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_secondary()))
                .when_some(quote_verdict(check), |d, (mark, verdict, color)| d.child(div().flex().items_center().gap(px(4.)).text_size(px(12.)).text_color(color).child(ui::icon(mark, 12., color)).child(verdict)))))
            .when(!row.note.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_secondary()).child(tf!("备注：{}", "Note: {}", row.note))))
            .when_some(warning, |d, warning| d.child(div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::warning()).child(warning)))
            .child(div().flex().flex_wrap().items_center().gap(px(5.))
                .when(row.source_available && linked, |d| d.children(judgments))
                .child(div().flex_1())
                .when(row.source_available, |d| d.child(ui::ghost_button(SharedString::from(format!("evidence-goto-{}", row.id)), None, Some(if has_pdf { t("看原文").into() } else { t("看文献").into() })).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::GotoSource(row.paper_id.clone(), page.filter(|_| has_pdf), row.quote.clone()), cx))))
                .child(ui::ghost_button(SharedString::from(format!("evidence-edit-{}", row.id)), None, Some(t("编辑").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::EditReviewEvidence(Some(row.id.clone()), String::new()), cx)))
                .child(ui::ghost_button(SharedString::from(format!("evidence-delete-{}", row.id)), None, Some(t("删除").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::AskRemoveReviewEvidence(row.id.clone()), cx))))
            .into_any_element()
    }

    /// The review's record in place of the conversation: what was agreed, each section with the evidence its citations gave, and the checks.
    fn render_review_record(&self, chat: &Chat, review: &wisp_core::review::Review, cx: &mut Context<Self>) -> AnyElement {
        use crate::app::ReviewText;
        let heading = |title: String, state: Option<(&'static str, Hsla)>, what: ReviewText, has: bool, cx: &mut Context<Self>| div().flex().items_center().gap(px(8.))
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title.clone()))
            .when_some(state, |d, (label, color)| d.child(div().text_size(px(12.)).text_color(color).child(label)))
            .child(div().flex_1())
            .child(ui::ghost_button(SharedString::from(format!("review-edit-{title}")), Some("pen-line"), Some(if has { t("编辑").into() } else { t("填写").into() })).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::EditReviewText(what), cx)));
        let text = |id: &str, text: &str, empty: &'static str| if text.is_empty() { div().text_size(px(13.)).text_color(Theme::text_tertiary()).child(empty) } else { crate::markdown::render(id, text, None, None) };
        let confirmed = |on: bool, has: bool| if !has { None } else if on { Some((t("已确认"), Theme::success())) } else { Some((t("待确认"), Theme::warning())) };
        let judged = review.evidence_rows.iter().filter(|row| row.judgment != wisp_core::review::EvidenceJudgment::Unreviewed && row.source_current && review.linked(row)).count();
        let block = || div().flex().flex_col().gap(px(8.));
        // Evidence stands under the section its claim is in; what belongs to none comes after them.
        let loose: Vec<_> = review.evidence_rows.iter().filter(|row| row.section_detached || !review.sections.iter().any(|section| section.name == row.section)).collect();
        div().id("review-record").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().px(px(16.)).py(px(14.)).gap(px(22.))
            .child(block()
                .child(heading(t("写作要求").into(), confirmed(review.requirements_confirmed, !review.requirements.is_empty()), ReviewText::Requirements, !review.requirements.is_empty(), cx))
                .child(text("review-requirements", &review.requirements, t("还没有记下。让助手整理出要求后，点回答下面的「记为要求」，或者自己填写。"))))
            // The framework is shown once it is its turn, and stays out of the way where the review went on without one.
            .when(review.requirements_confirmed && (!review.framework.is_empty() || review.step() == 2), |d| d.child(block()
                .child(heading(t("初步框架").into(), confirmed(review.framework_confirmed, !review.framework.is_empty()), ReviewText::Framework, !review.framework.is_empty(), cx))
                .child(div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_tertiary()).child(t("从已有综述得来：它们各问什么、怎么组织、哪里没讲到。它只是起点，大纲要对照它、按文献库里的研究重建。")))
                .child(text("review-framework", &review.framework, t("还没有记下。让助手读已有综述、提出初步框架后，点回答下面的「记为初步框架」；没有可参照的综述，就点上方的「跳过」。")))))
            .when(review.requirements_confirmed && review.framework_settled(), |d| d.child(block()
                .child(heading(t("大纲").into(), confirmed(review.outline_confirmed, !review.outline.is_empty()), ReviewText::Outline, !review.outline.is_empty(), cx))
                .when(review.framework.is_empty(), |d| d.child(div().flex().items_center().gap(px(4.)).text_size(px(12.)).text_color(Theme::text_tertiary())
                    .child(t("没有参照已有综述。"))
                    .child(ui::ghost_button("review-add-framework", None, Some(t("补上初步框架").into())).h(px(22.)).text_size(px(12.)).on_click(self.on(Cmd::EditReviewText(ReviewText::Framework), cx)))))
                .child(text("review-outline", &review.outline, if review.framework.is_empty() { t("还没有记下。让助手提出大纲后，点回答下面的「记为大纲」。") } else { t("还没有记下。让助手对照初步框架重建大纲后，点回答下面的「记为大纲」。") }))))
            .when(review.outline_confirmed, |d| d
                .child(block()
                    .child(div().flex().items_center().gap(px(8.))
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(t("正文与证据")))
                        .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("{} 节 · {} 条证据，已判断 {judged}", "{} sections · {} evidence records, {judged} judged", review.sections.len(), review.evidence_rows.len())))
                        .child(div().flex_1())
                        .child(ui::ghost_button("evidence-add", Some("plus"), Some(t("添加证据").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::EditReviewEvidence(None, String::new()), cx))))
                    .child(div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_tertiary()).child(t("保存一节正文时，它的每处引用成为一条证据：论断是引用所在的那句话，摘录已和文献的文字比对。来源是否支持论断，由你对照原文判断。")))
                    .when(review.sections.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(Theme::text_tertiary()).child(t("还没有保存正文。让助手按大纲写一节，点回答下面的「保存正文」。")))))
                .children(review.sections.iter().enumerate().map(|(n, section)| {
                    let rows: Vec<_> = review.evidence_rows.iter().filter(|row| !row.section_detached && row.section == section.name).collect();
                    block()
                        .child(div().flex().items_center().gap(px(8.))
                            .child(div().font_weight(FontWeight::MEDIUM).child(format!("{} · {}", n + 1, section.name)))
                            .child(div().flex_1())
                            .child(ui::ghost_button(SharedString::from(format!("review-edit-section-{n}")), Some("pen-line"), Some(t("编辑").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::EditReviewText(ReviewText::Section(section.name.clone())), cx)))
                            .child(ui::ghost_button(SharedString::from(format!("review-remove-section-{n}")), Some("bin"), Some(t("删除").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::AskRemoveReviewSection(section.name.clone()), cx))))
                        .child(crate::markdown::render(&format!("review-section-{n}"), &section.body, None, None))
                        .children(rows.into_iter().map(|row| self.render_evidence(review, row, &chat.review_checks, cx)))
                }))
                .when(!loose.is_empty(), |d| d.child(block()
                    .child(div().font_weight(FontWeight::MEDIUM).child(t("不在任何一节里的证据")))
                    .children(loose.iter().map(|row| self.render_evidence(review, row, &chat.review_checks, cx)))))
                .child(block()
                    .child(heading(t("证据笔记").into(), None, ReviewText::Notes, !review.evidence.is_empty(), cx))
                    .child(text("review-notes", &review.evidence, t("自由记下证据的情况：哪些文献只读到摘要，哪里还缺证据。"))))
                .when(!review.sections.is_empty(), |d| d.child(block()
                    .child(heading(t("检查记录").into(), (!review.checks.is_empty()).then_some(if review.checks_current { (t("对当前内容"), Theme::success()) } else { (t("之后内容又变了"), Theme::warning()) }), ReviewText::Checks, !review.checks.is_empty(), cx))
                    .child(text("review-checks", &review.checks, t("写完后让助手按 /verify 逐条核对，点回答下面的「记录检查」；检查是否属实仍要你过目。"))))))
            .into_any_element()
    }

    pub fn render_chat(&mut self, about_paper: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(lib) = self.lib.as_mut() { lib.check_quotes(about_paper) }
        // A drag that ended anywhere but in the conversation says nothing: the flag is put right here.
        if !cx.has_active_drag() { self.pinning = false }
        let lib = self.lib.as_ref().unwrap();
        let chat = lib.chat(about_paper);
        let profile = self.settings.profile_for_or_default(wisp_core::settings::Task::Chat);
        let configured = !profile.key().is_empty();
        let model = profile.title();
        // Ids of the two conversations' elements must differ: both can be on screen.
        let side = if about_paper { "paper" } else { "library" };

        // The papers in scope that have a PDF no text came of: said, so that nobody takes them for read.
        let in_scope = lib.scope_papers();
        let textless = in_scope.iter().filter(|p| p.fulltext == "textless").count();
        let textless_note = if textless > 0 && !about_paper { tf!("；其中 {textless} 篇的 PDF 没读出文字，只有记录和摘要", "; {textless} PDFs have no readable text, so only records and abstracts are used") } else { String::new() };
        // What the model has been given, or will be given with the first question.
        let scope = match &chat.ctx {
            Some(ctx) if about_paper => {
                let mut line = if ctx.pages > 0 { tf!("已提供这篇文献的记录和 {} 页全文", "Provided this paper's record and {} full-text pages", ctx.pages) } else { t("这篇文献没有可读取的全文，只提供了记录和摘要").to_string() };
                if ctx.pages_omitted > 0 { line.push_str(&tf!("（另有 {} 页超出长度未提供）", " ({} more pages exceeded the length limit)", ctx.pages_omitted)) }
                line
            }
            Some(ctx) => {
                let mut line = tf!("已提供 {} 篇文献的记录", "Provided records for {} papers", ctx.full);
                if ctx.titles_only > 0 { line.push_str(&tf!("，{} 篇仅题目", ", {} titles only", ctx.titles_only)) }
                if ctx.omitted > 0 { line.push_str(&tf!("，{} 篇未提供", ", {} omitted", ctx.omitted)) }
                if ctx.pages > 0 { line.push_str(&tf!("，{} 页相关全文", ", {} relevant full-text pages", ctx.pages)) }
                line + &textless_note
            }
            None if !chat.messages.is_empty() => t("历史对话：接着提问时，会按文献库现在的内容重新提供材料").to_string(),
            None if about_paper => {
                let id = lib.selected.clone().unwrap_or_default();
                match lib.pdf_file(&id).map(|f| lib.store.fulltext_pages(&f.id)) {
                    Some(Ok(Some(Ok(pages)))) => tf!("将提供这篇文献的记录和 {} 页全文", "Will provide this paper's record and {} full-text pages", pages.len()),
                    Some(Ok(Some(Err(error)))) => tf!("全文无法读取（{error}），将只提供记录和摘要", "Cannot read full text ({error}); only record and abstract will be provided"),
                    Some(_) => t("全文的文字还在提取，现在提问只会用到记录和摘要").to_string(),
                    None => t("这篇文献没有 PDF，将只提供记录和摘要").to_string(),
                }
            }
            None if !about_paper && !lib.asked.is_empty() => tf!("范围：只读你选定的 {} 篇文献，以及其中已存 PDF 的相关页面{textless_note}。「新对话」回到当前列表", "Scope: only the {} papers you picked, with relevant saved PDF pages{textless_note}. “New chat” returns to the current list", in_scope.len()),
            None if !about_paper && lib.scope_hits.is_some() => tf!("范围：当前列表里符合「{}」的 {} 篇文献，以及其中已存 PDF 的相关页面{textless_note}", "Scope: the {1} papers in the current list matching “{0}”, with relevant saved PDF pages{textless_note}", lib.scope_words.trim(), in_scope.len()),
            None => tf!("范围：当前列表的 {} 篇文献（受左侧视图、搜索和筛选影响），以及其中已存 PDF 的相关页面{textless_note}", "Scope: {} papers in the current list (affected by views, search and filters), with relevant saved PDF pages{textless_note}", lib.shown.len()),
        };

        let listing = chat.history.is_some();
        // The review this conversation writes, as last read: drawing asks the library nothing.
        let review = chat.review.as_ref().filter(|_| !about_paper && !listing);

        // What was asked before can be read without a model; only asking needs one.
        let body = if let Some(history) = &chat.history {
            self.render_history(about_paper, chat, history, cx)
        } else if let Some(review) = review.filter(|_| chat.review_open) {
            self.render_review_record(chat, review, cx)
        } else if !configured && chat.messages.is_empty() {
            ui::empty_state("sparkle", t("还没有配置模型"), t("提问需要一个模型服务：Claude API，或任何 OpenAI 兼容的服务。API Key 只保存在本机。以前的对话在右上角的「历史」里，不配置也能看。")).child(ui::primary_button(SharedString::from(format!("chat-settings-{side}")), t("打开设置")).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))).into_any_element()
        } else if chat.messages.is_empty() && chat.error.is_empty() && !about_paper {
            self.render_start(cx)
        } else if chat.messages.is_empty() && chat.error.is_empty() {
            let suggestions: &[&str] = &[t("这篇文献的研究问题、方法和主要结论是什么？"), t("作者的结论有哪些证据支持？有什么局限？"), t("用三句话向同行介绍这篇文章。")];
            ui::empty_state("sparkle", t("就这篇文献提问"), t("回答依据文献库里的记录、笔记和已存 PDF 的全文，并标出出处。需要时助手会自己检索和翻阅全文。"))
                .id(SharedString::from(format!("chat-empty-{side}"))).w_full().min_h_0().overflow_y_scroll().justify_start()
                .children(suggestions.iter().enumerate().map(|(i, text)| ui::secondary_button(SharedString::from(format!("suggest-{side}-{i}")), "").w_full().h_auto().min_h(px(30.)).py(px(6.)).child(div().flex_1().min_w_0().child(*text)).on_click(self.on(Cmd::Suggest(about_paper, text.to_string()), cx))))
                .into_any_element()
        } else {
            div().id(SharedString::from(format!("chat-{side}"))).flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&chat.scroll).px(px(16.)).py(px(14.)).gap(px(14.)).children(self.render_messages(about_paper, chat, review, cx)).into_any_element()
        };

        // The papers behind the scope line can be looked through: in a conversation whose material was built, or before the first question.
        let listable = !about_paper && !listing && (chat.ctx.is_some() || chat.messages.is_empty());
        let materials = (listable && chat.materials_open).then(|| self.render_materials(chat, window, cx));
        // The first question can still be kept to a few papers: dragged from the panel into the conversation.
        let pinnable = listable && chat.ctx.is_none();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .flex_wrap()
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
                            .min_w(px(200.))
                            .when(!about_paper, |d| d.child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(if listing { t("助手的历史对话") } else { t("助手") })))
                            // The paper's panel has its title above; its history says whose it is.
                            .when(about_paper && listing, |d| d.child(div().font_weight(FontWeight::MEDIUM).child(t("这篇文献的历史对话"))))
                            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(match (listing, about_paper) {
                                (true, false) => t("和助手就整个文献库有过的对话；点开一次可以接着说。单篇文献的对话在各自的「提问」里").to_string(),
                                (true, true) => t("就这篇文献提过的问题和回答；点开一次对话可以接着问").to_string(),
                                (false, _) => scope,
                            })),
                    )
                    .children(self.render_float_switch(about_paper, window, cx))
                    .when(listable, |d| d.child(ui::ghost_button("chat-materials", Some(if chat.materials_open { "chevron-up" } else { "list" }), Some(if chat.materials_open { t("收起").into() } else { t("文献").into() })).on_click(self.on(Cmd::ToggleMaterials, cx))))
                    .child(ui::ghost_button(SharedString::from(format!("chat-history-{side}")), Some(if listing { "arrow-left" } else { "history" }), Some(if listing { t("返回").into() } else { t("历史").into() })).on_click(self.on(Cmd::ToggleHistory(about_paper), cx)))
                    .when(!chat.messages.is_empty() && !listing, |d| d.child(ui::ghost_button(SharedString::from(format!("chat-new-{side}")), Some("plus"), Some(t("新对话").into())).on_click(self.on(Cmd::NewChat(about_paper), cx)))),
            )
            // Under the header, everything the panel of papers floats over: it pushes nothing down.
            .child(div().relative().flex().flex_col().flex_1().min_h_0()
            // A paper dragged out of the panel is let go anywhere in the conversation.
            .when(pinnable, |d| d.on_drop(cx.listener(|this, drag: &PaperDrag, window, cx| this.run(Cmd::Scope(ScopeDo::Pin(drag.id.clone())), window, cx))))
            // A press beside the panel puts it away, as with any floating thing; what it searched for stays.
            .when(materials.is_some(), |d| d.on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if let Some(lib) = this.lib.as_mut() { lib.library_chat.materials_open = false }
                cx.notify();
            })))
            // A review's conversation says where the review stands, and offers the step that is the person's to take.
            .when_some(review, |d, review| {
                use crate::app::ReviewDo;
                let sections = if review.sections.is_empty() { String::new() } else { tf!(" · 正文 {} 节", " · {} sections", review.sections.len()) };
                let open = review.evidence_rows.iter().filter(|row| row.judgment == wisp_core::review::EvidenceJudgment::Unreviewed || !row.source_current || !review.linked(row)).count();
                let evidence = if open > 0 { tf!(" · {open} 条证据待判断", " · {open} evidence records awaiting judgment") } else { String::new() };
                let step = |id: &'static str, label: &'static str, what: ReviewDo, cx: &mut Context<Self>| ui::primary_button(id, label).h(px(26.)).on_click(self.on(Cmd::Review(what, String::new()), cx));
                let ask = |id: &'static str, label: &'static str, request: &'static str, cx: &mut Context<Self>| ui::secondary_button(id, label).h(px(26.)).on_click(self.on(Cmd::Suggest(false, request.to_string()), cx));
                d.child(div().flex().flex_none().flex_wrap().items_center().gap(px(6.)).px(px(16.)).py(px(6.)).border_b_1().border_color(Theme::line()).bg(Theme::bg_sunken()).text_size(px(12.))
                    .child(div().flex_1().min_w_0().text_color(Theme::text_secondary()).child(format!("{}{sections}{evidence}", review_step(review))))
                    .when(review.step() == 1 && !review.requirements.is_empty(), |d| d.child(step("review-confirm-requirements", t("确认要求"), ReviewDo::ConfirmRequirements, cx)))
                    // Where the step's text is still to come, the request that asks for it is one click; keeping the answer is the person's.
                    .when(review.step() == 2 && review.framework.is_empty(), |d| d
                        .when(!chat.busy && !chat.review_open, |d| d.child(ask("review-ask-framework", t("提出初步框架"), t("请找出这个主题已发表的综述，逐篇比较，再提出初步框架。"), cx)))
                        .child(ui::secondary_button("review-skip-framework", t("跳过")).h(px(26.)).tooltip(|_, cx| cx.new(|_| crate::write_assist::Tip(t("没有可参照的综述时，直接按文献库拟大纲").to_string())).into()).on_click(self.on(Cmd::Review(ReviewDo::SkipFramework, String::new()), cx))))
                    .when(review.step() == 2 && !review.framework.is_empty(), |d| d.child(step("review-confirm-framework", t("确认框架"), ReviewDo::ConfirmFramework, cx)))
                    .when(review.step() == 3 && review.outline.is_empty() && !chat.busy && !chat.review_open, |d| d.child(if review.framework.is_empty() {
                        ask("review-ask-outline", t("提出大纲"), t("请按文献库里的文献提出大纲。"), cx)
                    } else {
                        ask("review-ask-outline", t("重建大纲"), t("请对照初步框架，按文献库里的研究重建大纲。"), cx)
                    }))
                    .when(review.step() == 3 && !review.outline.is_empty(), |d| d.child(step("review-confirm-outline", t("确认大纲"), ReviewDo::ConfirmOutline, cx)))
                    .when(review.step() == 4 && !review.sections.is_empty() && review.checks_current && !review.completed, |d| d.child(ui::secondary_button("review-complete", t("标为完成")).h(px(26.)).on_click(self.on(Cmd::Review(ReviewDo::Complete, String::new()), cx))))
                    .when(review.step() == 4 && !review.sections.is_empty(), |d| d.child(ui::secondary_button("review-manuscript", t("存为新稿件")).h(px(26.)).on_click(self.on(Cmd::ReviewToManuscript, cx))))
                    .child(ui::secondary_button("review-toggle", if chat.review_open { t("返回对话") } else { t("查看记录") }).h(px(26.)).on_click(self.on(Cmd::ToggleReview, cx))))
            })
            .child(body)
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
                        .child(div().flex_1().min_w_0().child(t("接着问需要一个模型服务。")))
                        .child(ui::secondary_button(SharedString::from(format!("chat-settings-{side}")), t("打开设置")).h(px(28.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))),
                )
            })
            .children(materials))
            .into_any_element()
    }

    /// The papers the library's questions are put to, to look through
    /// without leaving the page: with the labels of this conversation and
    /// what each was handed over as once the material is built; the papers
    /// of the current list before that. A row opens the paper beside the
    /// conversation; the scope stays what it is.
    /// The assistant's page before anything is said: what it can be asked
    /// to do, by kind. A line is a request sent as it stands, or a command
    /// left in the box for its words to follow.
    fn render_start(&self, cx: &mut Context<Self>) -> AnyElement {
        enum Start {
            Say(&'static str),
            Command(&'static str),
        }
        use Start::*;
        let groups: [(&str, &str, &[Start]); 4] = [
            ("search", "找文献", &[Say("按这个文献库的主题在 PubMed 和 Europe PMC 试检索，看看还有哪些文献没有收进来。"), Say("给还没有 PDF 的文献查找开放全文。"), Command("search")]),
            ("file-text", "读文献", &[Say("这些文献主要在研究哪些问题？按主题归类。"), Say("哪些文献的结论相互支持或矛盾？"), Say("我还没读的文献里，哪几篇最值得先读？为什么？"), Command("compare")]),
            ("list-todo", "整理", &[Say("给这些文献按主题打上标签，优先用已有的标签。"), Say("把我还没读的文献里最值得先读的几篇加上星标。"), Say("读一遍这些文献，为自定义词条提出取值。")]),
            ("pen-line", "写", &[Command("review"), Command("write"), Command("cite")]),
        ];
        let cards = groups.into_iter().map(|(icon, title, starts)| {
            let lines = starts.iter().enumerate().map(|(n, start)| {
                let line = div().id(SharedString::from(format!("start-{icon}-{n}"))).flex().gap(px(8.)).w_full().px(px(8.)).py(px(6.)).rounded(px(6.)).text_size(px(13.)).line_height(px(19.)).cursor_pointer().hover(|s| s.bg(Theme::hover()));
                match start {
                    Say(text) => line.child(div().flex_1().min_w_0().child(t(text))).on_click(self.on(Cmd::Suggest(false, t(text).to_string()), cx)),
                    Command(name) => {
                        let about = ai::SLASH.iter().find(|slash| slash.name == *name).map_or("", |slash| slash.about);
                        line.child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(Theme::accent()).child(format!("/{name}"))).child(div().flex_1().min_w_0().text_color(Theme::text_secondary()).child(t(about))).on_click(self.on(Cmd::StartCommand(name), cx))
                    }
                }
            });
            div()
                .flex()
                .flex_col()
                .flex_none()
                // A width of its own: a card that shares the row's width is measured before its lines wrap, and is then too short for them.
                .w(px(348.))
                .max_w_full()
                .gap(px(2.))
                .p(px(8.))
                .rounded(px(10.))
                .border_1()
                .border_color(Theme::line())
                .child(div().flex().items_center().gap(px(7.)).px(px(8.)).pt(px(4.)).pb(px(6.)).child(ui::icon(icon, 14., Theme::text_tertiary())).child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text_tertiary()).child(t(title))))
                .children(lines)
        });
        div()
            .id("chat-start")
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .w_full()
            .min_h_0()
            .overflow_y_scroll()
            .gap(px(8.))
            .px(px(24.))
            .pt(px(40.))
            .pb(px(24.))
            .child(ui::icon("sparkle", 30., Theme::text_placeholder()))
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("交给助手")))
            .child(div().max_w(px(600.)).text_size(px(12.)).line_height(px(18.)).text_center().text_color(Theme::text_tertiary()).child(t("交代一件事，或者提一个问题。助手会自己检索和翻阅文献库，回答标出出处；要改动文献库时先问你。")))
            // Two cards to a row, one where the page is narrow.
            .child(div().flex().flex_wrap().justify_center().items_start().gap(px(12.)).w_full().max_w(px(720.)).pt(px(20.)).children(cards))
            .into_any_element()
    }

    /// The papers the library's questions are put to, in a panel that
    /// floats over the conversation: with the labels of this conversation
    /// and what each was handed over as once the material is built; before
    /// that the papers of the current list, which its search box narrows
    /// (and the scope with them). A row opens the paper beside the
    /// conversation; dragged into the conversation, or by its plus, the
    /// paper is one of the few the first question is kept to.
    fn render_materials(&self, chat: &Chat, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        /// The rows drawn; a library of thousands is not a list to scroll through here.
        const LISTED: usize = 300;
        let lib = self.lib.as_ref().unwrap();
        // (paper, its label in this conversation, how its record was given, pages of its text given)
        let rows: Vec<(&wisp_core::model::Paper, String, &str, usize)> = match &chat.ctx {
            Some(ctx) => ctx.given.iter().filter(|given| lib.scope_has(&given.paper_id)).filter_map(|given| Some((lib.paper(&given.paper_id)?, ctx.sources.iter().find(|source| source.paper_id == given.paper_id).map(|source| source.label.clone()).unwrap_or_default(), given.record, given.pages))).collect(),
            None => lib.scope_listed().into_iter().map(|paper| (paper, String::new(), "", 0)).collect(),
        };
        let total = rows.len();
        let all = chat.ctx.as_ref().map_or(lib.shown.len(), |ctx| ctx.given.len());
        let searching = lib.scope_hits.is_some();
        // Before the material is built a paper can be picked for the first question.
        let pinnable = chat.ctx.is_none();
        let note = if chat.ctx.is_some() {
            t("这次对话开始时交给模型的材料。助手还能用工具查到库里的其他文献；在这里点开一篇不会改变范围。").to_string()
        } else if !lib.asked.is_empty() {
            tf!("第一个问题只读选定的 {} 篇。再拖进对话可以添加，在输入框上方可以移除。", "The first question reads only the {} picked papers. Drag more into the conversation to add them; remove them above the question box.", lib.asked.len())
        } else if searching {
            t("第一个问题默认带上找到的这些文献。把其中几篇拖进下面的对话，就只读那几篇。").to_string()
        } else {
            t("第一个问题默认带上当前列表的这些文献。在上面输入可以缩小范围；把其中几篇拖进下面的对话，就只读那几篇。").to_string()
        };
        let workspace = cx.entity().downgrade();
        let listed: Vec<Stateful<Div>> = rows.into_iter().take(LISTED).map(|(paper, label, record, pages)| {
            let by = [paper.authors.split([';', ',']).next().unwrap_or("").trim().to_string(), paper.year.map(|year| year.to_string()).unwrap_or_default()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
            // What the model has of it: the record, and how much of the text.
            let mut state = vec![];
            match record {
                "full" => state.push(t("完整记录").to_string()),
                "title" => state.push(t("仅题目").to_string()),
                "omitted" => state.push(t("未提供").to_string()),
                _ => {}
            }
            if pages > 0 { state.push(tf!("{pages} 页全文", "{pages} full-text pages")) }
            let (text, color) = match paper.fulltext.as_str() {
                "textless" => (t("PDF 无文字"), Theme::warning()),
                "stored" => (t("有全文"), Theme::text_tertiary()),
                _ => (t("无全文"), Theme::text_placeholder()),
            };
            let selected = lib.selected.as_deref() == Some(paper.id.as_str());
            let pinned = pinnable && lib.asked.contains(&paper.id);
            div()
                // By the paper, not the row's place: the list changes under a held row as the search is typed.
                .id(SharedString::from(format!("material-{}", paper.id)))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(selected, |d| d.bg(Theme::accent_soft()))
                .when(!selected, |d| d.hover(|s| s.bg(Theme::hover())))
                .on_click(self.on(Cmd::Select(paper.id.clone()), cx))
                .when(pinnable, |d| {
                    let workspace = workspace.clone();
                    d.on_drag(PaperDrag { id: paper.id.clone(), title: paper.title.clone(), held: Point::default() }, move |drag, held, _, cx| {
                        workspace.update(cx, |this, cx| {
                            this.pinning = true;
                            cx.notify();
                        }).ok();
                        cx.new(|_| PaperDrag { held, ..drag.clone() })
                    })
                })
                .when(!label.is_empty(), |d| d.child(div().flex_none().w(px(34.)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::accent()).child(label)))
                .child(div().flex_1().min_w_0().truncate().child(paper.title.clone()))
                .when(!by.is_empty(), |d| d.child(div().flex_none().max_w(px(150.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(by)))
                .when(!state.is_empty(), |d| d.child(div().flex_none().text_size(px(12.)).text_color(if record == "full" { Theme::text_secondary() } else { Theme::warning() }).child(state.join(" · "))))
                .child(div().flex_none().w(px(68.)).text_right().text_size(px(12.)).text_color(color).child(text))
                // The same as dragging the row into the conversation, for a hand that would rather click.
                .when(pinnable, |d| {
                    let (id, tip) = (paper.id.clone(), if pinned { t("不再限定这一篇") } else { t("只读这一篇（也可以把这一行拖进对话）") });
                    d.child(
                        div()
                            .id(SharedString::from(format!("material-pin-{}", paper.id)))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(22.))
                            .rounded(px(6.))
                            .when(pinned, |d| d.bg(Theme::accent_soft()))
                            .hover(|s| s.bg(Theme::pressed()))
                            .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(tip.to_string())).into())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // Not the row's click as well, which would open the paper.
                                cx.stop_propagation();
                                this.run(Cmd::Scope(if pinned { ScopeDo::Unpin(id.clone()) } else { ScopeDo::Pin(id.clone()) }), window, cx)
                            }))
                            .child(ui::icon(if pinned { "check" } else { "plus" }, 13., if pinned { Theme::accent() } else { Theme::text_tertiary() })),
                    )
                })
        }).collect();
        let panel = div()
            .absolute()
            .top(px(6.))
            .left(px(12.))
            .right(px(12.))
            .max_h(relative(0.62))
            // The conversation under it hears nothing through it.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .shadow(theme::shadow_panel())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .mx(px(8.))
                    .mt(px(8.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .bg(Theme::bg_sunken())
                    .border_1()
                    .border_color(if lib.scope_find.focus_handle(cx).is_focused(window) { Theme::accent() } else { Theme::bg_sunken() })
                    .child(ui::icon("search", 15., Theme::text_tertiary()))
                    .child(div().flex().flex_1().min_w_0().child(lib.scope_find.clone()))
                    .when(searching, |d| d.child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(format!("{total} / {all}"))).child(ui::ghost_button("materials-find-clear", Some("x"), None).h(px(22.)).min_w(px(22.)).on_click(self.on(Cmd::Scope(ScopeDo::ClearFind), cx)))),
            )
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
                    .when(total == 0, |d| d.child(div().px(px(8.)).py(px(10.)).text_color(Theme::text_tertiary()).child(if searching { t("没有符合的文献。换个词，或清空搜索。") } else { t("当前列表里没有文献。") })))
                    .when(total > LISTED, |d| d.child(div().px(px(8.)).py(px(6.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("另有 {} 篇没有列出；再输入几个词可以找到。", "{} more papers not listed here; type a few more words to find them.", total - LISTED)))),
            );
        ui::fade_in("materials-in", panel).into_any_element()
    }

    /// What was said, in order: questions, the assistant's tool calls
    /// folded by run, the pieces of its answers with their sources.
    fn render_messages(&self, about_paper: bool, chat: &Chat, review: Option<&wisp_core::review::Review>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let side = if about_paper { "paper" } else { "library" };
        let mut items: Vec<AnyElement> = vec![];
        // An answer can be added to a manuscript once there is one.
        let has_manuscript = self.lib.as_ref().is_some_and(|lib| !lib.writing.manuscripts.is_empty());
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
                        .child(if chat.steps > 1 { t("正在整理查到的内容…") } else { t("正在阅读文献并思考…") })
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
                    let (mark, verdict, color) = quote_verdict(check).unwrap_or(("file-text", String::new(), Theme::accent()));
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
                let label = if open { t("收起").to_string() } else { tf!("另外 {} 个出处", "{} more sources", total - shown) };
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
                                .child(ui::ghost_button(SharedString::from(format!("picked-copy-{side}-{i}")), Some("copy"), Some(t("复制所选").into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::CopyPicked(about_paper), cx)))
                                .child(ui::ghost_button(SharedString::from(format!("picked-quote-{side}-{i}")), Some("pen-line"), Some(t("引用到输入框").into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::QuotePicked(about_paper), cx)))
                                .when(!about_paper && review.is_none() && has_manuscript, |d| d.child(ui::ghost_button(SharedString::from(format!("picked-insert-{side}-{i}")), Some("plus"), Some(t("所选插入稿件").into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on_menu(MenuKind::InsertInto(None), cx))))
                                // In a review's conversation, the part selected can be kept in place of the whole answer.
                                .when_some(review, |d, review| {
                                    let text = chat.picked_text().unwrap_or_default();
                                    let (label, what) = match review.step() { 1 => (t("所选记为要求"), crate::app::ReviewDo::Requirements), 2 => (t("所选记为初步框架"), crate::app::ReviewDo::Framework), 3 => (t("所选记为大纲"), crate::app::ReviewDo::Outline), _ => (t("所选存为正文"), crate::app::ReviewDo::Section) };
                                    d.child(ui::ghost_button(SharedString::from(format!("picked-review-{side}-{i}")), Some("check"), Some(label.into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::Review(what, text.clone()), cx)))
                                        .when(review.step() == 4, |d| d.child(ui::ghost_button(SharedString::from(format!("picked-evidence-{side}-{i}")), Some("plus"), Some(t("记为论断").into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::EditReviewEvidence(None, text), cx))))
                                }),
                        )
                    })
                    .when(!chips.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(chips)))
                    .when_some(whole, |d, (question, whole)| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .flex_wrap()
                                .gap(px(2.))
                                .child(ui::ghost_button(SharedString::from(format!("copy-{side}-{i}")), Some("copy"), None).h(px(24.)).min_w(px(24.)).opacity(0.7).on_click(self.on(Cmd::CopyText(whole.clone()), cx)))
                                // A review's answers go to its record, step by step; any other answer about the library can become a manuscript as it is.
                                .when(!about_paper && review.is_none() && !whole.trim().is_empty(), |d| d.child(ui::ghost_button(SharedString::from(format!("manuscript-{side}-{i}")), Some("pen-line"), Some(t("保存为新稿件").into())).h(px(24.)).text_size(px(12.)).opacity(0.7).on_click(self.on(Cmd::AnswerToManuscript(question.clone(), whole.clone()), cx))))
                                .when(!about_paper && review.is_none() && has_manuscript && !whole.trim().is_empty(), |d| d.child(ui::ghost_button(SharedString::from(format!("insert-{side}-{i}")), Some("plus"), Some(t("插入到稿件").into())).h(px(24.)).text_size(px(12.)).opacity(0.7).on_click(self.on_menu(MenuKind::InsertInto(Some(whole.clone())), cx))))
                                .when_some(review, |d, review| {
                                    use crate::app::ReviewDo;
                                    let mut button = |key: &'static str, label: &'static str, what: ReviewDo| ui::ghost_button(SharedString::from(format!("review-{key}-{side}-{i}")), Some("check"), Some(label.into())).h(px(24.)).text_size(px(12.)).opacity(0.7).on_click(self.on(Cmd::Review(what, whole.clone()), cx));
                                    // Only what the step in hand takes: the rest is changed in the record.
                                    match review.step() {
                                        1 => d.child(button("requirements", t("记为要求"), ReviewDo::Requirements)),
                                        2 => d.child(button("framework", t("记为初步框架"), ReviewDo::Framework)),
                                        3 => d.child(button("outline", t("记为大纲"), ReviewDo::Outline)),
                                        _ => d.child(button("section", t("保存正文"), ReviewDo::Section)).when(!review.sections.is_empty(), |d| d.child(button("checks", t("记录检查"), ReviewDo::Checks))),
                                    }
                                })
                                // An answer about one paper can join that paper's notes; one about the library names many.
                                .when(about_paper, |d| d.child(ui::ghost_button(SharedString::from(format!("note-{side}-{i}")), Some("pen-line"), Some(t("存为笔记").into())).h(px(24.)).text_size(px(12.)).opacity(0.7).on_click(self.on(Cmd::AnswerToNote(question, whole), cx)))),
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
                    .child(div().flex().items_center().gap(px(6.)).font_weight(FontWeight::MEDIUM).child(ui::icon("info", 14., Theme::accent())).child(t("助手想改动文献库，等你同意")))
                    .children(pending.calls.iter().enumerate().filter(|(_, call)| call.confirm).map(|(n, call)| {
                        let line = div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(call.line.clone());
                        if call.details.is_empty() { return div().pl(px(20.)).child(line) }
                        // What it would change, paper by paper: all of it, in a box that scrolls when it is long.
                        let rows = call.details.iter().map(|detail| {
                            let (title, change) = detail.split_once('：').unwrap_or((detail.as_str(), ""));
                            div().flex().flex_col().py(px(3.)).text_size(px(12.)).line_height(px(18.)).child(div().truncate().text_color(Theme::text_tertiary()).child(title.to_string())).child(div().text_color(Theme::text()).child(change.to_string()))
                        });
                        div().pl(px(20.)).flex().flex_col().gap(px(4.)).child(line).child(div().id(SharedString::from(format!("consent-details-{side}-{n}"))).flex().flex_col().max_h(px(240.)).overflow_y_scroll().px(px(10.)).py(px(4.)).rounded(px(8.)).bg(Theme::bg_body()).border_1().border_color(Theme::line()).children(rows))
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .pl(px(20.))
                            .child(button("once", t("允许"), Consent::Once, self, cx))
                            .child(button("always", t("这次对话里都允许"), Consent::Always, self, cx))
                            .child(button("deny", t("不允许"), Consent::Deny, self, cx)),
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
                        .when(failed > 0, |d| d.child(div().flex_none().text_color(Theme::danger()).child(tf!("· {failed} 项没有成功", "· {failed} items failed"))))
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
            return ui::empty_state("history", t("还没有历史对话"), t("提过的问题和回答会留在这个文献库里，之后可以在这里找到，接着问下去。")).into_any_element();
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
                                    .child(tf!("{} · {} 个问题", "{} · {} questions", time::short_time(&conversation.updated_at), conversation.questions))
                                    .when(current, |d| d.child(div().text_color(Theme::accent()).child(t("正在进行")))),
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
        // The papers the first question is kept to, while it is still to come; and a paper on its way in.
        let open = !about_paper && chat.ctx.is_none() && chat.messages.is_empty() && chat.history.is_none();
        let kept: Vec<&wisp_core::model::Paper> = if open { lib.asked.iter().take(PINNED_SHOWN).filter_map(|id| lib.paper(id)).collect() } else { vec![] };
        let dropping = open && self.pinning && cx.has_active_drag();
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
                                .when(!command.hint.is_empty(), |d| d.child(div().flex_none().text_color(Theme::text_placeholder()).child(t(&command.hint).to_string())))
                                .child(div().flex_1().min_w_0().truncate().text_right().text_color(Theme::text_secondary()).child(if command.custom { command.about.clone() } else { t(&command.about).to_string() }))
                                // One the person wrote says so: what it asks is theirs, not the app's.
                                .when(command.custom, |d| d.child(div().flex_none().px(px(6.)).rounded(px(5.)).bg(Theme::groove()).text_size(px(11.)).text_color(Theme::text_tertiary()).child(t("自定义"))))
                        }))
                        .child(div().px(px(8.)).pt(px(5.)).pb(px(2.)).mt(px(3.)).border_t_1().border_color(Theme::line()).text_size(px(11.)).text_color(Theme::text_placeholder()).child(t("↑↓ 选择 · Enter 或 Tab 确认 · Esc 关闭"))),
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
                    .border_color(if focused || dropping { Theme::accent() } else { Theme::line_strong() })
                    .bg(if dropping { Theme::accent_soft() } else { Theme::bg_body() })
                    .shadow(theme::shadow_card())
                    // A paper held over the conversation: what letting go of it does.
                    .when(dropping, |d| d.child(div().flex().items_center().gap(px(6.)).px(px(4.)).pt(px(2.)).text_size(px(12.)).text_color(Theme::accent()).child(ui::icon("plus", 13., Theme::accent())).child(t("松开，第一个问题只读拖进来的文献"))))
                    // The papers the first question is kept to: each can be let go again, and without any the list on show is back.
                    .when(!kept.is_empty(), |d| {
                        let more = lib.asked.len().saturating_sub(kept.len());
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.))
                                .px(px(2.))
                                .pt(px(2.))
                                .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("只读这 {} 篇", "Only these {}", lib.asked.len())))
                                .children(kept.iter().map(|paper| {
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .gap(px(2.))
                                        .h(px(24.))
                                        .pl(px(8.))
                                        .pr(px(2.))
                                        .rounded(px(6.))
                                        .bg(Theme::accent_soft())
                                        .text_size(px(12.))
                                        .child(div().max_w(px(226.)).truncate().child(paper.title.clone()))
                                        .child(ui::ghost_button(SharedString::from(format!("scope-unpin-{}", paper.id)), Some("x"), None).h(px(20.)).min_w(px(20.)).on_click(self.on(Cmd::Scope(ScopeDo::Unpin(paper.id.clone())), cx)))
                                }))
                                .when(more > 0, |d| d.child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("另外 {more} 篇", "{more} more"))))
                                .child(ui::ghost_button("scope-unpin-all", None, Some(t("清除").into())).h(px(22.)).text_size(px(12.)).on_click(self.on(Cmd::Scope(ScopeDo::UnpinAll), cx))),
                        )
                    })
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
                                        .child(div().truncate().text_size(px(11.)).text_color(Theme::text_tertiary()).child(tf!("原文 · 第 {} 页 · {title}", "Original · Page {} · {title}", quote.page))),
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
            .child(div().flex().gap(px(8.)).px(px(4.)).text_size(px(11.)).text_color(Theme::text_placeholder()).child(div().flex_1().min_w_0().truncate().child(if about_paper { t("Enter 发送 · 回答可能有误，请对照原文") } else { t("Enter 发送，Shift+Enter 换行 · 回答可能有误，请对照原文") })).child(div().flex_none().max_w(relative(0.4)).truncate().child(model.to_string())))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::tools_summary;

    #[test]
    fn a_kept_answer_names_its_section() {
        use wisp_core::review::{Review, ReviewSection};
        let mut review = Review::default();
        assert_eq!(super::section_of("## 一、引言\n\n正文一段。\n", &review), ("一、引言".to_string(), "正文一段。".to_string()));
        // Without a heading it is numbered; a heading with nothing under it is the text itself.
        assert_eq!(super::section_of("正文一段。", &review), ("正文 1".to_string(), "正文一段。".to_string()));
        assert_eq!(super::section_of("## 只有标题", &review).0, "正文 1");
        // Kept again, it is another section: a click overwrites nothing.
        review.sections.push(ReviewSection { name: "一、引言".into(), body: "旧".into() });
        assert_eq!(super::section_of("## 一、引言\n新", &review).0, "一、引言 (2)");
    }

    #[test]
    fn a_question_carries_where_the_review_stands_and_not_its_text() {
        use wisp_core::review::{Review, ReviewSection};
        let mut review = Review { requirements: "中文，600 字".into(), ..Default::default() };
        assert!(super::review_note(&review).contains("写作要求尚未确认") && super::review_note(&review).contains("中文，600 字"));
        review.requirements_confirmed = true;
        review.outline = "一、引言".into();
        assert!(super::review_note(&review).contains("大纲尚未确认"));
        review.outline_confirmed = true;
        review.sections.push(ReviewSection { name: "一、引言".into(), body: "很长的正文".into() });
        let note = super::review_note(&review);
        assert!(note.contains("已由用户确认") && note.contains("已保存的章节：一、引言") && !note.contains("很长的正文"));
    }

    #[test]
    fn tool_runs_in_a_line() {
        assert_eq!(tools_summary(&["检索全文：chromium", "查看概况", "检索全文：10x · 第 2 页起"]), "检索全文 ×2 · 查看概况");
        assert_eq!(tools_summary(&[]), "");
    }
}
