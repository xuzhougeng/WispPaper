//! 写作's citations: 引用 puts a paper's key in at the caret (`[@Zhang2024]`,
//! `wisp_core::cite`), and under the manuscript the reference list of the
//! part being written or of the whole manuscript, each entry leading to its
//! paper, with what does not resolve pointed out. Any paper of the library
//! can be cited by typing part of its author or title; the list can be
//! copied and the cited papers saved as BibTeX under the same keys.

use crate::i18n::t;
use crate::app::{Cmd, Library, Workspace};
use crate::input::{InputEvent, TextInput};
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::BTreeMap;
use wisp_core::cite::{self, Cited, Found, Index};
use wisp_core::passage::{self, Candidate};
use wisp_core::store::Store;

/// Papers offered while a part of one is typed.
const OFFERED: usize = 5;

/// What the citations of the writing page keep.
pub struct Citing {
    /// Part of an author or a title, to cite a paper that is not among the candidates.
    pub find: Entity<TextInput>,
    /// The keys papers were cited by (`Store::cite_keys`).
    pub kept: BTreeMap<String, String>,
    /// The reference list of the whole manuscript, not only of the part being written.
    pub whole: bool,
    /// Where in its paper each citation is from (write_source.rs).
    pub sources: crate::write_source::Sourcing,
}

impl Citing {
    /// `manuscript` is the one being written: its citations' passages are read.
    pub fn new(store: &Store, manuscript: &str, subscriptions: &mut Vec<Subscription>, cx: &mut Context<Workspace>) -> Citing {
        let find = cx.new(|cx| TextInput::new("引用库里的其他文献：输入作者、题目或年份", cx));
        subscriptions.push(cx.observe(&find, |_, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe(&find, |this, input, event: &InputEvent, cx| match event {
            // Enter cites the first paper offered.
            InputEvent::Submit => {
                let query = input.read(cx).text().to_string();
                let first = this.lib.as_ref().and_then(|lib| index(lib).matching(&query, 1).first().map(|paper| paper.id.clone()));
                if let Some(id) = first { this.cite_paper(&id, None, cx) }
            }
            InputEvent::Cancel => input.update(cx, |input, cx| input.set_text("", cx)),
            _ => {}
        }));
        Citing { find, kept: store.cite_keys().unwrap_or_default(), whole: false, sources: crate::write_source::Sourcing::new(store.passages(manuscript).unwrap_or_default()) }
    }
}

/// Every paper of the open library, the trash's included, with its key.
pub(crate) fn index(lib: &Library) -> Index<'_> {
    Index::new(&lib.papers, lib.trash.iter().map(|entry| &entry.paper), &lib.citing.kept)
}

impl Workspace {
    /// 引用: the paper's key at the caret, kept so it stays the paper's.
    /// `from` is the sentence of the paper it is cited from (a candidate's): kept as the passage the citation rests on.
    /// A sentence that cites the paper already is not made to cite it twice: the passage is kept for the citation there.
    pub fn cite_paper(&mut self, id: &str, from: Option<Candidate>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(key) = index(lib).key(id).map(str::to_string) else { return self.fail(t("文献库里没有这篇文献。"), cx) };
        if !lib.citing.kept.contains_key(&key) {
            if let Err(err) = lib.store.keep_cite_key(id, &key) { return self.fail(tf!("引用没有记下：{err:#}", "Citation not saved: {err:#}"), cx) }
            lib.citing.kept.insert(key.clone(), id.to_string());
        }
        let (text, caret) = { let editor = lib.writing.editor.read(cx); (editor.text().to_string(), editor.caret()) };
        let cited_here = from.as_ref().and_then(|_| wisp_core::citefind::sentences(&text).into_iter().find(|range| range.start <= caret && caret <= range.end)).filter(|range| wisp_core::citefind::cites(&text[range.clone()], &key));
        if cited_here.is_none() {
            lib.writing.editor.update(cx, |input, cx| {
                let before = input.text()[..input.caret()].chars().last();
                // After a word a space; after Chinese, an opening bracket or a space, none.
                let space = if before.is_some_and(|c| c.is_ascii() && !c.is_whitespace() && !"([（".contains(c)) { " " } else { "" };
                input.insert(&format!("{space}{}", cite::written(&key)), cx);
            });
        }
        lib.citing.find.update(cx, |input, cx| input.set_text("", cx));
        if let Some(from) = from {
            // What the sentence that now cites the paper says: the citation just written ends before the caret, one that stood there is in the caret's sentence.
            let (text, caret) = { let editor = lib.writing.editor.read(cx); (editor.text().to_string(), editor.caret()) };
            let claim = {
                let index = index(lib);
                let citations = passage::citations(&text, &cite::find(&text, &index));
                citations.iter().find(|one| one.paper == id && if cited_here.is_some() { one.sentence.start <= caret && caret <= one.sentence.end } else { one.at.end + 1 == caret }).map(|one| one.claim.clone())
            };
            if let Some(claim) = claim {
                let section = lib.writing.section_id.clone();
                self.sources_keep(vec![(section, id.to_string(), claim, from, String::new(), passage::CITED.to_string())], cx);
                if cited_here.is_some() { self.say(t("这句话已经引用了这篇文献：已把这句原文记为它的出处。"), cx) }
            }
        }
        cx.notify();
    }

    /// Replace pieces of the part being written: (start, end, what is there, what replaces it), last first.
    /// A piece that no longer holds what it held is left alone.
    pub fn cite_fix(&mut self, pieces: Vec<(usize, usize, String, String)>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        // The keys written in are kept, as 引用 keeps them.
        let written: Vec<(String, String)> = {
            let index = index(lib);
            pieces.iter().flat_map(|(_, _, _, to)| cite::find(to, &index)).filter_map(|one| one.found.paper().and_then(|id| Some((index.key(id)?.to_string(), id.to_string())))).collect()
        };
        for (key, id) in written {
            if lib.citing.kept.contains_key(&key) { continue }
            if let Err(err) = lib.store.keep_cite_key(&id, &key) { return self.fail(tf!("引用没有记下：{err:#}", "Citation not saved: {err:#}"), cx) }
            lib.citing.kept.insert(key, id);
        }
        lib.writing.editor.update(cx, |input, cx| {
            for (start, end, was, to) in pieces {
                if input.text().get(start..end) == Some(was.as_str()) { input.replace_text(start..end, &to, cx) }
            }
        });
    }

    /// The texts the reference list is made of: the part being written, or every part in order.
    pub(crate) fn cited_texts(&self, cx: &App) -> Vec<(String, String)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let section = lib.writing.section_id.clone();
        let current = lib.writing.editor.read(cx).text().to_string();
        if !lib.citing.whole { return vec![(section, current)] }
        // Every section of the manuscript in its order; the one being written as the editor has it.
        lib.writing.current().map(|m| m.sections.iter().map(|s| (s.id.clone(), if s.id == section { current.clone() } else { s.body.clone() })).collect()).unwrap_or_default()
    }

    fn cited_references(&self, cx: &App) -> Option<(Vec<cite::Reference>, String)> {
        let lib = self.lib.as_ref()?;
        let index = index(lib);
        let cited: Vec<Cited> = self.cited_texts(cx).iter().flat_map(|(_, text)| cite::find(text, &index)).collect();
        let references = cite::references(&cited, &index);
        let bib = cite::bibtex(&references, &index);
        Some((references, bib))
    }

    pub fn copy_references(&mut self, cx: &mut Context<Self>) {
        let Some((references, _)) = self.cited_references(cx) else { return };
        if references.is_empty() { return self.fail(t("还没有引用文献。"), cx) }
        cx.write_to_clipboard(ClipboardItem::new_string(cite::reference_list(&references)));
        self.say(tf!("已复制 {} 条参考文献", "Copied {} references", references.len()), cx);
    }

    /// The cited papers as BibTeX, under the keys the manuscript cites them by.
    pub fn export_cited(&mut self, cx: &mut Context<Self>) {
        let Some((references, text)) = self.cited_references(cx) else { return };
        if references.is_empty() { return self.fail(t("还没有引用文献。"), cx) }
        let Some(lib) = self.lib.as_ref() else { return };
        let name = tf!("{}-引用.bib", "{}-references.bib", lib.info.name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-"));
        let directory = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        let count = references.len();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = picked.await {
                if path.extension().is_none() { path.set_extension("bib"); }
                let result = std::fs::write(&path, text);
                this.update(cx, |this, cx| match result {
                    Ok(()) => this.say(tf!("已导出 {count} 篇引用文献到 {}", "Exported {count} cited papers to {}", path.display()), cx),
                    Err(err) => this.fail(tf!("无法写入 {}：{err}", "Cannot write {}: {err}", path.display()), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    /// Under the manuscript: the reference list, what does not resolve, and citing any paper of the library.
    pub fn render_references(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let index = index(lib);
        let section = lib.writing.section_id.as_str();
        let section_name = |id: &str| lib.writing.current().and_then(|m| m.section(id)).map(|s| s.name.clone()).unwrap_or_default();
        let texts = self.cited_texts(cx);
        let found: Vec<(&str, &str, Vec<Cited>)> = texts.iter().map(|(key, text)| (key.as_str(), text.as_str(), cite::find(text, &index))).collect();
        let all: Vec<Cited> = found.iter().flat_map(|(_, _, cited)| cited.iter().cloned()).collect();
        let references = cite::references(&all, &index);
        // Where each citation is from: the passage kept for it, and the citation the caret is in.
        let standing = crate::write_source::standing(&found, &lib.citing.sources.kept);
        let at = self.source_at(&standing, cx);
        self.source_follow(at, at.and_then(|one| references.iter().position(|reference| reference.paper_id == one.citation.paper)));
        let source_actions = self.render_source_actions(&standing, cx);
        let key_of = |id: &str| index.key(id).unwrap_or("").to_string();
        let title_of = |id: &str| index.paper(id).map(|paper| paper.title.clone()).unwrap_or_default();

        // What does not resolve, where it is written; in the part being written it can be put right.
        let mut issues: Vec<AnyElement> = vec![];
        for (part, _, cited) in &found {
            let here = *part == section;
            let place = if lib.citing.whole { format!("{}：", section_name(part)) } else { String::new() };
            for (n, one) in cited.iter().enumerate() {
                let shown = if one.old { format!("（{}）", one.written) } else { format!("[{}]", one.written) };
                let what = match &one.found {
                    Found::Paper(_) => continue,
                    Found::Trashed(_) => t("这篇文献在回收站里"),
                    Found::Removed => t("这篇文献已从库里删除"),
                    Found::Unknown if one.old => t("库里没有对得上的文献"),
                    Found::Unknown => t("库里没有这个引用键"),
                    Found::Ambiguous(_) => t("对得上不止一篇，选一篇："),
                };
                let mut line = div().flex().items_center().flex_wrap().gap(px(4.)).text_size(px(12.)).child(div().text_color(if matches!(one.found, Found::Trashed(_)) { Theme::warning() } else { Theme::danger() }).child(format!("{place}{shown} {what}")));
                if let (Found::Ambiguous(ids), true) = (&one.found, here) {
                    // Only a parenthesis citing this one paper is replaced whole.
                    let alone = cited.iter().filter(|other| other.group == one.group).count() == 1;
                    for (m, id) in ids.iter().enumerate() {
                        let text = format!("@{} {}", key_of(id), title_of(id));
                        let button = ui::ghost_button(SharedString::from(format!("cite-pick-{part}-{n}-{m}")), None, Some(text.into())).h(px(22.)).px(px(6.)).max_w(px(260.)).truncate().text_size(px(12.));
                        let fix = (one.group.start, one.group.end, texts.iter().find(|(key, _)| key == part).map(|(_, text)| text[one.group.clone()].to_string()).unwrap_or_default(), cite::written(&key_of(id)));
                        line = line.child(if alone { button.on_click(self.on(Cmd::WriteCiteFix(vec![fix]), cx)) } else { button.on_click(self.on(Cmd::GotoSource(id.clone(), None, String::new()), cx)) });
                    }
                }
                issues.push(line.into_any_element());
            }
        }
        // Old-style citations that name one paper each can become keys.
        let (_, current, cited_here) = found.iter().find(|(part, _, _)| *part == section).cloned().unwrap_or(("", "", vec![]));
        let upgrades: Vec<(usize, usize, String, String)> = cite::upgrades(&cited_here, &index).into_iter().map(|(range, to)| (range.start, range.end, current[range].to_string(), to)).collect();

        let query = lib.citing.find.read(cx).text().trim().to_string();
        let offered: Vec<AnyElement> = index
            .matching(&query, OFFERED)
            .into_iter()
            .map(|paper| {
                let about = [paper.short_authors(), paper.year.map(|y| y.to_string()).unwrap_or_default()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
                div()
                    .id(SharedString::from(format!("cite-offer-{}", paper.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .text_size(px(12.))
                    .child(div().flex_none().text_color(Theme::accent()).child(format!("@{}", key_of(&paper.id))))
                    .child(div().flex_1().min_w_0().truncate().text_color(Theme::text()).child(paper.title.clone()))
                    .child(div().flex_none().text_color(Theme::text_tertiary()).child(about))
                    .on_click(self.on(Cmd::WriteCite(paper.id.clone()), cx))
                    .into_any_element()
            })
            .collect();

        let rows: Vec<AnyElement> = references
            .iter()
            .map(|reference| {
                let own: Vec<&crate::write_source::Standing> = standing.iter().filter(|one| one.citation.paper == reference.paper_id).collect();
                let lit = at.is_some_and(|one| one.citation.paper == reference.paper_id);
                // The entry leads to its paper; the passages under it, each to where it stands in the paper.
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(2.))
                    .pb(px(2.))
                    .rounded(px(6.))
                    .child(
                        div()
                            .id(SharedString::from(format!("cite-ref-{}", reference.paper_id)))
                            .flex()
                            .items_start()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::hover()))
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .child(div().flex_1().min_w_0().text_color(if reference.trashed { Theme::text_tertiary() } else { Theme::text() }).child(reference.text.clone()))
                            .child(div().flex_none().text_size(px(11.)).text_color(if lit { Theme::accent() } else { Theme::text_tertiary() }).child(format!("@{}", reference.key)))
                            .on_click(self.on(Cmd::GotoSource(reference.paper_id.clone(), None, String::new()), cx)),
                    )
                    .child(div().flex().flex_col().gap(px(3.)).pl(px(8.)).pr(px(4.)).children(self.render_passages(&own, at, cx)))
                    .into_any_element()
            })
            .collect();

        let mut scope = |id: &'static str, name: &'static str, whole: bool| {
            let on = lib.citing.whole == whole;
            div().id(id).flex().items_center().h(px(22.)).px(px(8.)).rounded(px(11.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent())).when(!on, |d| d.text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(self.on(Cmd::WriteCiteScope(whole), cx)).child(name)
        };
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(px(22.)).px(px(6.)).text_size(px(12.));
        let count = if references.is_empty() { t("还没有引用").to_string() } else { tf!("{} 篇", "{} papers", references.len()) };

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .mx(px(20.))
            .mb(px(8.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(div().flex_none().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(t("参考文献")))
                    .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(count))
                    .child(ui::hint("cite-about", t("在右边的句子上点「引用」，或在这里找一篇文献。引用写作 [@Zhang2024]，导出的 Markdown 和 BibTeX 可以直接交给 Pandoc。每篇文献下面是出处：引用它的那句话依据的文献原文，点一下在 PDF 里看；光标停在一处引用上，它的出处会标出来。")))
                    .children(source_actions)
                    .child(div().w(px(2.)))
                    .child(scope("cite-scope-part", t("本节"), false))
                    .child(scope("cite-scope-whole", t("全文"), true))
                    // Finding a paper to cite is part of the header: no line of its own while nothing is cited.
                    .child(div().flex_1().min_w(px(180.)).child(ui::field(&lib.citing.find, window, cx)))
                    .when(!upgrades.is_empty(), |d| d.child(small("cite-upgrade", t("旧式引用改为引用键")).on_click(self.on(Cmd::WriteCiteFix(upgrades.clone()), cx))))
                    .when(!references.is_empty(), |d| d.child(small("cite-copy", t("复制")).on_click(self.on(Cmd::WriteCopyReferences, cx))).child(small("cite-bib", t("导出 BibTeX")).on_click(self.on(Cmd::WriteExportCited, cx)))),
            )
            .children(issues)
            .when(!rows.is_empty(), |d| d.child(div().id("cite-references").flex().flex_col().max_h(px(220.)).overflow_y_scroll().track_scroll(&lib.citing.sources.scroll).children(rows)))
            .when(!query.is_empty(), |d| d.child(if offered.is_empty() { div().px(px(8.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("没有对得上的文献")).into_any_element() } else { div().flex().flex_col().children(offered).into_any_element() }))
    }
}
