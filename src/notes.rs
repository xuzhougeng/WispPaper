//! A destination picker for notes and selected questions; tools share the same document.

use crate::{app::{Cmd, Modal, Talk, Workspace}, i18n::t, input::TextInput, theme::{Theme, text_px}, ui};
use gpui::{prelude::FluentBuilder, *};
use serde_json::Value;
use wisp_core::{notes::{self, Document, Questions, Saved, Selection}, store::Store};

pub struct Choice {
    conversation: String,
    number: usize,
    label: String,
    chosen: bool,
}

pub struct Form {
    token: String,
    subject: String,
    choices: Vec<Choice>,
    from_chat: bool,
    include_notes: bool,
    title: Entity<TextInput>,
    parent: Entity<TextInput>,
    recovery_page: Entity<TextInput>,
    document: Option<Document>,
    pending: Option<Saved>,
    pub busy: bool,
    preview: bool,
    notion: bool,
    error: String,
    result: String,
    url: String,
}

impl Form {
    fn selection(&self) -> Selection {
        let mut conversations: Vec<Questions> = vec![];
        for choice in self.choices.iter().filter(|c| c.chosen) {
            if let Some(q) = conversations.iter_mut().find(|q| q.id == choice.conversation) { q.questions.push(choice.number) }
            else { conversations.push(Questions { id: choice.conversation.clone(), questions: vec![choice.number] }) }
        }
        Selection { paper_id: self.subject.clone(), include_notes: Some(self.include_notes), conversations }
    }
}

impl Workspace {
    pub fn open_note(&mut self, talk: Option<Talk>, notion: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(talk) = talk {
            if self.lib.as_ref().is_some_and(|l| l.chat(talk).busy) { return self.fail(t("请等当前回答结束后再保存问答。"), cx) }
            self.save_chat(talk, cx);
        }
        let Some(lib) = self.lib.as_ref() else { return };
        let subject = if talk == Some(Talk::Library) { String::new() } else {
            let Some(id) = lib.selected.clone() else { return };
            id
        };
        let current = talk.map(|t| lib.chat(t).id.clone()).unwrap_or_default();
        let mut choices = vec![];
        if talk.is_some() {
            let found = match notes::sources(&lib.store, &subject) { Ok(found) => found, Err(e) => return self.fail(e.to_string(), cx) };
            for c in found["conversations"].as_array().into_iter().flatten() {
                let id = c["id"].as_str().unwrap_or("");
                for q in c["questions"].as_array().into_iter().flatten() {
                    let label = q["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
                    choices.push(Choice { conversation: id.to_string(), number: q["number"].as_u64().unwrap_or(1) as usize,
                        label: if label.is_empty() { t("图片提问").into() } else { label }, chosen: id == current });
                }
            }
        }
        let saves = lib.store.note_saves(&subject).unwrap_or_default();
        let parent_value = saves.first().map(|s| format!("https://www.notion.so/{}", s.parent)).unwrap_or_default();
        let pending = saves.into_iter().find(|s| s.status == "pending");
        let recovery = pending.as_ref().filter(|s| !s.page.is_empty()).map(Saved::url).unwrap_or_default();
        self.modal_subscriptions.clear();
        let title = self.modal_input("笔记标题", "", false, window, cx);
        let parent = self.modal_input("Notion 父页面链接或 ID", &parent_value, false, window, cx);
        let recovery_page = self.modal_input("实际保存成功的 Notion 页面链接", &recovery, false, window, cx);
        self.modal_error.clear();
        self.modal = Some(Modal::ExportNote(Box::new(Form {
            token: wisp_core::model::new_id("draft"), subject, choices, from_chat: talk.is_some(), include_notes: talk.is_none(), title, parent, recovery_page,
            document: None, pending, busy: false, preview: false, notion, error: String::new(), result: String::new(), url: String::new(),
        })));
        self.refresh_note(true, cx);
    }

    pub fn pick_note(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(Modal::ExportNote(form)) = self.modal.as_mut() else { return };
        if form.busy || !form.from_chat { return }
        if at == usize::MAX { form.include_notes = !form.include_notes }
        else if let Some(choice) = form.choices.get_mut(at) { choice.chosen = !choice.chosen }
        self.refresh_note(false, cx);
    }

    fn refresh_note(&mut self, set_title: bool, cx: &mut Context<Self>) {
        let (Some(lib), Some(Modal::ExportNote(form))) = (self.lib.as_ref(), self.modal.as_mut()) else { return };
        if form.busy { return }
        form.document = None;
        form.result.clear();
        form.url.clear();
        match notes::prepare(&lib.store, &lib.info, &form.selection()) {
            Ok(doc) => {
                // Selecting questions must not replace a title the person has edited.
                if set_title || form.title.read(cx).text().is_empty() { form.title.update(cx, |input, cx| input.set_text(&doc.title, cx)) }
                form.document = Some(doc);
                form.error.clear();
            }
            Err(error) => form.error = error.to_string(),
        }
        cx.notify();
    }

    pub fn note_destination(&mut self, notion: bool, cx: &mut Context<Self>) {
        let Some(Modal::ExportNote(form)) = self.modal.as_mut() else { return };
        if form.busy { return }
        form.notion = notion;
        form.result.clear();
        form.url.clear();
        if form.document.is_some() { form.error.clear() }
        cx.notify();
    }

    pub fn preview_note(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::ExportNote(form)) = self.modal.as_mut() else { return };
        form.preview = !form.preview;
        cx.notify();
    }

    pub fn save_note(&mut self, notion: bool, cx: &mut Context<Self>) {
        let (Some(lib), Some(Modal::ExportNote(form))) = (self.lib.as_ref(), self.modal.as_mut()) else { return };
        if form.busy || (notion && form.pending.is_some()) { return }
        let Some(mut doc) = form.document.clone() else { return };
        doc.title = form.title.read(cx).text().trim().to_string();
        if let Err(error) = doc.validate() { form.error = error.to_string(); return cx.notify() }
        let parent = form.parent.read(cx).text().to_string();
        if notion && let Err(error) = notes::page_id(&parent) { form.error = error.to_string(); return cx.notify() }
        let info = lib.info.clone();
        let token = form.token.clone();
        form.busy = true;
        form.error.clear();
        form.result.clear();
        cx.notify();
        if notion {
            let library = info.id.clone();
            let work = cx.background_executor().spawn(async move {
                let mut store = Store::open(&info.dir).map_err(|e| e.to_string())?;
                wisp_net::notes::save(&mut store, &doc, &parent)
            });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                this.update(cx, |this, cx| this.note_done(&library, &token, result, cx)).ok();
            }).detach();
        } else {
            let name = format!("{}.md", doc.title.chars().take(80).collect::<String>().replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-"));
            let directory = dirs::document_dir().unwrap_or_else(|| info.dir.clone());
            let picked = cx.prompt_for_new_path(&directory, Some(&name));
            cx.spawn(async move |this, cx| {
                let result = match picked.await {
                    Ok(Ok(Some(mut path))) => {
                        if path.extension().is_none() { path.set_extension("md"); }
                        cx.background_executor().spawn(async move { notes::export(&doc, &path).map_err(|e| format!("{e:#}")) }).await
                    }
                    Ok(Ok(None)) => Ok(serde_json::json!({"cancelled": true})),
                    _ => Err(t("无法打开保存文件窗口。").to_string()),
                };
                this.update(cx, |this, cx| this.note_done(&info.id, &token, result, cx)).ok();
            }).detach();
        }
    }

    fn note_done(&mut self, library: &str, token: &str, result: Result<Value, String>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut().filter(|lib| lib.info.id == library) else { return };
        if let Ok(links) = lib.store.note_links() { lib.note_links = links }
        let Some(Modal::ExportNote(form)) = self.modal.as_mut().filter(|modal| matches!(modal, Modal::ExportNote(form) if form.token == token)) else { return cx.notify() };
        form.busy = false;
        form.pending = lib.store.note_saves(&form.subject).unwrap_or_default().into_iter().find(|s| s.status == "pending");
        if let Some(saved) = &form.pending && !saved.page.is_empty() { form.recovery_page.update(cx, |input, cx| input.set_text(&saved.url(), cx)) }
        match result {
            Ok(value) if value["cancelled"] == true => {}
            Ok(value) if value["url"].is_string() => {
                form.url = value["url"].as_str().unwrap_or("").into();
                form.result = if value["duplicate"] == true { t("相同笔记已保存，跳过重复写入。") } else { t("已保存到 Notion。") }.into();
            }
            Ok(value) => form.result = tf!("已导出到 {}", "Exported to {}", value["path"].as_str().unwrap_or("")),
            Err(error) => form.error = error,
        }
        cx.notify();
    }

    pub fn resolve_note(&mut self, saved: bool, cx: &mut Context<Self>) {
        let (Some(lib), Some(Modal::ExportNote(form))) = (self.lib.as_mut(), self.modal.as_mut()) else { return };
        if form.busy { return }
        let Some(pending) = &form.pending else { return };
        let page = form.recovery_page.read(cx).text().to_string();
        match lib.store.resolve_note(&pending.id, saved.then_some(page.as_str())) {
            Ok(()) => {
                form.pending = None;
                form.error.clear();
                form.result = t("保存记录已核对。").into();
                if let Ok(links) = lib.store.note_links() { lib.note_links = links }
            }
            Err(error) => form.error = error.to_string(),
        }
        cx.notify();
    }

    pub fn render_note_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let Some(Modal::ExportNote(form)) = self.modal.as_ref() else { return None };
        let ready = !form.busy && form.document.is_some() && (!form.notion || form.pending.is_none());
        let destinations = div().flex().gap(px(4.)).p(px(4.)).rounded(px(9.)).bg(Theme::bg_sunken())
            .children([(true, "Notion", "link"), (false, t("本地文件"), "file-text")].into_iter().enumerate().map(|(i, (notion, label, icon))| {
                ui::ghost_button(("note-destination", i), Some(icon), Some(label.into())).flex_1().h(text_px(34.))
                    .when(form.notion == notion, |d| d.bg(Theme::bg_body()).text_color(Theme::accent()))
                    .when(!form.busy, |d| d.on_click(self.on(Cmd::NoteDestination(notion), cx)))
            }));
        let mut body = div().id("note-save-body").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().p(px(20.)).gap(px(16.))
            .child(ui::form_row(t("保存位置"), destinations).flex_none())
            .child(ui::form_row(t("笔记标题"), ui::field(&form.title, window, cx)).flex_none());
        if form.from_chat {
            let count = form.choices.iter().filter(|q| q.chosen).count();
            body = body.child(div().flex().flex_col().flex_none().gap(px(6.))
                .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(tf!("选择问答 · 已选 {} 条", "Questions · {} selected", count)))
                .child(div().id("note-choices").flex().flex_col().flex_none().max_h(text_px(144.)).overflow_y_scroll()
                    .children(form.choices.iter().enumerate().map(|(i, choice)| {
                        let label = choice.label.clone();
                        ui::menu_row(("note-choice", i), label.clone(), choice.chosen, None).flex_none().h(text_px(36.))
                            .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(label.clone())).into())
                            .when(!form.busy, |d| d.on_click(self.on(Cmd::NotePick(i), cx)))
                    })))
                .when(!form.subject.is_empty(), |d| d.child(ui::menu_row("note-include-personal", t("包含我的笔记"), form.include_notes, None).flex_none()
                    .when(!form.busy, |d| d.on_click(self.on(Cmd::NotePick(usize::MAX), cx))))));
        }
        if form.notion {
            body = body.child(ui::form_row(t("保存到此页面下"), ui::field(&form.parent, window, cx)).flex_none())
                .child(div().flex_none().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(t("首次保存会创建子页面，后续内容追加到同一页。相同内容不会重复保存。")));
            if let Some(pending) = &form.pending {
                body = body.child(div().flex().flex_col().flex_none().gap(px(8.)).p(px(12.)).bg(Theme::accent_soft()).rounded(px(8.))
                    .child(div().child(t("上次保存结果待核对。请先打开 Notion 检查内容；部分写入时请手动整理，不要标为未写入。")))
                    .child(ui::secondary_button("note-check-remote", t("打开 Notion")).on_click(self.on(Cmd::OpenUrl(pending.url()), cx)))
                    .child(ui::field(&form.recovery_page, window, cx))
                    .child(div().flex().flex_wrap().gap(px(8.))
                        .child(ui::secondary_button("note-confirm-saved", t("已核对，保存成功")).on_click(self.on(Cmd::NoteResolve(true), cx)))
                        .child(ui::secondary_button("note-confirm-missing", t("已核对，完全未写入")).on_click(self.on(Cmd::NoteResolve(false), cx)))));
            }
        } else {
            body = body.child(div().flex_none().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(t("导出为 Markdown 文件，图片一并保存在旁边的附件文件夹中。点击导出后选择文件位置。")));
        }
        if let Some(doc) = &form.document {
            body = body.child(div().flex().flex_col().flex_none().gap(px(8.))
                .child(div().flex().items_center().gap(px(8.))
                    .child(ui::ghost_button("note-preview-toggle", Some(if form.preview { "chevron-down" } else { "chevron-right" }), Some(t("内容预览").into()))
                        .on_click(self.on(Cmd::NotePreview, cx)))
                    .when(!doc.attachments.is_empty(), |d| d.child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(tf!("含 {} 张图片", "{} images included", doc.attachments.len())))))
                .when(form.preview, |d| d.child(div().id("note-preview").max_h(px(220.)).overflow_y_scroll().p(px(12.)).rounded(px(8.)).bg(Theme::bg_sunken())
                    .child(crate::markdown::render("note-preview-text", &doc.markdown, None, None)))));
        }
        body = body.when(!form.error.is_empty(), |d| d.child(ui::error_line(&form.error).flex_none()))
            .when(!form.result.is_empty(), |d| d.child(div().flex_none().text_color(Theme::success()).child(form.result.clone())))
            .when(!form.url.is_empty(), |d| d.child(div().flex().child(ui::ghost_button("note-result-link", Some("link"), Some(t("打开 Notion").into())).on_click(self.on(Cmd::OpenUrl(form.url.clone()), cx)))));
        Some(ui::dialog(600.)
            .child(ui::dialog_title(if form.from_chat { t("保存问答") } else { t("保存笔记") }).flex_none()).child(body)
            .child(div().flex().flex_none().justify_end().gap(px(8.)).px(px(20.)).py(px(14.)).border_t_1().border_color(Theme::line())
                .child(ui::secondary_button("note-close", if form.busy { t("收起") } else { t("关闭") }).on_click(self.on(Cmd::CloseModal, cx)))
                .child(ui::primary_button("note-save", if form.busy { t("正在保存…") } else if form.notion { t("保存到 Notion") } else { t("导出到本地") })
                    .when(!ready, |d| d.opacity(0.45).cursor_default())
                    .when(ready, |d| d.on_click(self.on(Cmd::NoteSave(form.notion), cx))))))
    }
}
