//! Notion connection settings and the explicit preview → save action.

use crate::{app::{Cmd, Modal, Workspace}, i18n::t, input::{InputEvent, TextInput}, theme::{Theme, text_px}, ui};
use gpui::{prelude::FluentBuilder, *};
use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use wisp_core::notion;

#[derive(Default)]
pub struct State {
    pub busy: bool,
    pub connected: bool,
    pub account: Option<wisp_net::notion::Account>,
    pub status: String,
    pub account_details: bool,
}

pub struct Fields {
    parent: Entity<TextInput>,
    root: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

impl Fields {
    pub fn new(settings: &notion::Settings, cx: &mut Context<Workspace>) -> Self {
        let parent = cx.new(|cx| TextInput::new("https://app.notion.com/p/…", cx).with_text(&settings.parent_url));
        let root = cx.new(|cx| TextInput::new("WispPaper笔记", cx).with_text(&settings.root_title));
        let subscriptions = [&parent, &root].into_iter().map(|input| cx.subscribe(input, |this, _, event: &InputEvent, cx| {
            if *event == InputEvent::Changed { this.save_notion_settings(cx); }
        })).collect();
        Self { parent, root, _subscriptions: subscriptions }
    }
}

pub struct Form {
    pub job: wisp_net::notion::Save,
    pub selected: Vec<bool>,
    order: Vec<usize>,
    pub editors: Vec<Entity<TextInput>>,
    pub stage: Stage,
    pub direct: bool,
    pub histories: Vec<History>,
    pub title: Entity<TextInput>,
    pub states: Vec<notion::EntryState>,
    pub refinements: Vec<Refinement>,
    pub refine_busy: bool,
    refine_session: Arc<()>,
    refine_cancel: Arc<AtomicBool>,
    refine_generation: u64,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage { Histories, Questions, Refining, Editing, Titling }

pub struct Refinement {
    pub input: Entity<TextInput>,
    pub use_refined: bool,
    pub pending: bool,
    pub error: String,
    pub model: String,
    applied: String,
}

pub struct History {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub current: bool,
    pub chosen: bool,
    pub loaded: bool,
    pub range: std::ops::Range<usize>,
    pub questions: usize,
}

impl Form {
    pub fn active(&self) -> Vec<usize> {
        self.order.iter().copied().filter(|i| self.histories.is_empty() || self.histories.iter().any(|h| h.chosen && h.range.contains(i))).collect()
    }
    pub fn move_question(&mut self, i: usize, down: bool) {
        let active = self.active();
        let Some(at) = active.iter().position(|&n| n == i) else { return };
        let Some(&other) = at.checked_add_signed(if down { 1 } else { -1 }).and_then(|p| active.get(p)) else { return };
        let a = self.order.iter().position(|&n| n == i).unwrap();
        let b = self.order.iter().position(|&n| n == other).unwrap();
        self.order.swap(a,b);
    }
    pub fn count(&self) -> usize { self.active().into_iter().filter(|i| self.selected[*i]).count() }
    pub fn ordinal(&self, index: usize) -> usize {
        self.active().iter().position(|&i| i == index).unwrap_or(index) + 1
    }
    pub fn image_count(&self, cx: &App) -> usize {
        self.active().into_iter().filter(|i| self.selected[*i]).flat_map(|i| {
            let entry = &self.job.entries[i];
            let images = entry.images.iter().filter(|id| !matches!(self.stage, Stage::Editing | Stage::Refining | Stage::Titling) || self.editors[i].read(cx).text().contains(&format!("![原图](wisppaper-image://{id})"))).cloned().collect::<Vec<_>>();
            images.into_iter().map(|id| (entry.group.as_ref().map(|g| g.id.clone()).unwrap_or_default(), id))
        }).collect::<BTreeSet<_>>().len()
    }
    fn append_entries(&mut self, entries: Vec<notion::Entry>, states: Vec<notion::EntryState>, cx: &mut Context<Workspace>) {
        for (entry, state) in entries.into_iter().zip(states) {
            let input = cx.new(|cx| TextInput::new(t("点击 AI 提炼生成，也可以直接输入修改后的问题…"), cx).multiline(3));
            let (at, session) = (self.refinements.len(), self.refine_session.clone());
            self._subscriptions.push(cx.subscribe(&input, move |this, _, event: &InputEvent, cx| {
                if *event == InputEvent::Changed {
                    if let Some(Modal::Notion(form)) = &mut this.modal && Arc::ptr_eq(&session, &form.refine_session) && let Some(refinement) = form.refinements.get_mut(at) { refinement.error.clear(); }
                    this.modal_error.clear();
                    cx.notify();
                }
            }));
            self.refinements.push(Refinement { input, use_refined: false, pending: false, error: String::new(), model: String::new(), applied: entry.label.clone() });
            let editor = cx.new(|cx| TextInput::new(t("编辑笔记 Markdown…"), cx).multiline(8).markdown().with_text(&if entry.body.is_empty() { entry.draft() } else { entry.body.clone() }));
            self._subscriptions.push(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| { if *event == InputEvent::Changed { this.modal_error.clear(); cx.notify(); } }));
            self.editors.push(editor);
            self.selected.push(state == notion::EntryState::New);
            self.states.push(state);
            notion::insert_in_time_order(&mut self.order, &self.job.entries, &entry);
            self.job.entries.push(entry);
        }
    }
    fn stop_refining(&mut self) {
        self.refine_cancel.store(true, Ordering::Relaxed);
        self.refine_generation += 1;
        self.refine_busy = false;
        for refinement in &mut self.refinements { refinement.pending = false; }
    }
    fn apply_refinements(&mut self, cx: &mut Context<Workspace>) -> Result<(), String> {
        let changes = self.active().into_iter().filter(|i| self.selected[*i]).map(|i| {
            let choice = &self.refinements[i];
            let question = if choice.use_refined { wisp_core::question::clean(choice.input.read(cx).text())? } else { self.job.entries[i].label.clone() };
            let (body, applied) = wisp_core::question::replace_heading(self.editors[i].read(cx).text(), &choice.applied, &question)?;
            Ok((i, body, applied))
        }).collect::<Result<Vec<_>, String>>()?;
        for (i, body, applied) in changes {
            if self.editors[i].read(cx).text() != body { self.editors[i].update(cx, |input, cx| input.set_text(&body, cx)); }
            self.refinements[i].applied = applied;
        }
        Ok(())
    }
}

impl Drop for Form {
    fn drop(&mut self) { self.refine_cancel.store(true, Ordering::Relaxed); }
}

impl Workspace {
    pub fn save_notion_settings(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.settings_view else { return };
        let fields = &view.notion;
        let settings = &mut self.settings.notion;
        settings.parent_url = fields.parent.read(cx).text().trim().into();
        settings.root_title = fields.root.read(cx).text().trim().into();
        self.save_settings(cx);
        cx.notify();
    }

    pub fn notion_field(&mut self, field: String, movement: i32, cx: &mut Context<Self>) {
        let fields = &mut self.settings.notion.fields;
        if let Some(i) = fields.iter().position(|f| f == &field) {
            if movement == 0 { fields.remove(i); } else { let next = (i as i32 + movement).clamp(0, fields.len() as i32 - 1) as usize; fields.swap(i, next); }
        } else if movement == 0 { fields.push(field); }
        self.save_settings(cx);
        cx.notify();
    }

    pub fn notion_status(&mut self, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        self.notion.status = t("正在检查 Notion 账户…").into();
        self.notion.account = None;
        self.notion.connected = false;
        self.notion.busy = true;
        let path = self.settings_path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { wisp_net::notion::account_info(&path) }).await;
            this.update(cx, |this, cx| {
                this.notion.busy = false;
                match result { Ok(account) => { this.notion.connected = true; this.notion.account = Some(account); this.notion.status.clear(); }, Err(error) => { this.notion.connected = false; this.notion.account = None; this.notion.status = error; } }
                cx.notify();
            }).ok();
        }).detach();
    }

    pub fn check_notion(&mut self, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        self.notion.busy = true;
        self.notion.status = t("正在检查 Notion 连接与目标页面…").into();
        let path = self.settings_path.clone();
        let settings = self.settings.notion.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { wisp_net::notion::check(&path, &settings) }).await;
            this.update(cx, |this, cx| {
                this.notion.busy = false;
                this.notion.status = match result { Ok(account) => { this.notion.connected = true; this.notion.account = Some(account); t("连接和必要工具可用；尚未创建或写入页面。").into() }, Err(error) => { this.notion.connected = false; this.notion.account = None; error } };
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }

    pub fn notion_title_mode(&mut self, custom: bool, cx: &mut Context<Self>) {
        self.settings.notion.title_mode = if custom { "custom" } else { "parts" }.into();
        self.save_settings(cx);
        cx.notify();
    }

    pub fn notion_title_part(&mut self, part: String, movement: i32, cx: &mut Context<Self>) {
        self.settings.notion.title_mode = "parts".into();
        let parts = &mut self.settings.notion.title_parts;
        if let Some(i) = parts.iter().position(|p| p == &part) {
            if movement == 0 { parts.remove(i); }
            else { let next = (i as i32 + movement).clamp(0, parts.len() as i32 - 1) as usize; parts.swap(i, next); }
        } else if movement == 0 { parts.push(part); }
        self.save_settings(cx);
        cx.notify();
    }

    pub fn notion_account_card(&self) -> Div {
        let card = div().flex().flex_col().w_full().gap(px(12.)).p(px(18.)).rounded(px(12.)).border_1().border_color(Theme::line()).bg(Theme::bg_sunken());
        match &self.notion.account {
            Some(account) => card
                .child(div().flex().items_center().gap(px(12.)).child(notion_logo(40.))
                    .child(div().flex().flex_col().flex_1().min_w_0().gap(px(4.))
                        .child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(t("接收笔记的 Notion 账户")))
                        .child(div().flex().flex_wrap().items_center().gap(px(8.))
                            .child(div().text_size(text_px(16.)).font_weight(FontWeight::SEMIBOLD).child(if account.name.is_empty() { t("Notion 未提供账户名称").to_string() } else { account.name.clone() }))
                            .child(ui::chip(t("已连接"), Theme::success(), Theme::bg_body())))
                        .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(if account.email.is_empty() { t("Notion 未提供邮箱").to_string() } else { account.email.clone() }))))
                .child(div().flex().flex_wrap().items_center().gap(px(8.)).pt(px(10.)).border_t_1().border_color(Theme::line())
                    .child(ui::icon("folder-open", 14., Theme::text_secondary()))
                    .child(div().flex_1().min_w_0().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(tf!("工作区：{}", "Workspace: {}", if account.workspace_name.is_empty() { &account.workspace_id } else { &account.workspace_name }))))
                .when(self.notion.account_details, |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(tf!("账户标识：{}", "Account ID: {}", account.user_id)))),
            None => card.child(div().flex().items_center().gap(px(12.)).child(notion_logo(40.))
                .child(div().flex().flex_col().gap(px(4.)).flex_1().min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(t("连接你的 Notion")))
                    .child(div().text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_secondary()).child(t("连接后，可将阅读问答和我的笔记保存到指定页面。"))))),
        }
    }

    pub fn render_notion_settings(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let fields = &view.notion;
        let settings = &self.settings.notion;
        let width = self.column_width(window);
        let inner = width - 38.;
        // A concrete width is required by GPUI's custom text input inside a form row.
        let field = |label: &'static str, input: &Entity<TextInput>, cx: &App| ui::form_row(t(label), ui::field(input, window, cx).w(px(inner))).w(px(inner)).flex_none();
        let button = |id: &'static str, text: &'static str, cmd: Cmd, cx: &mut Context<Self>| ui::secondary_button(id, t(text)).when(!self.notion.busy, |d| d.on_click(self.on(cmd, cx)));
        let custom = settings.custom_title();
        let parts = settings.title_parts.iter().enumerate().map(|(i, part)| {
            let label = notion::TITLE_PARTS.iter().find(|(key, _)| key == part).map_or(part.as_str(), |(_, label)| *label);
            div().flex().items_center().gap(px(5.)).pl(px(8.)).pr(px(3.)).h(text_px(30.)).rounded(px(7.)).border_1().border_color(Theme::line()).bg(Theme::bg_sunken())
                .child(div().text_size(text_px(10.)).text_color(Theme::text_tertiary()).child(format!("{}", i + 1)))
                .child(div().text_size(text_px(12.)).child(t(label).to_string()))
                .child(reorder_arrow(SharedString::from(format!("notion-title-up-{part}")), "chevron-left", i > 0).when(i > 0, |d| d.on_click(self.on(Cmd::NotionTitlePart(part.clone(), -1), cx))))
                .child(reorder_arrow(SharedString::from(format!("notion-title-down-{part}")), "chevron-right", i + 1 < settings.title_parts.len()).when(i + 1 < settings.title_parts.len(), |d| d.on_click(self.on(Cmd::NotionTitlePart(part.clone(), 1), cx))))
        }).collect::<Vec<_>>();
        let sample = wisp_core::model::Paper { title: t("文献题目").into(), journal: t("期刊名称").into(), year: Some(2026), ..Default::default() };
        let sample = self.lib.as_ref().and_then(|lib| lib.selected_paper()).unwrap_or(&sample);
        let library = wisp_core::libraries::LibraryInfo { id: "L-example".into(), name: t("示例文献库").into(), ..Default::default() };
        let library = self.lib.as_ref().map(|lib| &lib.info).unwrap_or(&library);
        let example = notion::Entry { asked_at: String::new(), key: "example".into(), source_key: "example".into(), group: None, label: t("这图什么意思").into(), original_question: t("这图什么意思").into(), question: t("这图什么意思").into(), answer: t("这里显示 AI 回答。你可以在保存前编辑文字，后续问答追加在同一文献笔记中。").into(), images: vec!["example-image".into()], body: String::new() };
        let images = BTreeMap::from([("example-image".into(), format!("> {}", t("原图（示意；实际保存为图片附件）")))]);
        let preview = format!("{}{}", settings.header(library, Some(sample)), example.markdown(&images).unwrap_or_default());
        let account = self.notion_account_card()
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(button("notion-connector-settings", "配置连接器", Cmd::OpenSettings(crate::settings::SettingsPage::Customize), cx))
                .child(button("notion-check", "测试连接", Cmd::CheckNotion, cx))
                .when(self.notion.connected, |d| d.child(button("notion-account-details", if self.notion.account_details { "收起详情" } else { "账户详情" }, Cmd::NotionAccountDetails, cx))))
            .when(!self.notion.status.is_empty(), |d| d.child(div().text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_secondary()).child(self.notion.status.clone())));
        let destination = settings_card("folder-open", "保存位置", "每篇文献一个笔记页面，后续问答持续追加。", width)
            .child(field("Notion 父页面访问链接（可留空）", &fields.parent, cx))
            .child(hint("留空时保存到 Notion 私人首页；填入同名根页面的链接时直接复用。"))
            .child(field("根页面名称（可留空）", &fields.root, cx))
            .child(hint("留空则省略根页面，直接在指定父页面或 Notion 私人首页下按文献库归档。"))
            .child(div().flex().flex_col().gap(px(7.)).p(px(12.)).rounded(px(8.)).bg(Theme::accent_soft()).line_height(text_px(19.))
                .child(div().flex().items_center().gap(px(6.)).text_size(text_px(11.)).text_color(Theme::text_secondary()).child(ui::icon("folder-open", 13., Theme::accent())).child(t("页面组织示例")))
                .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(if settings.parent_url.is_empty() { t("Notion 私人首页") } else if settings.root_title.trim().is_empty() { t("指定父页面") } else { t("指定父页面（同名根页面直接复用）") }))
                .when(!settings.root_title.trim().is_empty(), |d| d.child(div().pl(px(10.)).font_weight(FontWeight::MEDIUM).child(format!("└─ {}", settings.root_title))))
                .child(div().pl(px(if settings.root_title.trim().is_empty() { 10. } else { 22. })).text_size(text_px(12.)).child(format!("└─ {}", library.name)))
                .child(div().pl(px(if settings.root_title.trim().is_empty() { 22. } else { 34. })).text_size(text_px(12.)).text_color(Theme::text_secondary()).child(format!("└─ {}", if custom { t("保存前手动输入的标题").to_string() } else { settings.paper_title(sample) }))))
            .child(hint("按文献库自动归档；库级提问进入「文献库问答」。"));
        let title = settings_card("file-text", "文献笔记标题", "选择组合字段，或在每篇文献保存前手动输入标题。", width)
            .child(div().flex().flex_wrap().gap(px(6.))
                .child(choice("notion-title-parts", "组合字段", !custom).on_click(self.on(Cmd::NotionTitleMode(false), cx)))
                .child(choice("notion-title-custom", "自定义输入", custom).on_click(self.on(Cmd::NotionTitleMode(true), cx))))
            .when(!custom, |d| d
                .child(div().flex().flex_wrap().gap(px(7.)).children(notion::TITLE_PARTS.iter().map(|(key, label)| choice(SharedString::from(format!("notion-title-part-{key}")), label, settings.title_parts.iter().any(|s| s == key)).on_click(self.on(Cmd::NotionTitlePart((*key).into(), 0), cx)))))
                .child(div().flex().flex_col().gap(px(8.))
                    .child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(t("排列顺序 · 点击箭头调整")))
                    .child(div().flex().flex_wrap().gap(px(6.)).children(parts))))
            .when(custom, |d| d.child(hint("选择自定义输入后，点击最终保存按钮时会弹出标题窗口，逐篇填写；不会将统一模板套用到所有文献。库级问答仍使用「文献库问答」。")))
            .when(!custom, |d| d.child(div().flex().flex_col().gap(px(5.)).p(px(12.)).rounded(px(8.)).bg(Theme::bg_sunken())
                .child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(t("标题预览")))
                .child(div().font_weight(FontWeight::MEDIUM).line_height(text_px(21.)).child(settings.paper_title(sample)))))
            .when(!custom, |d| d.child(hint("缺失的组成项会跳过。较长的页面名自动截断，正文仍保留完整题目；已有笔记页面的标题不自动改写。")));
        let metadata = settings_card("list", "文献基本信息", "勾选笔记顶部需要保留的信息。缺失字段自动跳过。", width)
            .child(div().flex().flex_wrap().gap(px(7.)).children(notion::FIELDS.iter().map(|(key, label)| choice(SharedString::from(format!("notion-field-{key}")), label, settings.fields.iter().any(|s| s == key)).on_click(self.on(Cmd::NotionField((*key).into(), 0), cx)))))
            .child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(t("排列顺序 · 点击箭头调整")))
            .child(div().flex().flex_wrap().gap(px(6.)).children(settings.fields.iter().enumerate().map(|(i, field)| {
                let label = notion::FIELDS.iter().find(|(key, _)| key == field).map_or(field.as_str(), |(_, label)| *label);
                div().flex().items_center().gap(px(5.)).pl(px(8.)).pr(px(3.)).h(text_px(30.)).rounded(px(7.)).border_1().border_color(Theme::line()).bg(Theme::bg_sunken())
                    .child(div().text_size(text_px(10.)).text_color(Theme::text_tertiary()).child(format!("{}", i + 1)))
                    .child(div().text_size(text_px(12.)).child(t(label).to_string()))
                    .child(reorder_arrow(SharedString::from(format!("notion-field-up-{field}")), "chevron-left", i > 0).when(i > 0, |d| d.on_click(self.on(Cmd::NotionField(field.clone(), -1), cx))))
                    .child(reorder_arrow(SharedString::from(format!("notion-field-down-{field}")), "chevron-right", i + 1 < settings.fields.len()).when(i + 1 < settings.fields.len(), |d| d.on_click(self.on(Cmd::NotionField(field.clone(), 1), cx))))
            })))
            .child(hint("出版日期按已有精度显示；完整日期显示为 YYYY-MM-DD，缺失的月、日不会补造。旧记录缺少出版日期时可在编辑文献中补充。"));
        let example = settings_card("image", "保存效果示例", "Q 是问题标题，原图与 A 正文排列在其下。", width)
            .child(div().flex().items_center().gap(px(6.)).child(ui::chip(t("实时预览"), Theme::accent(), Theme::accent_soft())).child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(t("仅本机示例，不会写入 Notion"))))
            .child(div().p(px(16.)).rounded(px(9.)).bg(Theme::bg_sunken()).border_1().border_color(Theme::line())
                .child(div().pb(px(14.)).mb(px(14.)).border_b_1().border_color(Theme::line()).text_size(text_px(17.)).font_weight(FontWeight::SEMIBOLD).child(settings.paper_title(sample)))
                .child(crate::markdown::render("notion-settings-example", &preview, None, None)));
        div().flex().flex_col().w(px(width)).gap(px(18.))
            .child(div().flex().items_center().justify_between().gap(px(12.))
                .child(ui::ghost_button("notion-back-to-plugin", Some("arrow-left"), Some(t("返回").into())).on_click(self.on(Cmd::OpenSettings(crate::settings::SettingsPage::Customize), cx)))
                .child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("更改后自动保存"))))
            .child(account).child(destination).child(title).child(metadata).child(example)
            .when_some(settings.validate().err(), |d, error| d.child(ui::error_line(&error)))
            .into_any_element()
    }

    pub fn preview_notion(&mut self, paper: bool, only: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if self.notion.busy { self.fail(t("Notion 操作尚未完成，请稍后再试。"), cx); return }
        if let Err(error) = self.settings.notion.validate() { self.fail(error, cx); return }
        let Some(lib) = &self.lib else { return };
        let chat = lib.chat(if paper { crate::app::Talk::Paper } else { crate::app::Talk::Library });
        if chat.busy { self.fail(t("请等回答完成后再保存到 Notion。"), cx); return }
        let transcript = chat.transcript();
        let stored = match lib.store.conversation(&chat.id) { Ok(stored) => stored, Err(error) => { self.fail(error.to_string(), cx); return } };
        let current = wisp_core::store::Conversation { id: chat.id.clone(), transcript, ..stored.unwrap_or_default() };
        let entries = notion::conversation_entries(&current, only);
        if only.is_some() && entries.is_empty() { self.fail(t("没有可保存的完整问答。"), cx); return }
        let selected = if paper { lib.selected_paper().cloned() } else { None };
        if paper && selected.is_none() { return }
        let mut histories = match lib.store.list_conversations(selected.as_ref().map_or("", |p| p.id.as_str())) {
            Ok(histories) => histories.into_iter().filter(|h| only.is_none() || h.id == current.id).map(|h| {
                let is_current = h.id == current.id;
                History { id: h.id, title: h.title, updated_at: h.updated_at, current: is_current, chosen: is_current, loaded: is_current, range: if is_current { 0..entries.len() } else { 0..0 }, questions: h.questions }
            }).collect::<Vec<_>>(), Err(error) => { self.fail(error.to_string(), cx); return }
        };
        if !entries.is_empty() && !histories.iter().any(|h| h.current) { histories.insert(0, History { id: current.id.clone(), title: current.transcript.title(), updated_at: current.updated_at.clone(), current: true, chosen: true, loaded: true, range: 0..entries.len(), questions: entries.len() }); }
        if histories.is_empty() { self.fail(t("没有可保存的完整问答。"), cx); return }
        let job = wisp_net::notion::Save { settings: self.settings.notion.clone(), settings_path: self.settings_path.clone(), library: lib.info.clone(), paper: selected, title: None, entries, pictures: vec![] };
        self.open_notion_form(job, histories, if only.is_some() { Stage::Refining } else { Stage::Histories }, window, cx);
    }

    pub fn preview_notion_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notion.busy { self.fail(t("Notion 操作尚未完成，请稍后再试。"), cx); return }
        if let Err(error) = self.settings.notion.validate() { self.fail(error, cx); return }
        let Some(lib) = &self.lib else { return };
        let Some(paper) = lib.selected_paper().cloned() else { return };
        let entry = match notion::Entry::personal(&paper.id, lib.notes.read(cx).text()) { Ok(entry) => entry, Err(error) => { self.fail(error, cx); return } };
        let job = wisp_net::notion::Save { settings: self.settings.notion.clone(), settings_path: self.settings_path.clone(), library: lib.info.clone(), paper: Some(paper), title: None, entries: vec![entry], pictures: vec![] };
        self.open_notion_form(job, vec![], Stage::Editing, window, cx);
    }

    pub fn render_notion_form(&self, form: &Form, window: &Window, cx: &mut Context<Self>) -> Div {
        let width = (f32::from(window.viewport_size().width) * 0.92).min(760.);
        let busy = self.notion.busy;
        let job = &form.job;
        let title = if job.requires_title() { notion::manual_title(form.title.read(cx).text()).unwrap_or_else(|_| t("保存前手动输入的标题").into()) } else { job.paper.as_ref().map(|p| job.settings.paper_title(p)).unwrap_or_else(|| t("文献库问答").into()) };
        let destination = [job.settings.root_title.trim(), job.library.name.as_str(), title.as_str()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" → ");
        let count = form.count();
        let editing = form.stage == Stage::Editing;
        let titling = form.stage == Stage::Titling;
        let manual = job.requires_title();
        let mut steps = Vec::new();
        if !form.direct {
            steps.extend([(Stage::Histories, "选择对话"), (Stage::Questions, "选择问题"), (Stage::Refining, "提炼问题")]);
        } else if form.stage == Stage::Refining {
            steps.push((Stage::Refining, "提炼问题"));
        }
        steps.push((Stage::Editing, if manual { "编辑笔记" } else { "编辑并保存" }));
        if manual { steps.push((Stage::Titling, "填写标题")); }
        let can_advance = (if form.stage == Stage::Histories { form.histories.iter().any(|h| h.chosen) } else { count > 0 }) && !(form.stage == Stage::Refining && form.refine_busy) && (!titling || notion::manual_title(form.title.read(cx).text()).is_ok());
        let active = form.active();
        let body = div().id("notion-form-body").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().gap(px(14.)).px(px(20.)).py(px(12.))
            .when(steps.len() > 1, |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(steps.into_iter().enumerate().map(|(i, (stage, label))| ui::chip(format!("{} {}", i + 1, t(label)), if form.stage == stage { Theme::accent() } else { Theme::text_secondary() }, if form.stage == stage { Theme::accent_soft() } else { Theme::bg_sunken() })))))
            .child(div().line_height(text_px(21.)).child(destination))
            .when(!titling && !job.settings.parent_url.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(job.settings.parent_url.clone())))
            .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(if form.stage == Stage::Histories { tf!("已选择 {} 段对话", "{} conversation(s) selected", form.histories.iter().filter(|h| h.chosen).count()) } else { tf!("已选择 {} 条笔记 · {} 张原图", "{} note(s) selected · {} original image(s)", count, form.image_count(cx)) }))
            .when(busy, |d| d.child(div().py(px(20.)).child(self.notion.status.clone())))
            .when(!busy && titling, |d| d
                .when_some(job.paper.as_ref(), |d, paper| d.child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(paper.title.clone())))

                .child(ui::field(&form.title, window, cx).w(px(width - 40.)).flex_none())
                .child(hint("已有笔记页将更新标题，正文和链接保留。")))
            .when(!busy && form.stage == Stage::Histories, |d| d
                .child(hint("先选择对话历史，下一步选择问答并调整顺序。"))
                .child(div().flex().gap(px(8.))
                    .child(ui::secondary_button("notion-histories-all", t("全选")).on_click(self.on(Cmd::NotionHistory(None, true), cx)))
                    .child(ui::secondary_button("notion-histories-none", t("取消全选")).on_click(self.on(Cmd::NotionHistory(None, false), cx))))
                .children(form.histories.iter().enumerate().map(|(i, history)| div().id(("notion-history", i)).flex().flex_none().items_start().gap(px(10.)).p(px(12.)).rounded(px(8.)).border_1().border_color(if history.chosen { Theme::accent() } else { Theme::line() }).bg(if history.chosen { Theme::accent_soft() } else { Theme::bg_body() }).cursor_pointer().hover(|d| d.bg(Theme::hover()))
                    .child(ui::icon(if history.chosen { "check" } else { "square" }, 18., if history.chosen { Theme::accent() } else { Theme::text_tertiary() }))
                    .child(div().flex().flex_col().flex_1().min_w_0().gap(px(6.))
                        .child(div().flex().flex_wrap().items_center().gap(px(8.)).child(history.title.clone()).when(history.current, |d| d.child(ui::chip(t("当前对话"), Theme::accent(), Theme::bg_body()))))
                        .child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(tf!("{} 个问题 · 最近更新 {}", "{} question(s) · Updated {}", history.questions, wisp_core::time::short_time(&history.updated_at)))))
                    .on_click(self.on(Cmd::NotionHistory(Some(i), false), cx)))))
            .when(!busy && form.stage == Stage::Questions, |d| d
                .child(div().child(t("按提问时间排列，可上下移动整条问答以调整写入顺序。")))
                .child(hint("默认勾选未保存的问题；已保存的问题可勾选后编辑为新版本。待核实的问题请先检查远端结果。"))
                .child(div().flex().gap(px(8.))
                    .child(ui::secondary_button("notion-select-all", t("全选")).on_click(self.on(Cmd::NotionSelect(None, true), cx)))
                    .child(ui::secondary_button("notion-select-none", t("取消全选")).on_click(self.on(Cmd::NotionSelect(None, false), cx))))
                .children(active.iter().enumerate().map(|(position, &i)| {
                    let entry = &job.entries[i];
                    let question = entry.label.clone();
                    let question = if question.chars().count() > 160 { format!("{}…", question.chars().take(159).collect::<String>()) } else { question };
                    let selected = form.selected[i];

                    let pending = form.states[i] == notion::EntryState::Pending;
                    div().flex().flex_none().flex_col().gap(px(8.))
                        .child(div().id(("notion-question", i)).flex().flex_none().items_start().gap(px(10.)).p(px(12.)).rounded(px(8.)).border_1().border_color(if selected { Theme::accent() } else { Theme::line() }).bg(if selected { Theme::accent_soft() } else { Theme::bg_body() }).when(!pending, |d| d.cursor_pointer().hover(|d| d.bg(Theme::hover())))
                        .child(ui::icon(if selected { "check" } else { "square" }, 18., if selected { Theme::accent() } else { Theme::text_tertiary() }))
                        .child(div().flex().flex_col().flex_1().min_w_0().gap(px(5.)).child(div().line_height(text_px(21.)).child(format!("{}. {}", position + 1, question))).when(!entry.images.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_secondary()).child(tf!("{} 张原图", "{} original image(s)", entry.images.len())))))
                        .child(ui::chip(t(match form.states[i] { notion::EntryState::New => "未保存", notion::EntryState::Saved => "已保存", notion::EntryState::Pending => "待核实" }), Theme::text_secondary(), Theme::bg_sunken()))
                        .when(!pending, |d| d.on_click(self.on(Cmd::NotionSelect(Some(i), false), cx))))
                        .child(div().flex().justify_end().gap(px(6.))
                            .child(ui::secondary_button(("notion-up",i), t("上移")).when(position > 0, |d| d.on_click(self.on(Cmd::NotionMove(i,false), cx))).when(position == 0, |d| d.opacity(0.4)))
                            .child(ui::secondary_button(("notion-down",i), t("下移")).when(position + 1 < active.len(), |d| d.on_click(self.on(Cmd::NotionMove(i,true), cx))).when(position + 1 == active.len(), |d| d.opacity(0.4))))
                })))
            .when(!busy && editing, |d| d
                .child(crate::markdown::render("notion-header", &job.settings.header(&job.library, job.paper.as_ref()), None, None))
                .child(div().text_size(text_px(12.)).line_height(text_px(20.)).text_color(Theme::text_secondary()).child(t("下方内容可直接编辑。原图占位行会在保存时替换成实际图片；删除整行可移除图片。本地原笔记与问答保留。")))
                .children(active.iter().filter(|i| form.selected[**i]).map(|&i| div().flex().flex_none().flex_col().gap(px(6.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(tf!("笔记 {}", "Note {}", form.ordinal(i))))
                    .child(ui::field(&form.editors[i], window, cx).w(px(width - 40.)).flex_none()))))
            .when(!busy && form.stage == Stage::Refining, |d| d
                .child(self.render_refine_toolbar(form, cx))
                .children(active.iter().filter(|i| form.selected[**i]).map(|&i| self.render_question_refinement(form, i, width - 40., window, cx))))
            .child(ui::error_line(&self.modal_error));
        ui::dialog(width).max_h(relative(0.88))
            .child(ui::dialog_title(match form.stage { Stage::Histories => t("选择要保存的历史对话"), Stage::Questions => t("选择要保存的问答"), Stage::Refining => t("提炼问题并对比（可跳过）"), Stage::Editing if manual => t("编辑笔记"), Stage::Editing => t("编辑笔记并保存到 Notion"), Stage::Titling => t("填写本篇文献的笔记标题") }))
            .child(body)
            .child(div().flex_none().px(px(20.)).child(self.notion_account_card()))
            .child(div().flex().flex_none().justify_end().gap(px(8.)).px(px(20.)).py(px(14.))
                .when(form.stage != Stage::Histories && !titling && !form.direct && !busy, |d| d.child(ui::secondary_button("notion-back", t("上一步")).on_click(self.on(Cmd::NotionBack, cx))))
                .child(ui::secondary_button("notion-cancel", if busy { t("关闭") } else if titling { t("上一步") } else { t("取消") }).on_click(self.on(if titling && !busy { Cmd::NotionBack } else { Cmd::CloseModal }, cx)))
                .when(form.stage == Stage::Refining && !busy, |d| d.child(ui::secondary_button("notion-refine-skip", t("跳过，直接编辑")).on_click(self.on(Cmd::NotionRefineSkip, cx))))
                .child(ui::primary_button("notion-submit", if busy && self.notion.status == t("正在检查 Notion 账户…") { t("正在检查 Notion 账户…") } else if busy { t("正在保存…") } else { match form.stage { Stage::Histories => t("下一步：选择问题"), Stage::Questions => t("下一步：提炼问题"), Stage::Refining => t("下一步：编辑笔记"), Stage::Editing if manual => t("下一步：填写标题"), Stage::Editing | Stage::Titling => t("保存到 Notion") } })
                    .when(!busy && can_advance, |d| d.on_click(self.on(Cmd::SubmitModal, cx)))
                    .when(busy || !can_advance, |d| d.opacity(0.5))))
    }

    fn render_refine_toolbar(&self, form: &Form, cx: &mut Context<Self>) -> Div {
        let profile = self.settings.profile_for_or_default(wisp_core::settings::Task::QuestionRefine);
        let candidates = form.active().iter().any(|i| form.selected[*i] && !form.refinements[*i].pending && wisp_core::question::clean(form.refinements[*i].input.read(cx).text()).is_ok());
        div().flex().flex_col().gap(px(10.))
            .child(hint("默认保留原问。提炼只改笔记中的问题标题，不核验事实。"))
            .child(hint("发送给模型：问题、回答及文献信息节选，不含原图。"))
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(t("提炼模型")))
                .child(ui::ghost_button("notion-refine-model", Some("chevron-down"), Some(crate::write_assist::model_name(&profile).into())).text_color(Theme::text()).border_1().border_color(Theme::line_strong()).when(!form.refine_busy, |d| d.on_click(self.on_menu(crate::app::MenuKind::TaskModel(wisp_core::settings::Task::QuestionRefine), cx)))))
            .when_some(profile.missing(), |d, message| d.child(ui::error_line(t(message))))
            .child(div().flex().flex_wrap().gap(px(8.))
                .child(ui::primary_button("notion-refine-all", if form.refine_busy { t("正在提炼…") } else if form.count() > 1 { t("全部提炼") } else { t("提炼") }).when(!form.refine_busy && profile.missing().is_none(), |d| d.on_click(self.on(Cmd::NotionRefine(None), cx))).when(form.refine_busy || profile.missing().is_some(), |d| d.opacity(0.5)))
                .when(form.refine_busy, |d| d.child(ui::secondary_button("notion-refine-stop", t("停止提炼")).on_click(self.on(Cmd::NotionRefineStop, cx))))
                .child(ui::secondary_button("notion-refine-originals", t("全部保留原问")).on_click(self.on(Cmd::NotionRefineChoice(None, false), cx)))
                .child(ui::secondary_button("notion-refine-adopt", t("采用全部提炼稿")).when(candidates, |d| d.on_click(self.on(Cmd::NotionRefineChoice(None, true), cx))).when(!candidates, |d| d.opacity(0.5))))
    }

    fn render_question_refinement(&self, form: &Form, i: usize, width: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let entry = &form.job.entries[i];
        let refinement = &form.refinements[i];
        let wide = width >= 580.;
        let cell = if wide { (width - 12.) / 2. } else { width };
        let original = if entry.original_question.trim().is_empty() { t("原提问只有图片，没有文字问题。").to_string() } else { entry.original_question.clone() };
        let candidate = wisp_core::question::clean(refinement.input.read(cx).text()).is_ok() && !refinement.pending;
        let card = |selected: bool| div().flex().flex_col().flex_none().w(px(cell)).min_w_0().gap(px(10.)).p(px(12.)).rounded(px(9.)).border_1().border_color(if selected { Theme::accent() } else { Theme::line() }).bg(if selected { Theme::accent_soft() } else { Theme::bg_sunken() });
        let original = card(!refinement.use_refined).id(("notion-original-card", i)).cursor_pointer().on_click(self.on(Cmd::NotionRefineChoice(Some(i), false), cx))
            .child(div().flex().flex_wrap().items_center().gap(px(6.)).child(div().flex_1().font_weight(FontWeight::MEDIUM).child(t("原提问")))
                .child(choice(SharedString::from(format!("notion-refine-original-{i}")), "保留原问", !refinement.use_refined)))
            .child(div().id(("notion-refine-original-body", i)).max_h(px(200.)).overflow_y_scroll().text_size(text_px(13.)).line_height(text_px(21.)).child(original));
        let proposed = card(refinement.use_refined).id(("notion-proposal-card", i)).when(candidate, |d| d.cursor_pointer().on_click(self.on(Cmd::NotionRefineChoice(Some(i), true), cx)))
            .child(div().flex().flex_wrap().items_center().gap(px(6.)).child(div().flex_1().font_weight(FontWeight::MEDIUM).child(t("提炼稿 · 可编辑")))
                .child(choice(SharedString::from(format!("notion-refine-choice-{i}")), "采用此稿", refinement.use_refined).when(!candidate, |d| d.opacity(0.5))))
            .when(refinement.pending, |d| d.child(div().py(px(18.)).text_color(Theme::text_secondary()).child(t("正在提炼…"))))
            .when(!refinement.pending, |d| d.child(div().id(("notion-proposal-editor", i)).on_click(|_, _, cx| cx.stop_propagation()).child(ui::field(&refinement.input, window, cx).w(px(cell - 26.)))))
            .when(!refinement.pending && !refinement.model.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(refinement.model.clone())))
            .when(!refinement.error.is_empty(), |d| d.child(ui::error_line(&refinement.error)));
        div().flex().flex_none().flex_col().gap(px(10.)).pb(px(16.)).border_b_1().border_color(Theme::line())
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(tf!("问题 {}", "Question {}", form.ordinal(i))))
                .child(ui::secondary_button(SharedString::from(format!("notion-refine-one-{i}")), if refinement.input.read(cx).text().trim().is_empty() { t("提炼") } else { t("重新提炼") }).when(!form.refine_busy, |d| d.on_click(self.on(Cmd::NotionRefine(Some(i)), cx))).when(form.refine_busy, |d| d.opacity(0.5))))
            .child(div().flex().gap(px(12.)).when(!wide, |d| d.flex_col()).child(original).child(proposed))
    }

    fn open_notion_form(&mut self, mut job: wisp_net::notion::Save, histories: Vec<History>, stage: Stage, window: &mut Window, cx: &mut Context<Self>) {
        let states = match local_states(&job, self.notion.account.as_ref(), &job.entries) { Ok(states) => states, Err(error) => { self.fail(error, cx); return } };
        let entries = std::mem::take(&mut job.entries);
        let title = cx.new(|cx| TextInput::new(t("请输入本篇文献的笔记标题"), cx));
        let subscription = cx.subscribe_in(&title, window, |this, _, event: &InputEvent, window, cx| {
            if !matches!(this.modal.as_ref(), Some(Modal::Notion(form)) if form.stage == Stage::Titling) { return }
            match event {
                InputEvent::Changed => { this.modal_error.clear(); cx.notify(); }
                InputEvent::Submit => this.run(Cmd::SubmitModal, window, cx),
                InputEvent::Cancel => this.run(Cmd::CloseModal, window, cx),
                _ => {}
            }
        });
        let mut form = Form { job, order: vec![], selected: vec![], editors: vec![], stage, direct: stage != Stage::Histories, histories, title, states: vec![], refinements: vec![], refine_busy: false, refine_session: Arc::new(()), refine_cancel: Arc::new(AtomicBool::new(false)), refine_generation: 0, _subscriptions: vec![subscription] };
        form.append_entries(entries, states, cx);
        if stage != Stage::Histories { for (selected, state) in form.selected.iter_mut().zip(&form.states) { *selected = *state != notion::EntryState::Pending; } }
        self.modal_error.clear();
        self.modal = Some(Modal::Notion(Box::new(form)));
        self.notion_status(cx);
        cx.notify();
    }

    pub fn notion_history(&mut self, index: Option<usize>, chosen: bool, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        let Some(Modal::Notion(form)) = &mut self.modal else { return };
        match index { Some(i) => { if let Some(history) = form.histories.get_mut(i) { history.chosen = !history.chosen; } }, None => { for history in &mut form.histories { history.chosen = chosen; } } }
        self.modal_error.clear();
        cx.notify();
    }

    pub fn notion_select(&mut self, index: Option<usize>, selected: bool, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        let Some(Modal::Notion(form)) = &mut self.modal else { return };
        match index {
            Some(i) => { if form.states.get(i) != Some(&notion::EntryState::Pending) && let Some(selected) = form.selected.get_mut(i) { *selected = !*selected; } },
            None => { for i in form.active() { form.selected[i] = selected && form.states[i] != notion::EntryState::Pending; } }
        }
        self.modal_error.clear();
        cx.notify();
    }

    pub fn notion_back(&mut self, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        if let Some(Modal::Notion(form)) = &mut self.modal {
            form.stop_refining();
            form.stage = match form.stage { Stage::Titling => Stage::Editing, Stage::Editing => Stage::Refining, Stage::Refining => Stage::Questions, _ => Stage::Histories };
        }
        self.modal_error.clear();
        cx.notify();
    }

    pub fn notion_refine_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(Modal::Notion(form)) = &mut self.modal { form.stop_refining(); }
        cx.notify();
    }

    pub fn notion_refine_skip(&mut self, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        if let Some(Modal::Notion(form)) = &mut self.modal { form.stop_refining(); form.stage = Stage::Editing; }
        self.modal_error.clear();
        cx.notify();
    }

    pub fn notion_refine_choice(&mut self, index: Option<usize>, refined: bool, cx: &mut Context<Self>) {
        let Some(Modal::Notion(form)) = &mut self.modal else { return };
        let indices = index.map_or_else(|| form.active(), |i| vec![i]);
        for i in indices {
            if !form.selected[i] || (refined && form.refinements[i].pending) { continue }
            if refined && wisp_core::question::clean(form.refinements[i].input.read(cx).text()).is_err() { continue }
            form.refinements[i].use_refined = refined;
        }
        self.modal_error.clear();
        cx.notify();
    }

    pub fn notion_refine(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        let profile = self.settings.profile_for_or_default(wisp_core::settings::Task::QuestionRefine);
        let Some(Modal::Notion(form)) = &mut self.modal else { return };
        if form.stage != Stage::Refining || form.refine_busy { return }
        let active = form.active();
        let indices = active.into_iter().filter(|i| form.selected[*i] && form.job.entries[*i].group.is_some() && index.is_none_or(|only| only == *i)).collect::<Vec<_>>();
        let requests = indices.iter().map(|i| wisp_core::question::refine_request(&profile, &form.job.entries[*i], form.job.paper.as_ref(), &form.job.library).map(|request| (*i, request))).collect::<Result<Vec<_>, _>>();
        let requests = match requests { Ok(requests) if !requests.is_empty() => requests, Ok(_) => return, Err(error) => { self.modal_error = error; cx.notify(); return } };
        form.stop_refining();
        form.refine_cancel = Arc::new(AtomicBool::new(false));
        form.refine_busy = true;
        let (session, cancelled, generation, protocol, model) = (form.refine_session.clone(), form.refine_cancel.clone(), form.refine_generation, profile.protocol, crate::write_assist::model_name(&profile));
        for i in indices {
            let refinement = &mut form.refinements[i];
            refinement.pending = true;
            refinement.error.clear();
        }
        self.modal_error.clear();
        cx.notify();
        cx.spawn(async move |this, cx| {
            for (i, request) in requests {
                if cancelled.load(Ordering::Relaxed) { break }
                let result = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol).and_then(|text| wisp_core::question::clean(&text)) }).await;
                if cancelled.load(Ordering::Relaxed) { break }
                let alive = this.update(cx, |this, cx| {
                    let Some(Modal::Notion(form)) = &mut this.modal else { return false };
                    if !Arc::ptr_eq(&session, &form.refine_session) || generation != form.refine_generation || form.stage != Stage::Refining { return false }
                    let refinement = &mut form.refinements[i];
                    refinement.pending = false;
                    match result {
                        Ok(text) => { refinement.input.update(cx, |input, cx| input.set_text(&text, cx)); refinement.use_refined = false; refinement.model = model.clone(); refinement.error.clear(); }
                        Err(error) => refinement.error = error,
                    }
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !alive { cancelled.store(true, Ordering::Relaxed); break }
            }
            this.update(cx, |this, cx| {
                let Some(Modal::Notion(form)) = &mut this.modal else { return };
                if !Arc::ptr_eq(&session, &form.refine_session) || generation != form.refine_generation { return }
                form.refine_busy = false;
                for refinement in &mut form.refinements { refinement.pending = false; }
                cx.notify();
            }).ok();
        }).detach();
    }

    pub fn submit_notion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notion.busy { return }
        let Some(Modal::Notion(form)) = &mut self.modal else { return };
        if form.stage == Stage::Histories {
            if !form.histories.iter().any(|h| h.chosen) { self.modal_error = t("请至少选择一段对话。").into(); cx.notify(); return }
            let result = (|| -> Result<(), String> {
                let store = wisp_core::store::Store::open(&form.job.library.dir).map_err(|e| e.to_string())?;
                let scope = form.job.paper.as_ref().map_or("", |p| p.id.as_str()).to_string();
                for i in 0..form.histories.len() {
                    if !form.histories[i].chosen || form.histories[i].loaded { continue }
                    let conversation = store.conversation(&form.histories[i].id).map_err(|e| e.to_string())?.ok_or("对话历史已不存在，请关闭窗口后重新选择。")?;
                    if conversation.paper_id != scope { return Err("对话历史不属于当前文献，已停止读取。".into()) }
                    let entries = notion::conversation_entries(&conversation, None);
                    let states = local_states(&form.job, self.notion.account.as_ref(), &entries)?;
                    let start = form.job.entries.len();
                    let end = start + entries.len();
                    form.append_entries(entries, states, cx);
                    form.histories[i].range = start..end;
                    form.histories[i].loaded = true;
                }
                Ok(())
            })();
            if let Err(error) = result { self.modal_error = error; cx.notify(); return }
            form.stage = Stage::Questions;
            cx.notify();
            return;
        }
        if form.count() == 0 { self.modal_error = t("请至少选择一条笔记。").into(); cx.notify(); return }
        if form.stage == Stage::Questions { form.stage = Stage::Refining; cx.notify(); return }
        if form.stage == Stage::Refining {
            if form.refine_busy { return }
            if let Err(error) = form.apply_refinements(cx) { self.modal_error = error; cx.notify(); return }
            form.stage = Stage::Editing;
            cx.notify();
            return;
        }
        let mut job = form.job.clone();
        let entries = form.active().into_iter().filter(|i| form.selected[*i]).map(|i| form.job.entries[i].edited(form.editors[i].read(cx).text())).collect::<Result<Vec<_>, _>>();
        match entries { Ok(entries) => job.entries = entries, Err(error) => { self.modal_error = error; cx.notify(); return } }
        if job.requires_title() {
            if form.stage == Stage::Editing {
                form.stage = Stage::Titling;
                form.title.read(cx).focus(window);
                self.modal_error.clear();
                cx.notify();
                return;
            }
            match notion::manual_title(form.title.read(cx).text()) { Ok(title) => job.title = Some(title), Err(error) => { self.modal_error = error; cx.notify(); return } }
        }
        // Read attachments only for selected notes, after edits may have removed them.
        let pictures = (|| -> Result<Vec<notion::Picture>, String> {
            let store = wisp_core::store::Store::open(&job.library.dir).map_err(|e| e.to_string())?;
            let mut pictures = vec![];
            let ids = job.entries.iter().flat_map(|entry| entry.images.iter().map(|id| (entry.group.as_ref().map(|g| g.id.clone()).unwrap_or_default(), id.clone()))).collect::<BTreeSet<_>>();
            for (conversation, id) in ids {
                let (media_type, bytes) = store.conversation_image(&conversation, &id).map_err(|e| e.to_string())?.ok_or_else(|| tf!("图片附件 {id} 无法读取，尚未保存。", "Cannot read image attachment {id}; nothing was saved."))?;
                pictures.push(notion::Picture { conversation, id, media_type, bytes });
            }
            Ok(pictures)
        })();
        match pictures { Ok(pictures) => job.pictures = pictures, Err(error) => { self.modal_error = error; cx.notify(); return } }
        self.notion.busy = true;
        self.notion.status = t("正在保存到 Notion…").into();
        self.say(t("正在保存到 Notion…"), cx);
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { wisp_net::notion::save(job) }).await;
            this.update(cx, |this, cx| {
                this.notion.busy = false;
                match result {
                    Ok(saved) => {
                        if matches!(this.modal, Some(Modal::Notion(_))) { this.modal = None; }
                        this.notion.status = tf!("已保存到 Notion，新增 {} 条笔记。", "Saved to Notion; {} new note(s).", saved.added);
                        this.say(this.notion.status.clone(), cx);
                    }
                    Err(error) => { this.notion.status = error.clone(); if matches!(this.modal, Some(Modal::Notion(_))) { this.modal_error = error; } else { this.fail(error, cx); } }
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }
}

pub(crate) fn notion_logo(size: f32) -> Div {
    let inset = (size * 0.14).round();
    div().flex_none().size(px(size)).rounded(px(size * 0.2)).bg(gpui::white()).p(px(inset))
        .child(svg().path("plugins/notion.svg").size(px(size - inset * 2.)).text_color(gpui::rgb(0x000000)))
}

fn local_states(job: &wisp_net::notion::Save, account: Option<&wisp_net::notion::Account>, entries: &[notion::Entry]) -> Result<Vec<notion::EntryState>, String> {
    let Some(account) = account else { return Ok(vec![notion::EntryState::New; entries.len()]) };
    let account = notion::fingerprint(&format!("{}/{}", account.workspace_id, account.user_id));
    let index = notion::Index::open(&job.settings_path.with_file_name("notion.sqlite"))?;
    index.states(&account, &job.settings, &job.library, job.paper.as_ref(), entries)
}

fn hint(text: &'static str) -> Div {
    div().text_size(text_px(11.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(t(text))
}

fn settings_card(icon: &'static str, title: &'static str, description: &'static str, width: f32) -> Div {
    div().flex().flex_col().flex_none().w(px(width)).gap(px(14.)).p(px(18.)).rounded(px(12.)).border_1().border_color(Theme::line()).bg(Theme::bg_body())
        .child(div().flex().items_start().gap(px(10.))
            .child(div().flex().flex_none().items_center().justify_center().size(px(30.)).rounded(px(8.)).bg(Theme::accent_soft()).child(ui::icon(icon, 16., Theme::accent())))
            .child(div().flex().flex_col().flex_1().min_w_0().gap(px(4.))
                .child(div().text_size(text_px(14.)).font_weight(FontWeight::SEMIBOLD).child(t(title)))
                .child(hint(description))))
}

fn choice(id: impl Into<ElementId>, label: &str, selected: bool) -> Stateful<Div> {
    div().id(id).flex().flex_none().items_center().gap(px(5.)).h(text_px(28.)).px(px(10.)).rounded(px(7.)).border_1().cursor_pointer().text_size(text_px(12.))
        .when(selected, |d| d.bg(Theme::accent_soft()).border_color(Theme::accent()).text_color(Theme::accent()).font_weight(FontWeight::MEDIUM).child(ui::icon("check", 12., Theme::accent())).hover(|s| s.opacity(0.8)))
        .when(!selected, |d| d.bg(Theme::bg_body()).border_color(Theme::line_strong()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
        .child(t(label).to_string())
}

fn reorder_arrow(id: impl Into<ElementId>, icon: &str, enabled: bool) -> Stateful<Div> {
    let color = if !enabled { Theme::text_tertiary() } else if crate::theme::dark() { gpui::white() } else { Theme::text() };
    div().id(id).flex().flex_none().items_center().justify_center().size(text_px(24.)).rounded(px(4.))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(Theme::hover())).active(|s| s.bg(Theme::pressed())))
        .child(ui::icon(icon, 16., color))
}
