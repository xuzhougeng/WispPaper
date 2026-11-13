//! What the hands expect of a list: the arrow keys move in the papers on
//! show (and in an open menu), Enter opens, Delete moves to the trash; and a
//! right click opens a menu of what can be done with the thing under it.

use crate::i18n::t;
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
    /// Shift with an arrow: the marked stretch reaches a row further. ⌘A / Ctrl+A: every paper on show.
    MarkUp,
    MarkDown,
    MarkAll,
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

    /// A right click on a row of the list: on one of several marked papers, the menu for all of them;
    /// on any other, that paper's own (and the marking ends).
    pub fn on_paper_context(&self, id: String, cx: &mut Context<Self>) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let Some(lib) = this.lib.as_mut() else { return };
            let kind = if lib.marked.len() > 1 && lib.marked.contains(&id) {
                // In the order of the list, whatever order they were marked in.
                MenuKind::Papers(lib.shown.iter().map(|&i| lib.papers[i].id.clone()).filter(|id| lib.marked.contains(id)).collect())
            } else {
                if !lib.marked.contains(&id) { lib.marked.clear() }
                MenuKind::Paper(id.clone())
            };
            this.open_menu(kind, event.position, cx);
        })
    }

    /// A click on a row with a key held. ⌘ / Ctrl: the paper joins the marked ones or leaves them
    /// (the paper on show is the first of them). Shift: every paper from where the marking started to this one.
    pub fn mark(&mut self, id: &str, stretch: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let ids: Vec<String> = lib.shown.iter().map(|&i| lib.papers[i].id.clone()).collect();
        let place = |id: &str| ids.iter().position(|other| other == id);
        if stretch {
            let from = lib.anchor.clone().or(lib.selected.clone()).filter(|id| place(id).is_some()).unwrap_or_else(|| id.to_string());
            let (Some(a), Some(b)) = (place(&from), place(id)) else { return };
            lib.marked = ids[a.min(b)..=a.max(b)].to_vec();
            (lib.anchor, lib.cursor) = (Some(from), Some(id.to_string()));
        } else {
            if lib.marked.is_empty() && let Some(shown) = lib.selected.clone().filter(|shown| shown != id) { lib.marked.push(shown) }
            match lib.marked.iter().position(|other| other == id) {
                Some(at) => { lib.marked.remove(at); }
                None => lib.marked.push(id.to_string()),
            }
            (lib.anchor, lib.cursor) = (Some(id.to_string()), None);
        }
        // Several papers have no panel of one; the keyboard stays with the list.
        if lib.marked.len() > 1 { self.select(None, window, cx) }
        window.focus(&self.focus);
        cx.notify();
    }

    /// What is done with several papers at once.
    pub(crate) fn run_batch(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        // Each paper read, changed and written; the list is read again once.
        let each = |this: &mut Self, ids: &[String], cx: &mut Context<Self>, edit: &dyn Fn(&mut wisp_core::model::Paper)| {
            this.change(cx, |lib| {
                for id in ids {
                    let mut paper = lib.store.get_paper(id)?;
                    edit(&mut paper);
                    lib.store.save_paper(&mut paper)?;
                }
                Ok(())
            })
        };
        match cmd {
            Cmd::AskPapers(ids) => {
                // A conversation of its own: what was being asked before is kept in the history.
                self.run(Cmd::NewChat(false), window, cx);
                if let Some(lib) = self.lib.as_mut() { lib.asked = ids }
                self.run(Cmd::SetPage(Page::Ask), window, cx);
            }
            Cmd::StarPapers(ids, starred) => {
                each(self, &ids, cx, &|paper| paper.starred = starred);
            }
            Cmd::ReadPapers(ids, value) => {
                each(self, &ids, cx, &|paper| paper.reading = value.clone());
            }
            Cmd::TagPapers(ids) => {
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                let name = self.modal_input("例如 综述、方法", "", false, window, cx);
                name.read(cx).focus(window);
                self.modal = Some(crate::app::Modal::TagPapers { ids, name });
            }
            Cmd::FindPdfsOf(ids) => {
                for id in ids { self.fetch_start(Some(id), cx) }
            }
            Cmd::AskTrashPapers(ids) => {
                self.modal_error.clear();
                self.modal = Some(crate::app::Modal::Confirm { title: tf!("把 {} 篇文献移到回收站？", "Move {} papers to trash?", ids.len()), message: t("它们的笔记、词条和 PDF 都留着，可以在回收站里恢复。").into(), action: t("移到回收站").into(), cmd: Cmd::TrashPapers(ids) });
            }
            Cmd::TrashPapers(ids) => {
                self.modal = None;
                let done = self.change(cx, |lib| {
                    for id in &ids { lib.store.trash_paper(id)? }
                    Ok(())
                });
                if done { self.say(tf!("已把 {} 篇文献移到回收站。", "Moved {} papers to trash.", ids.len()), cx) }
                window.focus(&self.focus);
            }
            _ => {}
        }
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
                if pdf.is_some() { items.push(item(t("打开 PDF"), Cmd::OpenPaper(id.clone(), Tab::Pdf))) }
                items.push(item(t("查看信息"), Cmd::OpenPaper(id.clone(), Tab::Info)));
                items.push(item(t("就这篇提问"), Cmd::OpenPaper(id.clone(), Tab::Ask)));

                breaks.push(items.len());
                items.push(item(if p.starred { t("取消收藏") } else { t("收藏") }, Cmd::ToggleStar(id.clone())));
                for (value, label) in READINGS {
                    let label = t(label);
                    items.push(MenuItem { label: tf!("标为{label}", "Mark as {label}").into(), checked: p.reading == value, note: None, cmd: Cmd::SetReading(id.clone(), value.into()) });
                }

                breaks.push(items.len());
                items.push(item(t("编辑题目、信息与词条…"), Cmd::EditPaper(id.clone())));
                if p.doi.is_empty() && p.pmid.is_empty() { items.push(item(t("按题目补全…"), Cmd::CompletePaper(id.clone()))) }
                items.push(item(if pdf.is_some() { t("再添加一个文件…") } else { t("添加 PDF 或附件…") }, Cmd::AttachPdf(id.clone())));
                if pdf.is_none() && wisp_core::openaccess::lookable(p) { items.push(item(t("查找开放全文"), Cmd::FindPdfs(Some(id.clone())))) }

                breaks.push(items.len());
                items.push(item(t("复制题目"), Cmd::CopyText(p.title.clone())));
                if !p.doi.is_empty() { items.push(item(t("复制 DOI"), Cmd::CopyText(p.doi.clone()))) }
                items.push(item(t("复制 BibTeX"), Cmd::CopyText(wisp_core::export::bibtex(&[p]))));
                if !p.link().is_empty() { items.push(item(t("打开网页"), Cmd::OpenLink(id.clone()))) }
                if let Some(file) = pdf { items.push(item(reveal_label(), Cmd::RevealFile(file.id.clone()))) }

                breaks.push(items.len());
                items.push(MenuItem { label: t("移到回收站").into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-backspace").into()), cmd: Cmd::TrashPaper(id.clone()) });
            }
            MenuKind::Papers(ids) => {
                let papers: Vec<&wisp_core::model::Paper> = ids.iter().filter_map(|id| lib.paper(id)).collect();
                if papers.is_empty() { return (items, breaks) }
                let count = papers.len();
                items.push(item(&tf!("就这 {count} 篇提问", "Ask about these {count} papers"), Cmd::AskPapers(ids.clone())));

                breaks.push(items.len());
                let starred = papers.iter().all(|p| p.starred);
                items.push(item(if starred { t("取消收藏") } else { t("收藏") }, Cmd::StarPapers(ids.clone(), !starred)));
                for (value, label) in READINGS {
                    let label = t(label);
                    items.push(MenuItem { label: tf!("标为{label}", "Mark as {label}").into(), checked: papers.iter().all(|p| p.reading == value), note: None, cmd: Cmd::ReadPapers(ids.clone(), value.into()) });
                }
                items.push(item(t("添加标签…"), Cmd::TagPapers(ids.clone())));
                // Those without a PDF that have a number to be looked up by.
                let lacking: Vec<String> = papers.iter().filter(|p| lib.pdf_file(&p.id).is_none() && wisp_core::openaccess::lookable(p)).map(|p| p.id.clone()).collect();
                if !lacking.is_empty() { items.push(MenuItem { label: t("查找开放全文").into(), checked: false, note: Some(tf!("{} 篇缺 PDF", "{} missing PDFs", lacking.len()).into()), cmd: Cmd::FindPdfsOf(lacking) }) }

                breaks.push(items.len());
                items.push(item(t("批量翻译标题"), Cmd::TranslatePapers(ids.clone(), wisp_core::translation::Field::Title)));
                items.push(item(t("批量翻译摘要"), Cmd::TranslatePapers(ids.clone(), wisp_core::translation::Field::Abstract)));

                breaks.push(items.len());
                items.push(item(t("复制题目"), Cmd::CopyText(papers.iter().map(|p| p.title.as_str()).collect::<Vec<_>>().join("\n"))));
                items.push(item(t("复制 BibTeX"), Cmd::CopyText(wisp_core::export::bibtex(&papers))));

                breaks.push(items.len());
                items.push(MenuItem { label: tf!("把 {count} 篇移到回收站…", "Move {count} papers to trash…").into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-backspace").into()), cmd: Cmd::AskTrashPapers(ids.clone()) });
            }
            MenuKind::Trashed(id) => {
                items.push(item(t("恢复"), Cmd::RestorePaper(id.clone())));
                items.push(item(t("彻底删除…"), Cmd::AskDeletePaper(id.clone())));
            }
            MenuKind::File(id) => {
                let Some(file) = lib.files.iter().find(|f| f.id == *id) else { return (items, breaks) };
                items.push(item(t("用系统应用打开"), Cmd::OpenFile(id.clone())));
                items.push(item(reveal_label(), Cmd::RevealFile(id.clone())));
                items.push(item(t("复制文件名"), Cmd::CopyText(file.title.clone())));
                if file.kind == "fulltext" { items.push(item(t("重新读取文字"), Cmd::RereadFile(id.clone()))) }
                breaks.push(items.len());
                items.push(item(t("移除附件"), Cmd::RemoveFile(id.clone())));
            }
            MenuKind::Tag(tag) => {
                let on = lib.page == Page::Papers && lib.filters.tag == *tag;
                items.push(item(if on { t("不再只看这个标签") } else { t("只看这个标签") }, Cmd::SetFilter(crate::app::FilterKey::Tag, if on { String::new() } else { tag.clone() })));
                items.push(item(t("重命名…"), Cmd::RenameTag(tag.clone())));
                items.push(item(t("复制标签名"), Cmd::CopyText(tag.clone())));
                breaks.push(items.len());
                items.push(item(t("去掉这个标签…"), Cmd::AskRemoveTag(tag.clone())));
            }
            MenuKind::Conversation(paper, id) => {
                items.push(item(t("接着问下去"), Cmd::OpenChat(*paper, id.clone())));
                items.push(item(t("删除这次对话…"), Cmd::AskRemoveChat(*paper, id.clone())));
            }
            MenuKind::Text(text) => {
                if self.picked.as_ref().is_some_and(|picked| picked.text.as_ref() == text.as_str() && !picked.chosen().is_empty()) { items.push(MenuItem { label: t("复制").into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-c").into()), cmd: Cmd::CopySelected }) }
                items.push(item(t("复制全部"), Cmd::CopyText(text.clone())));
            }
            MenuKind::Answer(paper, text) => {
                if lib.chat(*paper).picked_text().is_some_and(|picked| !picked.is_empty()) {
                    items.push(MenuItem { label: t("复制所选").into(), checked: false, note: Some(crate::shortcuts::pretty("secondary-c").into()), cmd: Cmd::CopyPicked(*paper) });
                    items.push(item(t("引用到输入框"), Cmd::QuotePicked(*paper)));
                    breaks.push(items.len());
                }
                items.push(item(t("复制整段回答"), Cmd::CopyText(text.clone())));
                // `/review` starts a review's record by itself; a conversation that began otherwise can be made one here.
                if !*paper && !lib.library_chat.id.is_empty() && lib.library_chat.review.is_none() && !lib.library_chat.busy {
                    breaks.push(items.len());
                    items.push(item(t("把这次对话作为综述来写"), Cmd::StartReview));
                }
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
        // 逐篇 and 确认 have their own places to move between.
        if lib.page == Page::Screen && lib.screen.mode != crate::app::ScreenMode::Batch { return self.screen_nav(nav, window, cx) }
        // The papers in the order the page shows them.
        let ids: Vec<String> = match lib.page {
            Page::Papers => lib.shown.iter().map(|&i| lib.papers[i].id.clone()).collect(),
            Page::Screen => lib.screen_view().0.iter().map(|p| p.id.clone()).collect(),
            _ => return,
        };
        let listed = lib.page == Page::Papers;
        let place = |id: &Option<String>| id.as_deref().and_then(|id| ids.iter().position(|other| other == id));
        // Marking with the keyboard, in the list of papers.
        if matches!(nav, Nav::MarkUp | Nav::MarkDown | Nav::MarkAll) {
            if !listed || ids.is_empty() { return }
            let from = place(&lib.anchor).or(place(&lib.selected)).unwrap_or(0);
            let (from, to) = match nav {
                Nav::MarkAll => (0, ids.len() - 1),
                Nav::MarkUp => (from, place(&lib.cursor).unwrap_or(from).saturating_sub(1)),
                _ => (from, (place(&lib.cursor).unwrap_or(from) + 1).min(ids.len() - 1)),
            };
            if nav != Nav::MarkAll { lib.table_scroll.scroll_to_item(to, if nav == Nav::MarkDown { ScrollStrategy::Bottom } else { ScrollStrategy::Top }) }
            let marked = ids[from.min(to)..=from.max(to)].to_vec();
            let (anchor, cursor) = (ids[from].clone(), ids[to].clone());
            if marked.len() > 1 { self.select(None, window, cx) }
            if let Some(lib) = self.lib.as_mut() { (lib.marked, lib.anchor, lib.cursor) = (marked, Some(anchor), Some(cursor)) }
            return cx.notify();
        }
        // Several marked: Delete is for all of them, after asking.
        if nav == Nav::Trash && listed && lib.marked.len() > 1 {
            let marked = ids.iter().filter(|id| lib.marked.contains(id)).cloned().collect();
            return self.run(Cmd::AskTrashPapers(marked), window, cx);
        }
        // Where the keys go on from: the paper on show, else where the marking reached.
        let at = place(&lib.selected).or(place(&lib.cursor)).or(place(&lib.anchor));
        match nav {
            Nav::Open => {
                // As a double click does: the paper's PDF, when it has one.
                if let Some(id) = place(&lib.selected).map(|at| ids[at].clone()).filter(|id| lib.pdf_file(id).is_some()) { self.run(Cmd::OpenPaper(id, Tab::Pdf), window, cx) }
                return;
            }
            Nav::Trash => {
                if listed && let Some(at) = place(&lib.selected) { self.run(Cmd::TrashPaper(ids[at].clone()), window, cx) }
                return;
            }
            _ => {}
        }
        if ids.is_empty() { return }
        let last = ids.len() - 1;
        // A screenful of rows, less one so that the eye keeps its place.
        let row = self.row_height(self.asking(window));
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
        if Some(to) == at && lib.selected.is_some() { return }
        // Only as far as brings the row into view: at the bottom going down, at the top going up.
        if listed { lib.table_scroll.scroll_to_item(to, if at.is_some_and(|at| to > at) { ScrollStrategy::Bottom } else { ScrollStrategy::Top }) }
        self.select(Some(ids[to].clone()), window, cx);
        if let Some(lib) = self.lib.as_mut() { (lib.anchor, lib.cursor) = (Some(ids[to].clone()), None) }
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { t("在访达中显示") } else { t("在文件夹中显示") }
}
