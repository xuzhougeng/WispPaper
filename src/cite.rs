//! 写作's citations: 引用 puts a paper's key in at the caret (`[@Zhang2024]`,
//! `wisp_core::cite`). The reference list is a page of the panel beside the
//! manuscript, after the candidates and the comments: of the part being
//! written or of the whole manuscript, each entry leading to its paper, with
//! what does not resolve pointed out. Any paper of the library can be cited
//! there by typing part of its author or title; the list can be copied and
//! the cited papers saved as BibTeX under the same keys. The outline's last
//! row opens the whole manuscript's list, and a manuscript read through ends
//! in it. Under each entry stand the passages of the paper its citations
//! rest on (write_source.rs).

use crate::i18n::t;
use crate::app::{Cmd, Library, Side, Workspace};
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
    /// What the manuscript cites, as worked out before the page was last drawn (`cite_refresh`).
    pub listed: Listed,
    /// Where the manuscript read through is scrolled to: its references are at its end.
    pub read_scroll: ScrollHandle,
    /// Where in its paper each citation is from (write_source.rs).
    pub sources: crate::write_source::Sourcing,
}

/// What the manuscript cites. Worked out once before the writing page is
/// drawn: the panel's tab, the outline's row, the page and the end of the
/// read-through all read it, and none of them builds the index again.
#[derive(Default)]
pub struct Listed {
    /// The reference list of the part being written.
    pub here: Vec<Entry>,
    /// The whole manuscript's.
    pub whole: Vec<Entry>,
    /// The citations that name no paper to cite, wherever in the manuscript they stand.
    unresolved: Vec<Issue>,
    /// Old-style citations of the part being written that can become keys.
    upgrades: Vec<(usize, usize, String, String)>,
    /// The papers that fit what is typed into the find box.
    offered: Vec<Offer>,
    /// Every citation of the manuscript that names a paper, with the passage kept for it.
    pub standing: Vec<crate::write_source::Standing>,
}

impl Listed {
    /// The list on show: the whole manuscript's, or the part's.
    pub fn shown(&self, whole: bool) -> &[Entry] {
        if whole { &self.whole } else { &self.here }
    }

    /// What does not resolve in the whole manuscript, or in one section of it.
    pub fn issues<'a>(&'a self, whole: bool, section: &'a str) -> impl Iterator<Item = &'a Issue> {
        self.unresolved.iter().filter(move |issue| whole || issue.section == section)
    }
}

/// One entry of the reference list.
pub struct Entry {
    pub reference: cite::Reference,
    /// The sections that cite it, by name, in the manuscript's order.
    pub cited_in: Vec<String>,
}

/// A citation that names no paper to cite.
pub struct Issue {
    /// The section it is written in: its id, and its name.
    pub section: String,
    pub place: String,
    /// The citation as written and what is wrong with it.
    pub said: String,
    /// The paper is in the trash and can come back: a warning.
    pub trashed: bool,
    /// For one that fits several papers, in the part being written: the papers to choose from.
    pub picks: Vec<Pick>,
}

pub struct Pick {
    pub paper_id: String,
    pub label: String,
    /// What to put there, when the parenthesis cites this one paper alone: (start, end, what is there, what replaces it).
    pub fix: Option<(usize, usize, String, String)>,
}

/// A paper offered while part of it is typed.
pub struct Offer {
    pub paper_id: String,
    pub key: String,
    pub title: String,
    pub about: String,
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
        Citing { find, kept: store.cite_keys().unwrap_or_default(), whole: false, listed: Listed::default(), read_scroll: ScrollHandle::new(), sources: crate::write_source::Sourcing::new(store.passages(manuscript).unwrap_or_default()) }
    }
}

/// Every paper of the open library, the trash's included, with its key.
pub(crate) fn index(lib: &Library) -> Index<'_> {
    Index::new(&lib.papers, lib.trash.iter().map(|entry| &entry.paper), &lib.citing.kept)
}

/// What the manuscript on show cites, section by section.
fn listed(lib: &Library, cx: &App) -> Listed {
    let Some(manuscript) = lib.writing.current() else { return Listed::default() };
    let index = index(lib);
    let section = lib.writing.section_id.as_str();
    let current = lib.writing.editor.read(cx).text();
    // Every section in its order; the one being written as the editor has it.
    let found: Vec<(&str, &str, &str, Vec<Cited>)> = manuscript
        .sections
        .iter()
        .map(|part| {
            let text = if part.id == section { current } else { part.body.as_str() };
            (part.id.as_str(), part.name.as_str(), text, cite::find(text, &index))
        })
        .collect();
    let all: Vec<Cited> = found.iter().flat_map(|(_, _, _, cited)| cited.iter().cloned()).collect();
    let (text_here, cited_here) = found.iter().find(|(id, ..)| *id == section).map(|(_, _, text, cited)| (*text, cited.clone())).unwrap_or_default();
    let entries = |cited: &[Cited]| -> Vec<Entry> {
        cite::references(cited, &index)
            .into_iter()
            .map(|reference| {
                let cited_in = found.iter().filter(|(_, _, _, cited)| cited.iter().any(|one| one.found.paper() == Some(reference.paper_id.as_str()))).map(|(_, name, ..)| name.to_string()).collect();
                Entry { reference, cited_in }
            })
            .collect()
    };
    let key_of = |id: &str| index.key(id).unwrap_or("").to_string();

    // What does not resolve, where it is written; in the part being written it can be put right.
    let mut unresolved = vec![];
    for (part, name, text, cited) in &found {
        for one in cited {
            let shown = if one.old { format!("（{}）", one.written) } else { format!("[{}]", one.written) };
            let what = match &one.found {
                Found::Paper(_) => continue,
                Found::Trashed(_) => t("这篇文献在回收站里"),
                Found::Removed => t("这篇文献已从库里删除"),
                Found::Unknown if one.old => t("库里没有对得上的文献"),
                Found::Unknown => t("库里没有这个引用键"),
                Found::Ambiguous(_) => t("对得上不止一篇，选一篇："),
            };
            let mut picks = vec![];
            if let (Found::Ambiguous(ids), true) = (&one.found, *part == section) {
                // Only a parenthesis citing this one paper is replaced whole.
                let alone = cited.iter().filter(|other| other.group == one.group).count() == 1;
                for id in ids {
                    let label = format!("@{} {}", key_of(id), index.paper(id).map(|paper| paper.title.clone()).unwrap_or_default());
                    let fix = alone.then(|| (one.group.start, one.group.end, text[one.group.clone()].to_string(), cite::written(&key_of(id))));
                    picks.push(Pick { paper_id: id.clone(), label, fix });
                }
            }
            unresolved.push(Issue { section: part.to_string(), place: name.to_string(), said: format!("{shown} {what}"), trashed: matches!(one.found, Found::Trashed(_)), picks });
        }
    }
    // Old-style citations that name one paper each can become keys.
    let upgrades = cite::upgrades(&cited_here, &index).into_iter().map(|(range, to)| (range.start, range.end, text_here[range].to_string(), to)).collect();
    let offered = index
        .matching(lib.citing.find.read(cx).text().trim(), OFFERED)
        .into_iter()
        .map(|paper| Offer { paper_id: paper.id.clone(), key: key_of(&paper.id), title: paper.title.clone(), about: [paper.short_authors(), paper.year.map(|y| y.to_string()).unwrap_or_default()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ") })
        .collect();
    let standing = crate::write_source::standing(found.iter().map(|(id, _, text, cited)| (*id, *text, cited.as_slice())), &lib.citing.sources.kept);
    Listed { here: entries(&cited_here), whole: entries(&all), unresolved, upgrades, offered, standing }
}

impl Workspace {
    /// Before the writing page is drawn: what the manuscript cites.
    pub(crate) fn cite_refresh(&mut self, cx: &App) {
        let Some(lib) = self.lib.as_ref() else { return };
        let listed = listed(lib, cx);
        if let Some(lib) = self.lib.as_mut() { lib.citing.listed = listed }
    }

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

    /// The page of the panel beside the manuscript: the reference list of the part being written or of the
    /// whole manuscript, what does not resolve, and citing any paper of the library.
    pub(crate) fn render_references(&self, width: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let (citing, listed) = (&lib.citing, &lib.citing.listed);
        let whole = citing.whole;
        let entries = listed.shown(whole);
        // A width in pixels for what wraps: a text that shares its row is measured narrower than it is drawn.
        let inner = width - 28.;

        let issues: Vec<AnyElement> = listed
            .issues(whole, &lib.writing.section_id)
            .enumerate()
            .map(|(n, issue)| {
                let said = if whole { format!("{}：{}", issue.place, issue.said) } else { issue.said.clone() };
                let picks: Vec<AnyElement> = issue
                    .picks
                    .iter()
                    .enumerate()
                    .map(|(m, pick)| {
                        let button = ui::ghost_button(SharedString::from(format!("cite-pick-{n}-{m}")), None, Some(pick.label.clone().into())).h(px(22.)).px(px(6.)).max_w(px(inner)).truncate().text_size(px(12.));
                        match &pick.fix {
                            Some(fix) => button.on_click(self.on(Cmd::WriteCiteFix(vec![fix.clone()]), cx)),
                            None => button.on_click(self.on(Cmd::GotoSource(pick.paper_id.clone(), None, String::new()), cx)),
                        }
                        .into_any_element()
                    })
                    .collect();
                div().flex().flex_col().w(px(inner)).gap(px(2.)).child(div().text_size(px(12.)).line_height(px(18.)).text_color(if issue.trashed { Theme::warning() } else { Theme::danger() }).child(said)).children(picks).into_any_element()
            })
            .collect();

        let typing = !citing.find.read(cx).text().trim().is_empty();
        let offered: Vec<AnyElement> = listed
            .offered
            .iter()
            .map(|offer| {
                div()
                    .id(SharedString::from(format!("cite-offer-{}", offer.paper_id)))
                    .flex()
                    .flex_col()
                    .w(px(inner))
                    .gap(px(1.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .child(div().truncate().text_size(px(12.)).text_color(Theme::text()).child(offer.title.clone()))
                    .child(div().flex().items_center().gap(px(6.)).text_size(px(11.)).child(div().flex_none().text_color(Theme::accent()).child(format!("@{}", offer.key))).child(div().flex_1().min_w_0().truncate().text_color(Theme::text_tertiary()).child(offer.about.clone())))
                    .on_click(self.on(Cmd::WriteCite(offer.paper_id.clone()), cx))
                    .into_any_element()
            })
            .collect();

        // Where each citation on show is from: the passage kept for it, and the citation the caret is in.
        let standing: Vec<&crate::write_source::Standing> = listed.standing.iter().filter(|one| whole || one.section == lib.writing.section_id).collect();
        let at = self.source_at(&listed.standing, cx);
        self.source_follow(at, at.and_then(|one| entries.iter().position(|entry| entry.reference.paper_id == one.citation.paper)));
        let source_actions = self.render_source_actions(&standing, cx);
        let rows: Vec<AnyElement> = entries
            .iter()
            .map(|entry| {
                let reference = &entry.reference;
                // In the whole manuscript's list: which sections cite the paper.
                let cited_in = if whole { entry.cited_in.join(" · ") } else { String::new() };
                let own: Vec<&crate::write_source::Standing> = standing.iter().copied().filter(|one| one.citation.paper == reference.paper_id).collect();
                let passages = self.render_passages("list", &own, at, width - 36., cx);
                // The entry leads to its paper; each passage under it, to where it stands in the paper.
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(width - 20.))
                    .child(
                        div()
                            .id(SharedString::from(format!("cite-ref-{}", reference.paper_id)))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::hover()))
                            .child(div().text_size(px(12.)).line_height(px(18.)).text_color(if reference.trashed { Theme::text_tertiary() } else { Theme::text() }).child(reference.text.clone()))
                            .child(div().flex().items_center().gap(px(6.)).text_size(px(11.)).child(div().flex_none().text_color(Theme::accent()).child(format!("@{}", reference.key))).child(div().flex_1().min_w_0().truncate().text_color(Theme::text_tertiary()).child(cited_in)))
                            .on_click(self.on(Cmd::GotoSource(reference.paper_id.clone(), None, String::new()), cx)),
                    )
                    .when(!passages.is_empty(), |d| d.child(div().flex().flex_col().gap(px(4.)).pl(px(8.)).pb(px(6.)).children(passages)))
                    .into_any_element()
            })
            .collect();

        let mut scope = |id: &'static str, name: String, on_whole: bool| {
            let on = whole == on_whole;
            div().id(id).flex().flex_none().items_center().h(px(22.)).px(px(8.)).rounded(px(11.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent())).when(!on, |d| d.text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(self.on(Cmd::WriteCiteScope(on_whole), cx)).child(name)
        };
        let (part, all) = (scope("cite-scope-part", t("本节").into(), false), scope("cite-scope-whole", tf!("全文", "Manuscript"), true));
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(px(22.)).px(px(6.)).text_size(px(12.));
        let empty = if !whole && !listed.whole.is_empty() {
            ui::empty_state("library", t("这一节还没有引用"), &tf!("全文引用了 {} 篇，点「全文」查看。在候选句上点「引用」，或在上面找一篇文献，就引用到光标处。", "The manuscript cites {} papers: see Manuscript. Click Cite on a candidate, or find a paper above, to cite it at the caret.", listed.whole.len()))
        } else {
            ui::empty_state("library", t("还没有引用"), t("在候选句上点「引用」，或在上面按作者、题目找一篇文献，就引用到光标处。"))
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.))
                    .px(px(14.))
                    .pt(px(12.))
                    .pb(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_none().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(t("参考文献")))
                            .child(ui::hint("cite-about", t("在候选句上点「引用」，或在这里找一篇文献。引用写作 [@Zhang2024]，导出的 Markdown 和 BibTeX 可以直接交给 Pandoc。每篇文献下面是出处：引用它的那句话依据的文献原文，点一下在 PDF 里看。")))
                            .child(div().flex_1())
                            .child(part)
                            .child(all),
                    )
                    // Finding a paper to cite: what is typed is offered right under the box.
                    .child(ui::field(&citing.find, window, cx))
                    .when(typing, |d| d.child(if offered.is_empty() { div().px(px(8.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("没有对得上的文献")).into_any_element() } else { div().flex().flex_col().children(offered).into_any_element() }))
                    .children(issues)
                    .when(!listed.upgrades.is_empty(), |d| d.child(div().flex().child(small("cite-upgrade", t("旧式引用改为引用键")).on_click(self.on(Cmd::WriteCiteFix(listed.upgrades.clone()), cx))))),
            )
            .when(!entries.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.))
                        .pl(px(18.))
                        .pr(px(10.))
                        .pb(px(4.))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(Theme::text_tertiary()).child(if whole { tf!("全文引用 {} 篇", "{} papers cited in the manuscript", entries.len()) } else { tf!("本节引用 {} 篇", "{} papers cited in this section", entries.len()) }))
                        .children(source_actions)
                        .child(small("cite-copy", t("复制")).on_click(self.on(Cmd::WriteCopyReferences, cx)))
                        .child(small("cite-bib", t("导出 BibTeX")).on_click(self.on(Cmd::WriteExportCited, cx))),
                )
            })
            .child(if entries.is_empty() { div().flex().flex_col().flex_1().min_h_0().px(px(14.)).child(empty).into_any_element() } else { div().id("cite-references").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&citing.sources.scroll).px(px(10.)).pb(px(12.)).gap(px(1.)).children(rows).into_any_element() })
    }

    /// The end of the manuscript read through: its reference list, as a paper ends in one.
    pub(crate) fn render_references_read(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref()?;
        let listed = &lib.citing.listed;
        let issues: Vec<AnyElement> = listed.issues(true, "").map(|issue| div().text_size(px(12.)).line_height(px(18.)).text_color(if issue.trashed { Theme::warning() } else { Theme::danger() }).child(format!("{}：{}", issue.place, issue.said)).into_any_element()).collect();
        if listed.whole.is_empty() && issues.is_empty() { return None }
        let rows: Vec<AnyElement> = listed
            .whole
            .iter()
            .map(|entry| {
                let reference = &entry.reference;
                div()
                    .id(SharedString::from(format!("cite-read-{}", reference.paper_id)))
                    .px(px(8.))
                    .mx(px(-8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .text_size(px(13.))
                    .line_height(px(21.))
                    .text_color(if reference.trashed { Theme::text_tertiary() } else { Theme::text() })
                    .child(reference.text.clone())
                    .on_click(self.on(Cmd::GotoSource(reference.paper_id.clone(), None, String::new()), cx))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text()).child(t("参考文献")))
                        .when(!listed.whole.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(tf!("{} 篇", "{} papers", listed.whole.len())))),
                )
                .children(issues)
                .child(div().flex().flex_col().gap(px(2.)).children(rows))
                .into_any_element(),
        )
    }

    /// The outline's row for the references: how many papers the manuscript cites, whether a citation names none, and whether the whole manuscript's list is on show.
    pub(crate) fn references_row(&self) -> (usize, bool, bool) {
        let Some(lib) = self.lib.as_ref() else { return (0, false, false) };
        (lib.citing.listed.whole.len(), lib.citing.listed.issues(true, "").next().is_some(), lib.writing.side == Side::References && lib.citing.whole && !lib.writing.reading)
    }
}
