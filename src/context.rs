//! What the hands expect of a list: the arrow keys move in the papers on
//! show (and in an open menu), Enter opens, Delete moves to the trash; and a
//! right click opens a menu of what can be done with the thing under it.

use crate::app::{Cmd, MenuItem, MenuKind, Page, Tab, Workspace};
use gpui::*;
use wisp_core::model::READINGS;

/// A key that moves: in an open menu, else in the list of papers on show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    First,
    Last,
    /// Enter.
    Open,
    /// Delete, or ⌘⌫ / Ctrl+Backspace.
    Trash,
}

impl Workspace {
    /// What a right click on something does: the menu of `kind`, where the pointer is.
    pub fn on_context(&self, kind: MenuKind, cx: &mut Context<Self>) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            // The innermost thing under the pointer answers; what it lies in stays quiet.
            cx.stop_propagation();
            this.open_menu(kind.clone(), event.position, cx);
        })
    }

    /// The items of a right-click menu, and the ones a line is drawn above.
    pub(crate) fn context_menu(&self, kind: &MenuKind) -> (Vec<MenuItem>, Vec<usize>) {
        let item = |label: &str, cmd: Cmd| MenuItem { label: label.to_string().into(), checked: false, note: None, cmd };
        let (mut items, mut breaks) = (vec![], vec![]);
        let Some(lib) = &self.lib else { return (items, breaks) };
        match kind {
            MenuKind::Paper(id) => {
                let Some(p) = lib.paper(id) else { return (items, breaks) };
                let pdf = lib.pdf_file(id);
                if pdf.is_some() { items.push(item("打开 PDF", Cmd::OpenPaper(id.clone(), Tab::Pdf))) }
                items.push(item("查看信息", Cmd::OpenPaper(id.clone(), Tab::Info)));
                items.push(item("就这篇提问", Cmd::OpenPaper(id.clone(), Tab::Ask)));

                breaks.push(items.len());
                items.push(item(if p.starred { "取消收藏" } else { "收藏" }, Cmd::ToggleStar(id.clone())));
                for (value, label) in READINGS {
                    items.push(MenuItem { label: format!("标为{label}").into(), checked: p.reading == value, note: None, cmd: Cmd::SetReading(id.clone(), value.into()) });
                }

                breaks.push(items.len());
                items.push(item("编辑题目、信息与词条…", Cmd::EditPaper(id.clone())));
                if p.doi.is_empty() && p.pmid.is_empty() { items.push(item("按题目补全…", Cmd::CompletePaper(id.clone()))) }
                items.push(item(if pdf.is_some() { "再添加一个文件…" } else { "添加 PDF 或附件…" }, Cmd::AttachPdf(id.clone())));
                if pdf.is_none() && wisp_core::openaccess::lookable(p) { items.push(item("查找开放全文", Cmd::FindPdfs(Some(id.clone())))) }

                breaks.push(items.len());
                items.push(item("复制题目", Cmd::CopyText(p.title.clone())));
                if !p.doi.is_empty() { items.push(item("复制 DOI", Cmd::CopyText(p.doi.clone()))) }
                items.push(item("复制 BibTeX", Cmd::CopyText(wisp_core::export::bibtex(&[p]))));
                if !p.link().is_empty() { items.push(item("打开网页", Cmd::OpenLink(id.clone()))) }
                if let Some(file) = pdf { items.push(item(reveal_label(), Cmd::RevealFile(file.id.clone()))) }

                breaks.push(items.len());
                items.push(MenuItem { label: "移到回收站".into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-backspace").into()), cmd: Cmd::TrashPaper(id.clone()) });
            }
            MenuKind::Trashed(id) => {
                items.push(item("恢复", Cmd::RestorePaper(id.clone())));
                items.push(item("彻底删除…", Cmd::AskDeletePaper(id.clone())));
            }
            MenuKind::File(id) => {
                let Some(file) = lib.files.iter().find(|f| f.id == *id) else { return (items, breaks) };
                items.push(item("用系统应用打开", Cmd::OpenFile(id.clone())));
                items.push(item(reveal_label(), Cmd::RevealFile(id.clone())));
                items.push(item("复制文件名", Cmd::CopyText(file.title.clone())));
                if file.kind == "fulltext" { items.push(item("重新读取文字", Cmd::RereadFile(id.clone()))) }
                breaks.push(items.len());
                items.push(item("移除附件", Cmd::RemoveFile(id.clone())));
            }
            MenuKind::Tag(tag) => {
                let on = lib.page == Page::Papers && lib.filters.tag == *tag;
                items.push(item(if on { "不再只看这个标签" } else { "只看这个标签" }, Cmd::SetFilter(crate::app::FilterKey::Tag, if on { String::new() } else { tag.clone() })));
                items.push(item("重命名…", Cmd::RenameTag(tag.clone())));
                items.push(item("复制标签名", Cmd::CopyText(tag.clone())));
                breaks.push(items.len());
                items.push(item("去掉这个标签…", Cmd::AskRemoveTag(tag.clone())));
            }
            MenuKind::Conversation(paper, id) => {
                items.push(item("接着问下去", Cmd::OpenChat(*paper, id.clone())));
                items.push(item("删除这次对话…", Cmd::AskRemoveChat(*paper, id.clone())));
            }
            MenuKind::Text(text) => {
                if self.picked.as_ref().is_some_and(|picked| picked.text.as_ref() == text.as_str() && !picked.chosen().is_empty()) { items.push(MenuItem { label: "复制".into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-c").into()), cmd: Cmd::CopySelected }) }
                items.push(item("复制全部", Cmd::CopyText(text.clone())));
            }
            MenuKind::Answer(paper, text) => {
                if lib.chat(*paper).picked_text().is_some_and(|picked| !picked.is_empty()) {
                    items.push(MenuItem { label: "复制所选".into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-c").into()), cmd: Cmd::CopyPicked(*paper) });
                    items.push(item("引用到输入框", Cmd::QuotePicked(*paper)));
                    breaks.push(items.len());
                }
                items.push(item("复制整段回答", Cmd::CopyText(text.clone())));
            }
            _ => {}
        }
        (items, breaks)
    }

    /// An arrow key, Enter or Delete that no text field took.
    pub fn nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        // An open menu has the keys: up and down along its items, Enter takes one.
        if let Some(menu) = self.menu.as_mut() {
            let count = menu.items.len();
            match nav {
                Nav::Down => menu.active = Some(menu.active.map_or(0, |at| (at + 1) % count)),
                Nav::Up => menu.active = Some(menu.active.map_or(count - 1, |at| (at + count - 1) % count)),
                Nav::First => menu.active = Some(0),
                Nav::Last => menu.active = Some(count - 1),
                Nav::Open => {
                    if let Some(cmd) = menu.active.and_then(|at| menu.items.get(at)).map(|item| item.cmd.clone()) { self.run(cmd, window, cx) }
                }
                _ => {}
            }
            return cx.notify();
        }
        if self.modal.is_some() || self.settings_view.is_some() { return }
        let Some(lib) = self.lib.as_ref() else { return };
        // A PDF on show: left and right turn its pages.
        let detail = lib.selected.is_some() && !matches!(lib.page, Page::Trash | Page::Sources | Page::Guide);
        if matches!(nav, Nav::Left | Nav::Right) && detail && (lib.tab == Tab::Pdf || (lib.tab == Tab::Ask && lib.split && self.detail_width(window) >= crate::detail::SPLIT_MIN)) && let Some(pdf) = lib.pdf.clone() {
            return pdf.update(cx, |pdf, cx| {
                let page = pdf.current_page();
                pdf.go_to(if nav == Nav::Left { page.saturating_sub(1).max(1) } else { page + 1 }, cx);
            });
        }
        // The papers in the order the page shows them.
        let ids: Vec<String> = match lib.page {
            Page::Papers => lib.shown.iter().map(|&i| lib.papers[i].id.clone()).collect(),
            Page::Screen => lib.screen_view().0.iter().map(|p| p.id.clone()).collect(),
            _ => return,
        };
        let listed = lib.page == Page::Papers;
        let at = lib.selected.as_deref().and_then(|id| ids.iter().position(|other| other == id));
        match nav {
            Nav::Open => {
                // As a double click does: the paper's PDF, when it has one.
                if let Some(id) = at.map(|at| ids[at].clone()).filter(|id| lib.pdf_file(id).is_some()) { self.run(Cmd::OpenPaper(id, Tab::Pdf), window, cx) }
                return;
            }
            Nav::Trash => {
                if listed && let Some(at) = at { self.run(Cmd::TrashPaper(ids[at].clone()), window, cx) }
                return;
            }
            _ => {}
        }
        if ids.is_empty() { return }
        let last = ids.len() - 1;
        // A screenful of rows, less one so that the eye keeps its place.
        let row = if self.settings.density == "compact" { 44. } else { 56. };
        let screenful = (((f32::from(window.viewport_size().height) - 190.) / row).floor() as usize).max(2) - 1;
        let to = match (nav, at) {
            (Nav::First, _) => 0,
            (Nav::Last, _) => last,
            (Nav::Up, None) => last,
            (_, None) => 0,
            (Nav::Up, Some(at)) => at.saturating_sub(1),
            (Nav::Down, Some(at)) => (at + 1).min(last),
            (Nav::PageUp | Nav::Left, Some(at)) => at.saturating_sub(screenful),
            (_, Some(at)) => (at + screenful).min(last),
        };
        if Some(to) == at { return }
        // Only as far as brings the row into view: at the bottom going down, at the top going up.
        if listed { lib.table_scroll.scroll_to_item(to, if at.is_some_and(|at| to > at) { ScrollStrategy::Bottom } else { ScrollStrategy::Top }) }
        self.select(Some(ids[to].clone()), window, cx);
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { "在访达中显示" } else { "在文件夹中显示" }
}
