//! The guide of a library: where to start filling it, and searches the
//! assistant composes from a topic. Each proposed search is tried at once,
//! so the person judges it by what it finds, not by how it reads.

use crate::i18n::{label, t};
use crate::app::{Cmd, GuideIdea, Page, Workspace};
use crate::settings::SettingsPage;
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};

use wisp_core::sources;

/// One way to start, as a card to click.
fn choice(id: &'static str, icon: &'static str, title: &'static str, about: &'static str, recommended: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(14.))
        .w(px(520.)).max_w_full()
        .p(px(16.))
        .rounded(px(10.))
        .border_1()
        .border_color(if recommended { Theme::accent() } else { Theme::line() })
        .cursor_pointer()
        .hover(|s| s.bg(Theme::hover()))
        .child(ui::icon(icon, 22., if recommended { Theme::accent() } else { Theme::text_secondary() }))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().flex().items_center().gap(px(8.)).child(div().font_weight(FontWeight::SEMIBOLD).child(title)).when(recommended, |d| d.child(ui::chip(t("推荐"), Theme::accent(), Theme::accent_soft()))))
                .child(div().text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_tertiary()).child(about)),
        )
}

impl Workspace {
    pub fn render_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let guide = &lib.guide;
        if guide.choosing {
            return div()
                .flex()
                .flex_col()
                .id("guide-choices")
                .size_full()
                .overflow_y_scroll()
                .items_center()
                .px(px(16.))
                .py(px(24.))
                .gap(px(12.))
                .child(div().text_size(text_px(18.)).font_weight(FontWeight::SEMIBOLD).child(tf!("「{}」建好了，从哪里开始？", "“{}” is ready. Where would you like to start?", lib.info.name)))
                .child(div().pb(px(8.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("之后随时可以用其他方式继续添加。")))
                .child(choice("guide-search", "search", t("从检索文献开始"), t("写下主题或关键词，助手为 PubMed 等数据库拟出几条检索式并逐条试检索；你挑出靠谱的，一次导入，之后还能定期运行。"), true).on_click(self.on(Cmd::GuideSearch, cx)))
                .child(choice("guide-import", "folder-open", t("导入已有的文献"), t("从 Zotero、EndNote 等导出的 BibTeX / RIS 文件，一批 PDF，或整个文件夹（子文件夹的名字成为标签）。"), false).on_click(self.on(Cmd::ImportReferences, cx)))
                .child(choice("guide-zotero", "folder-open", t("从 Zotero 导入"), t("读取本机 Zotero 数据库，带入文献信息、标签、笔记和已下载的 PDF。"), false).on_click(self.on(Cmd::ImportZotero, cx)))
                .child(choice("guide-empty", "plus", t("先空着，自己添加"), t("通过 DOI / PMID 逐篇添加，或者手动录入。"), false).on_click(self.on(Cmd::SetPage(Page::Papers), cx)))
                .into_any_element();
        }

        let configured = !self.settings.profile_for_or_default(wisp_core::settings::Task::Chat).key().is_empty();
        let chosen = guide.ideas.iter().filter(|entry| entry.chosen).count();
        let cards: Vec<AnyElement> = guide.ideas.iter().enumerate().map(|(i, entry)| self.render_idea(i, entry, cx)).collect();
        let sources_row = div().flex().items_center().flex_wrap().gap(px(6.)).children(sources::KINDS.iter().map(|(key, name)| {
            let on = guide.kinds.iter().any(|k| k == key);
            div()
                .id(*key)
                .flex()
                .flex_none()
                .items_center()
                .h(text_px(28.))
                .px(px(10.))
                .rounded(px(6.))
                .border_1()
                .border_color(if on { Theme::accent() } else { Theme::line() })
                .when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()))
                .text_size(text_px(13.))
                .cursor_pointer()
                .child(t(name))
                .on_click(self.on(Cmd::GuideToggleSource(key.to_string()), cx))
        }));

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .child(div().text_size(text_px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("从检索文献开始")))
                    .child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("写下主题或关键词，助手为所选的数据库各拟几条取向不同的检索式，并立刻试检索。看命中的文献对不对，选出靠谱的再导入：宁可先宽一些，导入之后再逐篇判断。")))
                    .child(ui::field(&guide.keywords, window, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(10.))
                            .child(sources_row)
                            .child(div().flex_1())
                            .when(!configured, |d| d.child(ui::ghost_button("guide-settings", Some("settings-gear"), Some(t("先配置模型").into())).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
                            .child(ui::secondary_button("guide-manual", t("自己写检索式")).on_click(self.on(Cmd::NewSearch, cx)))
                            .child(ui::primary_button("guide-generate", if guide.busy { t("正在拟检索式…") } else if guide.ideas.is_empty() { t("拟检索式") } else { t("重新拟") }).when(guide.busy, |d| d.opacity(0.5)).on_click(self.on(Cmd::GuideGenerate, cx))),
                    )
                    .when(!guide.error.is_empty(), |d| d.child(ui::error_line(&guide.error))),
            )
            .child(if cards.is_empty() {
                ui::empty_state("search", if guide.busy { t("正在拟检索式…") } else { t("还没有检索式") }, t("主题和关键词会发送给你配置的模型；拟出的检索式会发送给所选的数据库试检索。试检索不会向文献库写入任何内容。")).into_any_element()
            } else {
                div().id("guide-ideas").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().px(px(16.)).pb(px(12.)).gap(px(12.)).children(cards).into_any_element()
            })
            .when(!guide.ideas.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.))
                        .px(px(16.))
                        .py(px(12.))
                        .border_t_1()
                        .border_color(Theme::line())
                        .child(div().flex_1().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(tf!("选用的检索式会保存下来并立即运行，每条取回最新的 {} 篇；导入的文献都是「待判断」，可以整批撤销。", "Selected queries are saved and run immediately, retrieving the latest {} papers each. Imported papers await screening; the batch can be undone.", sources::MAX_RESULTS)))
                        .child(ui::primary_button("guide-import-chosen", tf!("保存并导入所选的 {chosen} 条", "Save and import {chosen} selected queries")).when(chosen == 0, |d| d.opacity(0.5)).on_click(self.on(Cmd::GuideImport, cx))),
                )
            })
            .into_any_element()
    }

    /// A proposed search: what it is for, the query, and what trying it found.
    fn render_idea(&self, index: usize, entry: &GuideIdea, cx: &mut Context<Self>) -> AnyElement {
        let source = label(&sources::KINDS, &entry.idea.source).to_string();
        let found = match &entry.trial {
            None => div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("正在试检索…")).into_any_element(),
            Some(Err(error)) => ui::error_line(&tf!("试检索失败：{error}", "Search preview failed: {error}")).into_any_element(),
            Some(Ok(found)) => {
                let total = found["total"].as_u64().map(|n| tf!("数据库中共命中 {n} 篇。", "{n} results in the database.")).unwrap_or_default();
                let head = if found["fetched"] == 0 { tf!("{total}没有取回任何记录。", "{total}No records retrieved.") } else { tf!("{total}最新的 {} 篇里，{} 篇是新的，{} 篇已在文献库中：", "{total}Of the latest {} papers, {} are new and {} are already in this library:", found["fetched"], found["new"], found["inLibrary"]) };
                let titles = found["results"].as_array().into_iter().flatten().take(6).map(|record| {
                    let year = record["year"].as_i64().map(|y| format!("{y} · ")).unwrap_or_default();
                    div().truncate().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(format!("· {year}{}", record["title"].as_str().unwrap_or("")))
                });
                div().flex().flex_col().gap(px(3.)).child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(head)).children(titles).into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .rounded(px(10.))
            .border_1()
            .border_color(if entry.chosen { Theme::accent() } else { Theme::line() })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::chip(source, Theme::text_secondary(), Theme::bg_sunken()))
                    .child(div().flex_1().min_w_0().text_size(text_px(13.)).line_height(text_px(19.)).child(entry.idea.why.clone()))
                    .child(if entry.chosen { ui::primary_button(("idea", index), t("已选用")) } else { ui::secondary_button(("idea", index), t("选用")) }.on_click(self.on(Cmd::GuideToggleIdea(index), cx))),
            )
            .child(div().px(px(10.)).py(px(8.)).rounded(px(6.)).bg(Theme::bg_sunken()).text_size(text_px(13.)).line_height(text_px(20.)).child(entry.idea.query.clone()))
            .child(found)
            .into_any_element()
    }
}
