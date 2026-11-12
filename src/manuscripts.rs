//! 写作's manuscripts in the window: which one is written and which of its
//! sections, making and removing them, the sections' names, kinds and
//! order, the whole manuscript to read, export, and each section's history.
//! The rules (what a new manuscript holds, when a snapshot is worth keeping,
//! what an export looks like) are `wisp_core::manuscript`'s.

use crate::app::{Cmd, MenuItem, MenuKind, Modal, Workspace, Writing};
use crate::input::TextInput;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::manuscript::{self, LANGUAGES, Manuscript, Section};
use wisp_core::model::label;
use wisp_core::structure::SECTIONS;

/// What a manuscript is exported as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManuscriptExport {
    Markdown,
    Docx,
    /// The whole text to the clipboard.
    Copy,
}

impl Writing {
    /// The manuscript being written.
    pub fn current(&self) -> Option<&Manuscript> {
        self.manuscripts.iter().find(|m| m.id == self.manuscript)
    }

    /// The section being written.
    pub fn section(&self) -> Option<&Section> {
        self.current().and_then(|m| m.section(&self.section_id))
    }

    /// Which part of the library's papers the candidates come from: the
    /// section's kind, a key of `structure::SECTIONS`.
    pub fn kind(&self) -> &str {
        self.section().map_or("introduction", |section| section.kind.as_str())
    }

    /// What the manuscript is written in: a code of `manuscript::LANGUAGES`.
    pub fn language(&self) -> &str {
        self.current().map_or(manuscript::DEFAULT_LANGUAGE, |m| m.language.as_str())
    }

    pub(crate) fn section_mut(&mut self) -> Option<&mut Section> {
        let (manuscript, section) = (self.manuscript.clone(), self.section_id.clone());
        self.manuscripts.iter_mut().find(|m| m.id == manuscript)?.sections.iter_mut().find(|s| s.id == section)
    }

    /// The manuscript and section to open with: the one last written in.
    pub(crate) fn latest(manuscripts: &[Manuscript]) -> (String, String) {
        let Some(manuscript) = manuscripts.iter().max_by(|a, b| a.updated_at.cmp(&b.updated_at).then_with(|| latest_section(a).cmp(latest_section(b)))) else { return Default::default() };
        // The section last written in; of those written at the same time (a migration), the first with text.
        let key = |s: &Section| (!s.body.trim().is_empty(), s.updated_at.clone());
        let section = manuscript.sections.iter().enumerate().max_by(|(n, a), (m, b)| key(a).cmp(&key(b)).then(m.cmp(n))).map(|(_, s)| s.id.clone()).unwrap_or_default();
        (manuscript.id.clone(), section)
    }
}

fn latest_section(manuscript: &Manuscript) -> &str {
    manuscript.sections.iter().map(|s| s.updated_at.as_str()).max().unwrap_or("")
}

fn file_name(title: &str) -> String {
    let name: String = title.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-").trim().chars().take(80).collect();
    if name.is_empty() { manuscript::UNTITLED.to_string() } else { name }
}

impl Workspace {
    // ---- which manuscript and section

    /// Make sure the library has a manuscript to write in.
    pub(crate) fn write_ensure(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut().filter(|l| l.writing.manuscripts.is_empty()) else { return };
        match lib.store.create_manuscript("", manuscript::DEFAULT_LANGUAGE) {
            Ok(made) => {
                let (id, section) = (made.id.clone(), made.sections.get(1).map(|s| s.id.clone()).unwrap_or_default());
                lib.writing.manuscripts.push(made);
                self.write_show(id, section, cx);
            }
            Err(err) => self.fail(format!("稿件没有建好：{err:#}"), cx),
        }
    }

    /// Keep the section being written as a snapshot when it changed: on leaving it, or after a pause.
    pub(crate) fn write_snapshot(&mut self, why: &str) {
        let Some(lib) = self.lib.as_mut() else { return };
        let section = lib.writing.section_id.clone();
        if !section.is_empty() { let _ = lib.store.take_snapshot(&section, why); }
    }

    /// Write this section of this manuscript (the first one when it is not
    /// there), after keeping the one left.
    pub(crate) fn write_show(&mut self, manuscript_id: String, section_id: String, cx: &mut Context<Self>) {
        let leaving = self.lib.as_ref().is_some_and(|l| l.writing.manuscript != manuscript_id || l.writing.section_id != section_id);
        if leaving { self.write_snapshot("leave") }
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(manuscript) = writing.manuscripts.iter().find(|m| m.id == manuscript_id).or(writing.manuscripts.first()) else { return };
        let section = manuscript.section(&section_id).or(manuscript.sections.first());
        let (body, title) = (section.map(|s| s.body.clone()).unwrap_or_default(), manuscript.title.clone());
        writing.section_id = section.map(|s| s.id.clone()).unwrap_or_default();
        writing.manuscript = manuscript.id.clone();
        writing.assist.leave();
        writing.editor.update(cx, |input, cx| input.set_text(&body, cx));
        writing.title.update(cx, |input, cx| input.set_text(&title, cx));
        self.write_openers(cx);
        cx.notify();
    }

    /// Read the manuscripts again after a change to their shape, staying where the person was.
    fn write_reload(&mut self, section_id: Option<String>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        match lib.store.manuscripts() {
            Ok(list) => lib.writing.manuscripts = list,
            Err(err) => return self.fail(format!("{err:#}"), cx),
        }
        let (manuscript, section) = (lib.writing.manuscript.clone(), section_id.unwrap_or_else(|| lib.writing.section_id.clone()));
        // Already left: the snapshot of the section left was taken by the caller.
        lib.writing.section_id = section.clone();
        self.write_show(manuscript, section, cx);
    }

    /// The section a `write:` step or `Cmd::WriteSection` names: its id, or the first of a kind.
    pub(crate) fn write_section(&mut self, wanted: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(manuscript) = lib.writing.current() else { return };
        let Some(section) = manuscript.section(&wanted).or_else(|| manuscript.sections.iter().find(|s| s.kind == wanted)).map(|s| s.id.clone()) else { return };
        let id = manuscript.id.clone();
        lib.writing.editor.read(cx).focus(window);
        if let Some(lib) = self.lib.as_mut() { lib.writing.reading = false }
        self.write_show(id, section, cx);
    }

    /// Typing in a section: keep the text now, and a snapshot once typing pauses.
    pub(crate) fn write_typed(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let section = lib.writing.section_id.clone();
        if section.is_empty() { return }
        if let Err(err) = lib.store.save_section_body(&section, &text) { return self.fail(format!("草稿没有保存：{err:#}"), cx) }
        if let Some(kept) = lib.writing.section_mut() { kept.body = text }
        lib.writing.typed += 1;
        let (typed, library) = (lib.writing.typed, lib.info.id.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(manuscript::PAUSE_SECS)).await;
            this.update(cx, |this, _| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && l.writing.typed == typed) else { return };
                let _ = lib.store.take_snapshot(&section, "pause");
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn write_title_typed(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let id = lib.writing.manuscript.clone();
        if let Err(err) = lib.store.rename_manuscript(&id, &title) { return self.fail(format!("题目没有保存：{err:#}"), cx) }
        if let Some(m) = lib.writing.manuscripts.iter_mut().find(|m| m.id == id) { m.title = title.trim().to_string() }
        cx.notify();
    }

    // ---- commands

    pub(crate) fn run_manuscript(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            Cmd::WriteSection(section) => self.write_section(section, window, cx),
            Cmd::WriteManuscript(id) => {
                let section = self.lib.as_ref().and_then(|l| l.writing.manuscripts.iter().find(|m| m.id == id)).map(|m| Writing::latest(std::slice::from_ref(m)).1).unwrap_or_default();
                if let Some(lib) = self.lib.as_mut() { lib.writing.reading = false }
                self.write_show(id, section, cx);
            }
            Cmd::NewManuscript => {
                let Some(lib) = self.lib.as_mut() else { return };
                let language = lib.writing.language().to_string();
                match lib.store.create_manuscript("", &language) {
                    Ok(made) => {
                        let (id, section) = (made.id.clone(), made.sections.get(1).map(|s| s.id.clone()).unwrap_or_default());
                        lib.writing.manuscripts.push(made);
                        lib.writing.reading = false;
                        self.write_show(id, section, cx);
                        if let Some(lib) = self.lib.as_ref() { lib.writing.title.read(cx).focus(window) }
                    }
                    Err(err) => self.fail(format!("{err:#}"), cx),
                }
            }
            Cmd::AskRemoveManuscript(id) => {
                let Some(m) = self.lib.as_ref().and_then(|l| l.writing.manuscripts.iter().find(|m| m.id == id)) else { return };
                let title = m.shown_title().to_string();
                self.modal = Some(Modal::Confirm { title: format!("移除稿件「{title}」？"), message: "它不再列在写作里。文字仍留在这个文献库的数据库中，不会被删除。".into(), action: "移除".into(), cmd: Cmd::RemoveManuscript(id) });
            }
            Cmd::RemoveManuscript(id) => {
                self.modal = None;
                self.write_snapshot("leave");
                let Some(lib) = self.lib.as_mut() else { return };
                if let Err(err) = lib.store.remove_manuscript(&id) { return self.fail(format!("{err:#}"), cx) }
                lib.writing.manuscripts.retain(|m| m.id != id);
                let (manuscript, section) = Writing::latest(&lib.writing.manuscripts);
                lib.writing.manuscript = manuscript.clone();
                lib.writing.section_id = section.clone();
                if manuscript.is_empty() { self.write_ensure(cx) } else { self.write_show(manuscript, section, cx) }
            }
            Cmd::WriteLanguage(code) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let id = lib.writing.manuscript.clone();
                if let Err(err) = lib.store.set_manuscript_language(&id, &code) { return self.fail(format!("{err:#}"), cx) }
                if let Some(m) = lib.writing.manuscripts.iter_mut().find(|m| m.id == id) { m.language = code }
            }
            Cmd::EditSection(editing) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let shown = editing.as_ref().and_then(|id| lib.writing.current()?.section(id));
                let (name, kind) = shown.map_or((String::new(), "other".to_string()), |s| (s.name.clone(), s.kind.clone()));
                let input = cx.new(|cx| TextInput::new("例如 Root hairs、研究现状", cx).with_text(&name));
                input.read(cx).focus(window);
                self.modal_error.clear();
                self.modal = Some(Modal::Section { editing, name: input, kind });
            }
            Cmd::PickSectionKind(key) => {
                if let Some(Modal::Section { kind, name, editing }) = self.modal.as_mut() {
                    // A new section's name follows its kind until the person types one.
                    let language = self.lib.as_ref().map_or("en", |l| l.writing.language()).to_string();
                    let typed = name.read(cx).text().trim().to_string();
                    if editing.is_none() && (typed.is_empty() || typed == manuscript::default_name(kind, &language)) && key != "other" {
                        let named = manuscript::default_name(&key, &language);
                        name.update(cx, |input, cx| input.set_text(&named, cx));
                    }
                    *kind = key;
                }
            }
            Cmd::MoveSection(id, by) => {
                let Some(lib) = self.lib.as_mut() else { return };
                if let Err(err) = lib.store.move_section(&id, by) { return self.fail(format!("{err:#}"), cx) }
                self.write_reload(None, cx);
            }
            Cmd::AskRemoveSection(id) => {
                let Some(section) = self.lib.as_ref().and_then(|l| l.writing.current()?.section(&id)).cloned() else { return };
                if self.lib.as_ref().and_then(|l| l.writing.current()).is_some_and(|m| m.sections.len() <= 1) { return self.fail("稿件至少要留一节。", cx) }
                if section.body.trim().is_empty() { return self.run_manuscript(Cmd::RemoveSection(id), window, cx) }
                let words = section.body.split_whitespace().count();
                self.modal = Some(Modal::Confirm { title: format!("删除「{}」这一节？", section.name), message: format!("这一节写了 {words} 词，连同它的历史版本会一起删除，不能恢复。要留着文字，先用「导出」或「复制全文」。"), action: "删除".into(), cmd: Cmd::RemoveSection(id) });
            }
            Cmd::RemoveSection(id) => {
                self.modal = None;
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(manuscript) = lib.writing.current() else { return };
                let at = manuscript.sections.iter().position(|s| s.id == id).unwrap_or(0);
                let next = manuscript.sections.iter().filter(|s| s.id != id).nth(at.saturating_sub(1)).map(|s| s.id.clone());
                let staying = if lib.writing.section_id == id { next } else { None };
                if let Err(err) = lib.store.remove_section(&id) { return self.fail(format!("{err:#}"), cx) }
                self.write_reload(staying, cx);
            }
            Cmd::WriteReading(reading) => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.writing.reading = reading;
                    if !reading { lib.writing.editor.read(cx).focus(window) }
                }
                if reading { self.write_snapshot("leave") }
            }
            Cmd::ExportManuscript(kind) => self.export_manuscript(kind, cx),
            Cmd::WriteHistory => {
                self.write_snapshot("leave");
                let Some(lib) = self.lib.as_ref() else { return };
                let section = lib.writing.section_id.clone();
                match lib.store.snapshots(&section) {
                    Ok(snapshots) => {
                        self.modal_error.clear();
                        self.modal = Some(Modal::History { section, snapshots, chosen: 0 });
                    }
                    Err(err) => self.fail(format!("{err:#}"), cx),
                }
            }
            Cmd::HistoryPick(n) => {
                if let Some(Modal::History { chosen, .. }) = self.modal.as_mut() { *chosen = n }
            }
            _ => {}
        }
        cx.notify();
    }

    /// The section dialog's or the history's button.
    pub(crate) fn submit_manuscript_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.modal.as_ref() {
            Some(Modal::Section { editing, name, kind }) => {
                let (editing, name, kind) = (editing.clone(), name.read(cx).text().trim().to_string(), kind.clone());
                let Some(lib) = self.lib.as_mut() else { return };
                let manuscript = lib.writing.manuscript.clone();
                let result = match &editing {
                    Some(id) => lib.store.edit_section(id, &name, &kind).map(|_| id.clone()),
                    None => lib.store.add_section(&manuscript, &name, &kind).map(|s| s.id),
                };
                match result {
                    Ok(id) => {
                        self.modal = None;
                        if editing.is_none() { self.write_snapshot("leave") }
                        // A section's kind decides its candidates: they are looked for again.
                        if let Some(lib) = self.lib.as_mut() { lib.writing.search.asked = None }
                        self.write_reload(Some(id), cx);
                        if let Some(lib) = self.lib.as_ref() { lib.writing.editor.read(cx).focus(window) }
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            Some(Modal::History { snapshots, chosen, .. }) => {
                let Some(snapshot) = snapshots.get(*chosen).cloned() else { self.modal = None; return cx.notify() };
                let Some(lib) = self.lib.as_mut() else { return };
                match lib.store.restore_snapshot(snapshot.id) {
                    Ok(_) => {
                        self.modal = None;
                        self.write_reload(Some(snapshot.section_id), cx);
                        self.say("已恢复这一版。恢复前的文字也留了一个版本，可以再换回去。", cx);
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn export_manuscript(&mut self, kind: ManuscriptExport, cx: &mut Context<Self>) {
        let Some(manuscript) = self.lib.as_ref().and_then(|l| l.writing.current()).cloned() else { return };
        if manuscript.sections.iter().all(|s| s.body.trim().is_empty()) { return self.fail("这份稿件还没有写什么。", cx) }
        let (bytes, extension) = match kind {
            ManuscriptExport::Copy => {
                cx.write_to_clipboard(ClipboardItem::new_string(manuscript::plain_text(&manuscript)));
                return self.say("已复制整份稿件。", cx);
            }
            ManuscriptExport::Markdown => (manuscript::markdown(&manuscript).into_bytes(), "md"),
            ManuscriptExport::Docx => (manuscript::docx(&manuscript), "docx"),
        };
        let name = format!("{}.{extension}", file_name(manuscript.shown_title()));
        let directory = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = picked.await {
                if path.extension().is_none() { path.set_extension(extension); }
                let result = std::fs::write(&path, bytes);
                this.update(cx, |this, cx| match result {
                    Ok(()) => this.say(format!("已导出到 {}", path.display()), cx),
                    Err(err) => this.fail(format!("无法写入 {}：{err}", path.display()), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    /// The items of the writing page's menus.
    pub(crate) fn manuscript_menu(&self, kind: &MenuKind) -> Vec<MenuItem> {
        let item = |label: String, checked: bool, cmd: Cmd| MenuItem { label: label.into(), checked, note: None, cmd };
        let Some(writing) = self.lib.as_ref().map(|l| &l.writing) else { return vec![] };
        let mut items = vec![];
        match kind {
            MenuKind::Manuscripts => {
                for m in &writing.manuscripts {
                    let words: usize = m.sections.iter().map(|s| s.body.split_whitespace().count()).sum();
                    items.push(MenuItem { label: m.shown_title().to_string().into(), checked: m.id == writing.manuscript, note: Some(format!("{words} 词").into()), cmd: Cmd::WriteManuscript(m.id.clone()) });
                }
                items.push(item("新建稿件".into(), false, Cmd::NewManuscript));
                if writing.manuscripts.len() > 1 || writing.current().is_some_and(|m| m.sections.iter().any(|s| !s.body.trim().is_empty())) {
                    items.push(item("移除这份稿件…".into(), false, Cmd::AskRemoveManuscript(writing.manuscript.clone())));
                }
            }
            MenuKind::Section(id) => {
                let Some(m) = writing.current() else { return items };
                let at = m.sections.iter().position(|s| s.id == *id).unwrap_or(0);
                items.push(item("名称与类型…".into(), false, Cmd::EditSection(Some(id.clone()))));
                if at > 0 { items.push(item("前移".into(), false, Cmd::MoveSection(id.clone(), -1))) }
                if at + 1 < m.sections.len() { items.push(item("后移".into(), false, Cmd::MoveSection(id.clone(), 1))) }
                items.push(item("历史版本…".into(), false, Cmd::WriteHistory));
                if m.sections.len() > 1 { items.push(item("删除这一节…".into(), false, Cmd::AskRemoveSection(id.clone()))) }
            }
            MenuKind::ManuscriptExport => {
                items.push(item("Markdown（.md）".into(), false, Cmd::ExportManuscript(ManuscriptExport::Markdown)));
                items.push(item("Word（.docx）".into(), false, Cmd::ExportManuscript(ManuscriptExport::Docx)));
                items.push(item("复制全文".into(), false, Cmd::ExportManuscript(ManuscriptExport::Copy)));
            }
            MenuKind::WriteLanguage => {
                for (code, name) in LANGUAGES { items.push(item(format!("{name}（{code}）"), writing.language() == code, Cmd::WriteLanguage(code.to_string()))) }
            }
            _ => {}
        }
        items
    }

    // ---- render

    /// Above the editor: the title, the manuscript's own buttons, and its sections.
    pub(crate) fn render_manuscript_bar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let Some(manuscript) = writing.current() else { return div() };
        let language = LANGUAGES.iter().find(|(code, _)| *code == manuscript.language).map_or(manuscript.language.as_str(), |(_, name)| name);
        let tool = |id: &'static str, icon: &'static str, text: Option<SharedString>| ui::ghost_button(id, Some(icon), text).h(px(26.)).px(px(7.)).text_size(px(12.));
        let pills: Vec<AnyElement> = manuscript
            .sections
            .iter()
            .enumerate()
            .map(|(n, section)| {
                let on = section.id == writing.section_id && !writing.reading;
                let empty = section.body.trim().is_empty();
                div()
                    .id(("write-part", n))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(14.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .border_1()
                    .when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).border_color(Theme::accent().opacity(0.35)).font_weight(FontWeight::MEDIUM))
                    .when(!on, |d| d.border_color(Theme::line()).text_color(if empty { Theme::text_tertiary() } else { Theme::text_secondary() }).hover(|s| s.bg(Theme::hover())))
                    .on_click(self.on(Cmd::WriteSection(section.id.clone()), cx))
                    .child(section.name.clone())
                    .when(on, |d| d.child(div().id("write-part-menu").flex().items_center().justify_center().size(px(18.)).rounded(px(9.)).hover(|s| s.bg(Theme::accent().opacity(0.12))).on_click(self.on_menu(MenuKind::Section(section.id.clone()), cx)).child(ui::icon("chevron-down", 12., Theme::accent()))))
                    .into_any_element()
            })
            .collect();
        let add = div().id("write-part-add").flex().flex_none().items_center().justify_center().size(px(28.)).rounded(px(14.)).cursor_pointer().border_1().border_color(Theme::line()).hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::EditSection(None), cx)).child(ui::icon("plus", 14., Theme::text_secondary()));
        let title_focused = writing.title.focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(6.))
                            .py(px(2.))
                            .ml(px(-6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(if title_focused { Theme::accent().opacity(0.55) } else { transparent_black() })
                            .hover(|s| s.bg(Theme::hover()))
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(writing.title.clone()),
                    )
                    .child(tool("write-manuscripts", "list", Some(format!("稿件 {}", writing.manuscripts.len()).into())).on_click(self.on_menu(MenuKind::Manuscripts, cx)))
                    .child(tool("write-language", "chevron-down", Some(language.to_string().into())).on_click(self.on_menu(MenuKind::WriteLanguage, cx)))
                    .child(tool("write-history", "history", None).on_click(self.on(Cmd::WriteHistory, cx)))
                    .child(tool("write-export", "export", Some("导出".into())).on_click(self.on_menu(MenuKind::ManuscriptExport, cx)))
                    .child(if writing.reading { tool("write-reading", "pen-line", Some("写作".into())).on_click(self.on(Cmd::WriteReading(false), cx)) } else { tool("write-reading", "eye", Some("通读".into())).on_click(self.on(Cmd::WriteReading(true), cx)) }),
            )
            .child(div().flex().items_center().flex_wrap().gap(px(6.)).children(pills).child(add))
    }

    /// The whole manuscript, to be read through; a heading opens its section.
    pub(crate) fn render_manuscript_reading(&self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let Some(manuscript) = lib.writing.current() else { return div().into_any_element() };
        let sections: Vec<AnyElement> = manuscript
            .sections
            .iter()
            .enumerate()
            .map(|(n, section)| {
                let paragraphs: Vec<Div> = manuscript::paragraphs(&section.body).map(|line| div().text_size(px(15.)).line_height(px(26.)).text_color(Theme::text()).child(line.to_string())).collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .id(("write-read-heading", n))
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .cursor_pointer()
                            .on_click(self.on(Cmd::WriteSection(section.id.clone()), cx))
                            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text()).hover(|s| s.text_color(Theme::accent())).child(section.name.clone()))
                            .when(section.kind != "other", |d| d.child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(label(&SECTIONS, &section.kind).to_string()))),
                    )
                    .when(paragraphs.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(Theme::text_tertiary()).child("这一节还没有写。")))
                    .children(paragraphs)
                    .into_any_element()
            })
            .collect();
        div()
            .id("write-reading-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .child(div().flex().justify_center().px(px(28.)).py(px(24.)).child(div().flex().flex_col().gap(px(26.)).w_full().max_w(px(720.)).child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).line_height(px(30.)).child(manuscript.shown_title().to_string())).children(sections)))
            .into_any_element()
    }

    /// The dialog for a section's name and kind.
    pub(crate) fn render_section_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let Some(Modal::Section { editing, name, kind }) = self.modal.as_ref() else { return None };
        let kinds: Vec<AnyElement> = SECTIONS
            .iter()
            .enumerate()
            .map(|(n, (key, shown))| {
                let on = kind == key;
                div().id(("section-kind", n)).flex().flex_none().items_center().h(px(28.)).px(px(12.)).rounded(px(14.)).text_size(px(13.)).cursor_pointer().border_1().when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).border_color(Theme::accent().opacity(0.35))).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(self.on(Cmd::PickSectionKind(key.to_string()), cx)).child(shown.to_string()).into_any_element()
            })
            .collect();
        let hint = if kind == "other" { "候选句来自文献里不属于导言、方法、结果、讨论的段落。综述的各节、致谢、展望都可以选它。" } else { "候选句来自文献库里的文献在这一部分的句子。" };
        Some(
            ui::dialog(480.)
                .child(ui::dialog_title(if editing.is_some() { "这一节的名称与类型" } else { "添加一节" }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .px(px(20.))
                        .py(px(8.))
                        .child(ui::form_row("名称", ui::field(name, window, cx)))
                        .child(ui::form_row("类型", div().flex().flex_wrap().gap(px(6.)).children(kinds)))
                        .child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(hint))
                        .child(ui::error_line(&self.modal_error)),
                )
                .child(self.manuscript_footer(if editing.is_some() { "保存" } else { "添加" }, true, cx)),
        )
    }

    fn manuscript_footer(&self, confirm: &'static str, enabled: bool, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .justify_end()
            .gap(px(8.))
            .px(px(20.))
            .py(px(14.))
            .child(ui::secondary_button("modal-cancel", "取消").on_click(self.on(Cmd::CloseModal, cx)))
            .when(enabled, |d| d.child(ui::primary_button("modal-ok", confirm).on_click(self.on(Cmd::SubmitModal, cx))))
    }

    /// A section's earlier versions: one to look at, and to restore.
    pub(crate) fn render_history_dialog(&self, cx: &mut Context<Self>) -> Option<Div> {
        let Some(Modal::History { section, snapshots, chosen }) = self.modal.as_ref() else { return None };
        let name = self.lib.as_ref().and_then(|l| l.writing.current()?.section(section)).map(|s| s.name.clone()).unwrap_or_default();
        let why = |why: &str| match why {
            "pause" => "停笔时",
            "leave" => "离开时",
            "restore" => "恢复前",
            _ => "",
        };
        let rows: Vec<AnyElement> = snapshots
            .iter()
            .enumerate()
            .map(|(n, snapshot)| {
                let on = n == *chosen;
                let words = snapshot.body.split_whitespace().count();
                div()
                    .id(("history-row", n))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(Theme::accent_soft()))
                    .when(!on, |d| d.hover(|s| s.bg(Theme::hover())))
                    .on_click(self.on(Cmd::HistoryPick(n), cx))
                    .child(div().text_size(px(13.)).text_color(if on { Theme::accent() } else { Theme::text() }).child(wisp_core::time::short_time(&snapshot.taken_at)))
                    .child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(format!("{} · {words} 词", why(&snapshot.why))))
                    .into_any_element()
            })
            .collect();
        let shown = snapshots.get(*chosen);
        let body = match shown {
            None => div().flex().flex_col().px(px(20.)).py(px(12.)).child(ui::empty_state("history", "还没有历史版本", &format!("停笔 {} 秒，或换到别的一节时，这一节的文字有了变化就会留一个版本，最多 {} 个。", manuscript::PAUSE_SECS, manuscript::KEEP_SNAPSHOTS))),
            Some(snapshot) => div()
                .flex()
                .h(px(380.))
                .gap(px(12.))
                .px(px(20.))
                .py(px(8.))
                .child(div().id("history-list").flex().flex_col().flex_none().w(px(170.)).h_full().overflow_y_scroll().gap(px(2.)).children(rows))
                .child(
                    div()
                        .id("history-text")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        .gap(px(10.))
                        .p(px(14.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(Theme::line())
                        .bg(Theme::bg_body())
                        .when(snapshot.body.trim().is_empty(), |d| d.child(div().text_size(px(13.)).text_color(Theme::text_tertiary()).child("（这一版是空的）")))
                        .children(manuscript::paragraphs(&snapshot.body).map(|line| div().text_size(px(14.)).line_height(px(23.)).text_color(Theme::text()).child(line.to_string()))),
                ),
        };
        Some(
            ui::dialog(680.)
                .child(ui::dialog_title(format!("「{name}」的历史版本")))
                .child(body)
                .child(div().px(px(20.)).child(ui::error_line(&self.modal_error)))
                .child(div().px(px(20.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child("恢复时，现在的文字会先留一个版本，所以恢复也可以撤回。"))
                .child(self.manuscript_footer("恢复这一版", shown.is_some(), cx)),
        )
    }
}

