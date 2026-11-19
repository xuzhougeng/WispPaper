//! The command palette (⌘K / Ctrl+K): one box over the window that finds a command, or a paper of the
//! open library, by a few letters. Every row ends in a `Cmd`: the palette does nothing a click cannot.

use crate::app::{Cmd, ExportKind, Page, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::settings::SettingsPage;
use crate::shortcuts;
use crate::theme::{text_px, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::filter;

/// The most papers listed: more than the eye takes in, and one more word narrows them.
const PAPERS: usize = 50;

pub struct Row {
    pub icon: &'static str,
    pub label: SharedString,
    /// A command's shortcut, or a paper's authors and year.
    pub note: SharedString,
    pub cmd: Cmd,
}

pub struct Palette {
    pub input: Entity<TextInput>,
    pub rows: Vec<Row>,
    /// How many of the rows are commands; the papers come after them, under a line.
    pub commands: usize,
    /// The row the arrow keys are on; Enter takes it.
    pub active: usize,
    pub scroll: ScrollHandle,
    _typed: Subscription,
}

/// Whether a label holds every word typed, whatever the case.
fn holds(label: &str, query: &str) -> bool {
    let label = label.to_lowercase();
    query.to_lowercase().split_whitespace().all(|word| label.contains(word))
}

impl Workspace {
    /// Open the palette, or put it away again.
    pub fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() { return self.close_palette(window, cx) }
        self.menu = None;
        let input = cx.new(|cx| {
            let mut input = TextInput::new("输入命令，或搜索文献的题目、作者、标签…", cx);
            // The arrow keys move in the rows, and Esc closes the palette alone.
            input.choosing = true;
            input
        });
        let typed = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| this.palette_event(*event, window, cx));
        input.read(cx).focus(window);
        let (rows, commands) = self.palette_rows("");
        self.palette = Some(Palette { input, rows, commands, active: 0, scroll: ScrollHandle::new(), _typed: typed });
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        window.focus(&self.focus);
        cx.notify();
    }

    pub(crate) fn palette_event(&mut self, event: InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.as_ref() else { return };
        match event {
            InputEvent::Changed => {
                let (rows, commands) = self.palette_rows(palette.input.read(cx).text());
                if let Some(palette) = self.palette.as_mut() {
                    (palette.rows, palette.commands, palette.active) = (rows, commands, 0);
                    palette.scroll.scroll_to_item(0);
                }
            }
            InputEvent::Up | InputEvent::Down if !palette.rows.is_empty() => {
                let count = palette.rows.len();
                let active = (palette.active + if event == InputEvent::Up { count - 1 } else { 1 }) % count;
                if let Some(palette) = self.palette.as_mut() {
                    palette.active = active;
                    palette.scroll.scroll_to_item(active);
                }
            }
            InputEvent::Submit => return self.palette_take(palette.active, window, cx),
            InputEvent::Cancel => return self.close_palette(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Carry out a row. With nothing found the palette stays, for other words.
    pub fn palette_take(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.as_ref().is_none_or(|palette| at >= palette.rows.len()) { return }
        let Some(row) = self.palette.take().and_then(|palette| palette.rows.into_iter().nth(at)) else { return };
        window.focus(&self.focus);
        // Whatever is not a page or a choice of the settings is done in the library under them.
        if self.settings_view.is_some() && !matches!(row.cmd, Cmd::OpenSettings(_) | Cmd::SetAppearance(_)) { self.close_settings(window, cx) }
        self.run(row.cmd, window, cx);
    }

    /// What `query` finds: the commands on offer where the window is, then the open library's papers
    /// (only once something is typed). The second value is how many are commands.
    fn palette_rows(&self, query: &str) -> (Vec<Row>, usize) {
        let key = |id: &str| if id.is_empty() { String::new() } else { shortcuts::pretty(&shortcuts::key_of(id, &self.settings.shortcuts)) };
        // (icon, label, the shortcut that does the same, command). A fixed label is a key of `locales/en.json`.
        let mut fixed: Vec<(&'static str, &'static str, &'static str, Cmd)> = vec![];
        if let Some(lib) = self.lib.as_ref() {
            fixed.extend(crate::library::VIEWS.into_iter().map(|(_, icon, label, view)| (icon, label, "", Cmd::SetView(view))));
            fixed.extend([
                ("sparkle", "助手", "ask", Cmd::SetPage(Page::Ask)),
                ("search", "检索与导入", "", Cmd::SetPage(Page::Sources)),
                ("list-todo", "筛选", "", Cmd::SetPage(Page::Screen)),
                ("scan-text", "提取", "", Cmd::SetPage(Page::Extract)),
                ("pen-line", "写作", "", Cmd::SetPage(Page::Write)),
                ("image", "图片库", "", Cmd::SetPage(Page::Figures)),
                ("bin", "回收站", "", Cmd::SetPage(Page::Trash)),
                ("plus", "通过 DOI / PMID 添加…", "add", Cmd::AddById),
                ("plus", "导入 PDF 文件…", "import-pdf", Cmd::ImportPdfs),
                ("plus", "导入文件夹…", "", Cmd::ImportFolder),
                ("plus", "导入 BibTeX / RIS…", "", Cmd::ImportReferences),
                ("plus", "从 Zotero 导入…", "", Cmd::ImportZotero),
                ("plus", "手动录入…", "", Cmd::AddManual),
                ("search", "新建检索式", "", Cmd::NewSearch),
                ("sliders", "自定义词条", "", Cmd::ManageFields),
                ("panel-left", "显示 / 收起侧边栏", "sidebar", Cmd::ToggleSidebar),
            ]);
            if lib.selected.is_some() { fixed.extend([("expand", "展开 / 收起文献详情", "detail", Cmd::ToggleWide), ("eye", "进入 / 退出阅读模式", "reading", Cmd::ToggleReading)]) }
            // A paper asked about with its PDF beside: the two change places.
            if lib.selected.is_some() && lib.tab == crate::app::Tab::Ask && lib.split && !self.settings.floating.on { fixed.push(("arrow-left-right", "交换 PDF 与提问的位置", "", Cmd::SwapPanes)) }
            fixed.push(("arrow-left", "回到所有文献库", "home", Cmd::GoHome));
        } else {
            fixed.push(("plus", "新建文献库", "new-library", Cmd::NewLibrary));
            fixed.push(("library", "导入文献库", "", Cmd::ImportLibrary));
        }
        fixed.push(("folder-open", "数据目录", "", Cmd::RevealFolder));
        // Typed in either language, a fixed label is found.
        let mut rows: Vec<Row> = fixed.into_iter().filter(|(_, label, ..)| holds(&format!("{} {label}", t(label)), query)).map(|(icon, label, shortcut, cmd)| Row { icon, label: t(label).to_string().into(), note: key(shortcut).into(), cmd }).collect();
        // A label put together is found by `also` too: its words in the other language.
        let mut made = |icon: &'static str, label: String, also: &str, shortcut: &str, cmd: Cmd| {
            if holds(&format!("{label} {also}"), query) { rows.push(Row { icon, label: label.into(), note: key(shortcut).into(), cmd }) }
        };
        if self.lib.is_some() {
            made("export", tf!("导出：{}", "Export: {}", "BibTeX（.bib）"), "导出 export", "", Cmd::Export(ExportKind::Bibtex));
            made("export", tf!("导出：{}", "Export: {}", t("表格（.csv）")), "导出 表格 export spreadsheet", "", Cmd::Export(ExportKind::Csv));
            if let Some(id) = self.lib.as_ref().map(|lib| lib.info.id.clone()) {
                made("export", tf!("导出：{}", "Export: {}", t("整个文献库，含 PDF（.wisppaper）…")), "导出 文献库 export library wisppaper", "", Cmd::ExportLibrary(id.clone(), true));
                made("export", tf!("导出：{}", "Export: {}", t("整个文献库，不含 PDF（.wisppaper）…")), "导出 文献库 export library wisppaper", "", Cmd::ExportLibrary(id, false));
            }
        }
        let open = self.lib.as_ref().map(|lib| lib.info.id.as_str());
        for info in self.libraries.iter().filter(|info| Some(info.id.as_str()) != open) { made("library", tf!("打开文献库：{}", "Open library: {}", info.name), "打开文献库 open library", "", Cmd::OpenLibrary(info.id.clone())) }
        for (label, also, value) in [("跟随系统", "system", ""), ("浅色", "light", "light"), ("深色", "dark", "dark")] { made("palette", tf!("外观：{}", "Appearance: {}", t(label)), &format!("外观 主题 {label} appearance theme {also}"), "", Cmd::SetAppearance(value.into())) }
        for page in SettingsPage::ALL { made(page.icon(), tf!("设置 › {}", "Settings › {}", page.title()), "设置 settings", if page == SettingsPage::General { "settings" } else { "" }, Cmd::OpenSettings(page)) }
        let commands = rows.len();
        if !query.trim().is_empty() && let Some(lib) = self.lib.as_ref() {
            // The words are looked for where the list's search box looks: title, authors, tags, abstract, notes.
            rows.extend(lib.papers.iter().filter(|paper| filter::says(paper, query)).take(PAPERS).map(|paper| Row {
                icon: "file-text",
                label: if paper.title.is_empty() { t("（无题目）").to_string().into() } else { paper.title.clone().into() },
                note: format!("{} {}", crate::i18n::short_authors(paper), paper.year.map_or(String::new(), |year| year.to_string())).trim().to_string().into(),
                // The paper opens beside the page in view, as one named in an import's report does.
                cmd: Cmd::OpenImported(paper.id.clone()),
            }));
        }
        (rows, commands)
    }

    pub fn render_palette(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let rows: Vec<AnyElement> = palette
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                // The line over the papers is inside the first one's cell: the n-th child stays the n-th row for the arrow keys' scrolling.
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .when(i == palette.commands && i > 0, |d| d.child(div().flex_none().h(px(1.)).mx(px(6.)).my(px(4.)).bg(Theme::line())))
                    .child(
                        div()
                            .id(("palette-row", i))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(10.))
                            .h(text_px(32.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::hover()))
                            .when(i == palette.active, |d| d.bg(Theme::hover()))
                            .on_click(cx.listener(move |this, _, window, cx| this.palette_take(i, window, cx)))
                            .child(ui::icon(row.icon, 16., Theme::text_secondary()))
                            .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
                            .child(div().flex_none().max_w(px(220.)).truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(row.note.clone())),
                    )
                    .into_any_element()
            })
            .collect();
        let found = !rows.is_empty();
        let card = ui::dialog(600.)
            .max_h(px(440.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex().flex_none().items_center().gap(px(10.)).h(text_px(46.)).px(px(16.)).child(ui::icon("search", 16., Theme::text_tertiary())).child(div().flex().flex_1().min_w_0().child(palette.input.clone())))
            .child(div().flex_none().h(px(1.)).bg(Theme::line()))
            .child(div().id("palette-rows").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&palette.scroll).p(px(6.)).children(rows).when(!found, |d| d.child(div().px(px(10.)).py(px(8.)).text_color(Theme::text_tertiary()).child(t("没有匹配的命令或文献")))));
        Some(
            div()
                .id("palette-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(96.))
                .bg(Theme::scrim())
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::holds;

    #[test]
    fn every_word_typed_is_looked_for_whatever_the_case() {
        assert!(holds("Open settings 打开设置", "SET open"));
        assert!(holds("Open settings 打开设置", "设置"));
        assert!(holds("anything", "  "));
        assert!(!holds("Open settings 打开设置", "open library"));
    }
}
