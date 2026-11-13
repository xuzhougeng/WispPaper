//! 写作: the manuscript on the left, one part at a time, and on the right
//! the sentences the library's papers have in the same part: how they open
//! it until something is asked, then those closest to the selection or the
//! paragraph at the caret, asked for by a shortcut (write_find.rs). A
//! sentence can be cited, copied, unfolded into the sentences around it,
//! or looked at where it stands in its PDF. The panel also says how much of the library there is
//! to draw on, and lets a model sort the full texts into parts or read the
//! pages of files that gave no text.

use crate::i18n::{label, t};
use crate::app::{Cmd, MenuItem, MenuKind, Side, Workspace};
use crate::settings::SettingsPage;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use serde_json::Value;
use wisp_core::settings::Task;
use wisp_core::structure::SECTIONS;

/// The tags offered for narrowing the candidates, in a menu: the commonest.
const NARROWING_TAGS: usize = 24;

/// The widest the manuscript's text is set, padding included.
const EDITOR_MEASURE: f32 = 720.;

/// The manuscript's right margin while comments mark it: room for their markers.
const MARGIN: f32 = 36.;

/// How long a text is, as a writer counts it: a Chinese, Japanese or Korean
/// letter is one, and so is every word of a language that spaces them.
/// The second is whether letters were counted, which is what to call the number.
pub(crate) fn text_length(text: &str) -> (usize, bool) {
    let letters = text.chars().filter(|c| wisp_core::structure::is_cjk(*c)).count();
    let words = text.split(|c: char| c.is_whitespace() || wisp_core::structure::is_cjk(c)).filter(|word| word.chars().any(char::is_alphanumeric)).count();
    (letters + words, letters > 0)
}

/// That length in words: 「1200 字」 for a text with Chinese in it, 「300 词」 for one without.
pub(crate) fn length_label(text: &str) -> String {
    match text_length(text) {
        (n, true) => tf!("{n} 字", "{n} characters"),
        (n, false) => tf!("{n} 词", "{n} words"),
    }
}

impl Workspace {
    pub fn render_write(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.comment_reveal(window, cx);
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let text = writing.editor.read(cx).text().to_string();
        // The words comments are about, drawn on a tint in the manuscript, and a marker in its margin on each line that has some.
        let marks = self.comment_marks(&text);
        let noted: Vec<(std::ops::Range<usize>, Hsla)> = marks.iter().map(|(range, tint, _)| (range.clone(), *tint)).collect();
        let tops = writing.editor.read(cx).mark_tops().to_vec();
        let markers = self.render_comment_markers(&marks, tops.get(..marks.len()).unwrap_or(&[]), cx);
        // What the assistant is changing and has changed (revise.rs): tinted after the comments' words, a bar in the left margin on its lines.
        // The tints move for a moment after each change and while one is being written: frames are asked for until they rest.
        let (changed, bars, moving) = self.revise_marks(&text, std::time::Instant::now());
        if moving { window.request_animation_frame() }
        let bars = self.render_revise_bars(&bars, cx);
        let lib = self.lib.as_ref().unwrap();
        lib.writing.editor.update(cx, |input, _| input.set_marks(noted.into_iter().chain(changed).collect()));
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let text = writing.editor.read(cx).text();
        let (length, characters) = (length_label(text), text.chars().filter(|c| !c.is_whitespace()).count());
        let focused = writing.editor.focus_handle(cx).is_focused(window);
        let stacked = width < 680.;
        let panel = if stacked { width } else if width > 980. { 420. } else { 340. };

        let editor = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(stacked, |d| d.flex_none().h(px(800.)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .child(self.render_manuscript_bar(window, cx)),
            )
            .child(
                div().flex().flex_col().flex_1().min_h_0().px(px(20.)).pb(px(8.)).child(if writing.reading { self.render_manuscript_reading(cx) } else {
                    div()
                        .id("write-editor")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&writing.editor_scroll)
                        .rounded(px(10.))
                        .border_1()
                        .border_color(if focused { Theme::accent().opacity(0.55) } else { Theme::line() })
                        .bg(Theme::bg_body())
                        .child(div().relative().w_full().max_w(px(EDITOR_MEASURE)).mx_auto().pl(px(18.)).pr(px(if markers.is_empty() { 18. } else { MARGIN })).py(px(14.)).text_size(px(15.)).line_height(px(26.)).text_color(Theme::text()).child(crate::input::with_selection_toolbar(&writing.editor, self.render_assist_bar(true, cx))).children(markers).children(bars))
                        .into_any_element()
                }),
            )
            .when(!writing.reading && writing.editor.read(cx).selection().is_empty(), |d| d.child(self.render_assist_bar(false, cx)))
            // With the caret in a citation: the paper it names and the passage of that paper its sentence rests on.
            .children(if writing.reading { None } else { self.render_source_strip(if stacked { width } else { width - panel } - 40., cx) })
            // Under the manuscript, the box where the assistant is told what to change in this section.
            .when(!writing.reading, |d| d.child(self.render_revise(if stacked { width } else { width - panel } - 40., window, cx)))
            .child(div().flex().flex_none().px(px(22.)).pb(px(12.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child(div().flex_1().min_w_0().child(if writing.reading {
                let whole: String = writing.current().map_or(String::new(), |m| m.sections.iter().map(|s| s.body.as_str()).collect::<Vec<_>>().join("\n"));
                tf!("全文 {} · 点一节的名称，回到那一节写", "Manuscript: {} · Click a section name to edit it", length_label(&whole))
            } else if characters == 0 {
                t("随写随存").to_string()
            } else {
                tf!("{length} · 已保存", "{length} · Saved")
            })));

        div().id("write-page").flex().size_full().when(stacked, |d| d.flex_col().overflow_y_scroll()).child(editor)
            .when(!writing.reading, |d| d.child(self.render_candidates(panel, window, cx).when(stacked, |d| d.flex_none().h(px(560.)))))
            .into_any_element()
    }

    /// The key that asks for candidates, as people read it.
    pub(crate) fn find_key(&self) -> String {
        let key = crate::shortcuts::key_of("write-find", &self.settings.shortcuts);
        if key.is_empty() { t("右边的按钮").into() } else { crate::shortcuts::pretty(&key) }
    }

    /// The panel beside the manuscript: what there is to draw on, and the candidates.
    fn render_candidates(&self, width: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let search = &writing.search;
        let name = label(&SECTIONS, writing.kind());
        let found = search.candidates.as_ref().and_then(|result| result.as_ref().ok());
        let sentences: &[Value] = found.and_then(|value| value["sentences"].as_array()).map_or(&[], Vec::as_slice);
        let asked = search.asked.as_ref();
        let heading = match asked {
            None => String::new(),
            Some(asked) if asked.text.is_empty() => t("各篇文献这一部分的开头").into(),
            Some(asked) if asked.literal => tf!("含有「{}」中的词的句子", "Sentences containing words in “{}”", asked.text),
            Some(asked) => {
                let short: String = asked.text.chars().take(36).collect();
                tf!("与「{short}{}」相近的句子", "Sentences similar to “{short}{}”", if short.len() < asked.text.len() { "…" } else { "" })
            }
        };
        let drawn_on = found.map(|value| {
            let (count, papers, without) = (value["sentencesInSection"].as_u64().unwrap_or(0), value["papersWithSection"].as_u64().unwrap_or(0), value["papersWithout"].as_u64().unwrap_or(0));
            let elsewhere = value["sentencesInOtherLanguages"].as_u64().unwrap_or(0);
            tf!("{papers} 篇文献的 {count} 句{}{}", "{count} sentences from {papers} papers{}{}", if without > 0 { tf!(" · {without} 篇没有{name}可查", " · {without} papers have no {name} to search") } else { String::new() }, if elsewhere > 0 { tf!(" · 另有 {elsewhere} 句不是稿件的语言", " · {elsewhere} more sentences in another language") } else { String::new() })
        });
        let rows: Vec<AnyElement> = sentences.iter().enumerate().map(|(n, sentence)| self.render_candidate(n, sentence, cx)).collect();
        let narrowed = !search.tag.is_empty() || search.starred;
        let empty = match &search.candidates {
            Some(Err(error)) => Some(ui::error_line(error).into_any_element()),
            Some(Ok(value)) if rows.is_empty() => Some(
                if value["sentencesInSection"].as_u64() == Some(0) && narrowed {
                    ui::empty_state("search", t("这些文献里没有这一部分的句子"), t("换一个标签，或者回到全部文献。"))
                } else if value["sentencesInSection"].as_u64() == Some(0) {
                    if writing.kind() == "abstract" { ui::empty_state("pen-line", t("还没有摘要可参考"), t("文献库里的文献都没有摘要。通过 DOI 或检索式添加的文献会带上摘要。")) } else { ui::empty_state("pen-line", t("还没有全文可参考"), t("把论文的 PDF 拖进文献库，或者在文献详情里附上 PDF。全文会被分成导言、方法、结果、讨论，这里就有句子可看了。")) }
                } else {
                    ui::empty_state("search", t("没有相近的句子"), t("换几个词试试，或者选中一句再查找。"))
                }
                .into_any_element(),
            ),
            _ => None,
        };
        let groups: Vec<AnyElement> = search
            .groups
            .iter()
            // One text each: a row of two wrapping texts is measured narrower than it is drawn.
            .map(|group| div().text_size(px(11.)).line_height(px(16.)).text_color(Theme::text_secondary()).child(if group.kind.is_empty() { group.words.clone() } else { format!("{} · {}", group.kind, group.words) }).into_any_element())
            .collect();
        let key = self.find_key();
        // What the panel is for, said once on hover rather than over the manuscript every time.
        let about = tf!("写哪一节，这里就列出文献库里的文献怎样开始这一部分。按 {key} 查找与选中的文字（没有选中时是光标所在的一段）相近的句子；用什么语言写都可以，模型会先把它变成几组检索词。句子是用来参考和引用的，不要照抄。", "This panel shows how the library's papers begin the part you are writing. Press {key} to find sentences similar to your selection or current paragraph; any language works, the model turns it into search terms first. Use the sentences as references and cite them; do not copy them verbatim.");
        let find_button = ui::icon_action("write-find", "scan-text", Theme::accent(), tf!("查找与选中的文字相近的句子；没有选中时，找光标所在的一段（{key}）", "Find sentences similar to the selection, or to the paragraph at the caret ({key})")).on_click(self.on(Cmd::WriteFind, cx));
        // How much there was to search: the number in the header, the rest on hover.
        let count = found.map(|value| value["sentencesInSection"].as_u64().unwrap_or(0));
        let drawn_on = count.zip(drawn_on).map(|(count, text)| div().id("write-drawn-on").flex_none().text_size(px(11.)).text_color(Theme::text_tertiary()).child(tf!("{count} 句", "{count} sentences")).tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(text.clone())).into()));

        let side = div().flex().flex_col().flex_none().w(px(width)).h_full().border_l_1().border_color(Theme::line()).bg(Theme::bg_sunken()).child(self.render_side_switch(cx));
        // What an action on the text came to stands over any page of the panel.
        if writing.side != Side::Candidates {
            let card = self.render_assist_card(width, cx);
            let side = side.when(card.is_some(), |d| d.child(div().flex_none().h(px(10.)))).children(card);
            return if writing.side == Side::Comments { side.child(self.render_comments(width, window, cx)) } else { side.child(self.render_references(width, window, cx)) };
        }
        side
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
                            .child(div().flex_none().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tf!("候选句 · {name}", "Candidates · {name}")))
                            .child(ui::hint("write-about", about))
                            .when(search.searching, |d| d.child(ui::spinner("write-searching", 13., Theme::accent())))
                            .child(div().flex_1())
                            .children(drawn_on),
                    )
                    .child(div().flex().items_center().gap(px(4.)).child(div().flex_1().min_w_0().child(ui::field(&writing.find, window, cx))).child(find_button))
                    .when(!groups.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(6.))
                                .child(div().id("write-groups-about").flex_none().pt(px(2.)).child(ui::icon("sparkle", 12., Theme::accent())).tooltip(|_, cx| cx.new(|_| crate::write_assist::Tip(t("模型给出的检索词，各组分别查找后合并").into())).into()))
                                // A width in pixels: a column that shares the row is measured narrower than it is drawn, and leaves a gap under the words.
                                .child(div().flex_none().w(px(width - 28. - 18.)).flex().flex_col().gap(px(3.)).children(groups)),
                        )
                    })
                    .when(!search.note.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(Theme::warning()).child(search.note.clone())))
                    .child(self.render_narrowing(cx))
                    .child(self.render_groundwork(cx)),
            )
            .children(self.render_assist_card(width, cx))
            .when(!heading.is_empty(), |d| d.child(div().flex_none().px(px(14.)).pb(px(6.)).text_size(px(11.)).text_color(Theme::text_tertiary()).truncate().child(heading)))
            .child(match empty {
                Some(empty) => div().flex().flex_col().flex_1().min_h_0().px(px(14.)).child(empty).into_any_element(),
                None => div().id("write-candidates").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&writing.scroll).px(px(10.)).pb(px(12.)).gap(px(6.)).children(rows).into_any_element(),
            })
    }

    /// Which papers the candidates come from: all, the starred, or those with a tag.
    fn render_narrowing(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let search = &lib.writing.search;
        let tags = wisp_core::filter::tag_counts(&lib.papers);
        let starred = lib.papers.iter().any(|paper| paper.starred);
        if tags.is_empty() && !starred && !search.starred { return div() }
        // One button saying which papers the candidates come from; the choices are its menu.
        let mut shown: Vec<String> = vec![];
        if search.starred { shown.push(t("★ 星标").into()) }
        if !search.tag.is_empty() { shown.push(search.tag.clone()) }
        let narrowed = !shown.is_empty();
        let name = if narrowed { shown.join(" · ") } else { t("全部文献").to_string() };
        div().flex().items_center().child(
            ui::ghost_button("write-only", Some("sliders"), Some(name.into()))
                .h(px(24.))
                .px(px(7.))
                .max_w_full()
                .text_size(px(12.))
                .when(narrowed, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()))
                .child(ui::icon("chevron-down", 12., Theme::text_tertiary()))
                .on_click(self.on_menu(MenuKind::WriteNarrow, cx)),
        )
    }

    /// The menu of that button: all papers, the starred ones, the commonest tags.
    pub(crate) fn narrowing_menu(&self) -> Vec<MenuItem> {
        let Some(lib) = &self.lib else { return vec![] };
        let search = &lib.writing.search;
        let mut items = vec![MenuItem { label: t("全部文献").into(), checked: search.tag.is_empty() && !search.starred, note: None, cmd: Cmd::WriteOnly(String::new(), false) }];
        if search.starred || lib.papers.iter().any(|paper| paper.starred) { items.push(MenuItem { label: t("★ 星标").into(), checked: search.starred, note: None, cmd: Cmd::WriteOnly(search.tag.clone(), !search.starred) }) }
        for (tag, count) in wisp_core::filter::tag_counts(&lib.papers).into_iter().take(NARROWING_TAGS) {
            let on = search.tag == tag;
            items.push(MenuItem { label: tag.clone().into(), checked: on, note: Some(count.to_string().into()), cmd: Cmd::WriteOnly(if on { String::new() } else { tag }, search.starred) });
        }
        items
    }

    /// One candidate: the sentence, where it is from, and what can be done with it.
    fn render_candidate(&self, n: usize, sentence: &Value, cx: &mut Context<Self>) -> AnyElement {
        let field = |key: &str| sentence[key].as_str().unwrap_or("").to_string();
        let (paper, text, cite) = (field("paperId"), field("text"), field("cite"));
        let (before, after) = (field("before"), field("after"));
        let page = sentence["page"].as_u64().map(|page| page as usize);
        // The part of its paper the sentence is from: the part the candidates were asked of.
        let part = self.lib.as_ref().and_then(|lib| lib.writing.search.candidates.as_ref()?.as_ref().ok()?["section"].as_str().map(str::to_string)).unwrap_or_default();
        // Known by what it shows: the list is sorted anew with every search.
        let card = format!("{paper}-{}", sentence["position"].as_u64().unwrap_or(0));
        let id = |part: &str| SharedString::from(format!("write-{part}-{card}-{n}"));
        let unfolded = self.lib.as_ref().is_some_and(|lib| lib.writing.search.unfolded.contains(&card));
        // Who, which page and under what heading: the journal's name is cut short in a panel this wide.
        let mut about = vec![cite.clone(), page.map(|page| tf!("第 {page} 页", "Page {page}")).unwrap_or_default(), field("heading")];
        about.retain(|part| !part.is_empty());
        let small = |id: SharedString, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(px(22.)).px(px(6.)).text_size(px(12.));
        // 引用 stays in view; the others come up when the card is under the pointer.
        let group = id("group");
        let around = |text: String| div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_tertiary()).child(text);
        let card_view = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .relative()
            .group(group.clone())
            .when(unfolded && !before.is_empty(), |d| d.child(around(before.clone())))
            .child(div().text_size(px(13.)).line_height(px(21.)).text_color(Theme::text()).child(text.clone()))
            .when(unfolded && !after.is_empty(), |d| d.child(around(after.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(Theme::text_tertiary()).child(about.join(" · ")))
                    .child(small(id("cite"), t("引用")).text_color(Theme::accent()).on_click(self.on(Cmd::WriteCiteFrom(paper.clone(), wisp_core::passage::Candidate { text: text.clone(), part, page: page.unwrap_or(0) }), cx))),
            )
            // Over the end of that line while the card is under the pointer, so it takes no width otherwise.
            .child(
                div()
                    .absolute()
                    .right(px(52.))
                    .bottom(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(6.))
                    .rounded(px(6.))
                    .bg(Theme::bg_body())
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .when(!before.is_empty() || !after.is_empty(), |d| d.child(small(id("around"), if unfolded { t("收起") } else { t("上下文") }).on_click(self.on(Cmd::WriteUnfold(card.clone()), cx))))
                    .child(small(id("copy"), t("复制")).on_click(self.on(Cmd::CopyText(text.clone()), cx)))
                    // With a page, the sentence is shown where it stands in the PDF; an abstract's opens the record.
                    .child(small(id("source"), if page.is_some() { t("原文") } else { t("文献") }).on_click(self.on(Cmd::GotoSource(paper.clone(), page, if page.is_some() { text } else { String::new() }), cx))),
            );
        ui::fade_in(id("card"), card_view).into_any_element()
    }

    /// What the candidates are drawn from: how many full texts there are,
    /// how they were sorted into parts, and the model's part in it.
    fn render_groundwork(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let sorting = &writing.sorting;
        let count = |method: &str| writing.structures.values().filter(|info| info.method == method).count();
        let (by_model, by_rule, unread) = (count("model"), count("rules"), count("none"));
        // The texts with paragraphs no heading names the part of: a model may sort those better.
        let open = writing.structures.values().filter(|info| info.worth_asking()).count();
        // Files longer than one run of reading reads: the rest of their pages are still to be read.
        let in_part = writing.structures.values().filter(|info| info.read_in_part()).count();
        let reading_now = lib.extracting.len();
        let mut facts = vec![tf!("全文 {} 篇", "{} full texts", writing.structures.len() + reading_now)];
        if by_rule > 0 { facts.push(tf!("按标题分节 {by_rule}", "{by_rule} split by headings")) }
        if by_model > 0 { facts.push(tf!("模型协助 {by_model}", "{by_model} assisted by model")) }
        if open > 0 { facts.push(tf!("{open} 篇有段落没有标题可依", "{open} papers have paragraphs without headings")) }
        if unread > 0 { facts.push(tf!("读不出文字 {unread}", "{unread} without readable text")) }
        if in_part > 0 { facts.push(tf!("{in_part} 篇的页面只读了一部分", "{in_part} papers partially read")) }
        // Which languages the texts are in, once there is more than one.
        let mut languages: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for info in writing.structures.values().filter(|info| info.method != "none") {
            *languages.entry(info.language.as_str()).or_default() += 1;
        }
        if languages.len() > 1 {
            let mut shown: Vec<(&str, usize)> = languages.into_iter().collect();
            shown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            facts.push(shown.iter().map(|(code, n)| format!("{}{n}", wisp_core::structure::LANGUAGES.iter().find(|(key, _)| key == code).map_or(t("语言不明"), |(_, name)| t(name)))).collect::<Vec<_>>().join(" "));
        }
        let unread = unread + in_part;
        if reading_now > 0 { facts.push(tf!("正在读取 {reading_now}", "Reading {reading_now}")) }
        let configured = !self.settings.profile_for_or_default(Task::Structure).key().is_empty();
        let button = |id: &'static str, text: String| ui::secondary_button(id, text).h(px(24.)).px(px(8.)).text_size(px(12.));
        // A model's run, as an icon with how many papers it would read; what it does is said on hover.
        let sparkle = |id: &'static str, icon: &'static str, n: usize, tip: String| {
            ui::ghost_button(id, None, Some(n.to_string().into())).h(px(24.)).px(px(6.)).gap(px(4.)).text_size(px(12.)).text_color(Theme::accent()).flex_row_reverse().child(ui::icon(icon, 13., Theme::accent())).tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(tip.clone())).into())
        };
        let running = sorting.running();
        let titles: Vec<String> = sorting.working.iter().filter_map(|id| lib.files.iter().find(|f| f.id == *id)).filter_map(|file| lib.paper(&file.paper_id)).map(|paper| paper.title.clone()).collect();
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(6.))
                    // The first fact in words, the others on hover: they are bookkeeping, not what is written by.
                    .child(div().flex_none().text_size(px(11.)).text_color(Theme::text_tertiary()).child(facts[0].clone()))
                    .when(facts.len() > 1, |d| d.child(ui::hint("write-groundwork-about", facts.join(" · "))))
                    .child(div().flex_1())
                    .when(!running && !configured && open + unread > 0, |d| d.child(ui::icon_action("write-settings", "settings-gear", Theme::text_secondary(), t("配置模型后，可以让模型为没有标题的全文分节、读出没有文字的页面")).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
                    .when(!running && configured && open > 0, |d| d.child(sparkle("write-sort", "sparkle", open, tf!("让模型判断 {open} 篇有段落没有标题可依的全文", "Ask the model to sort {open} papers whose paragraphs have no heading")).on_click(self.on(Cmd::WriteRun(false), cx))))
                    .when(!running && configured && unread > 0, |d| d.child(sparkle("write-read", "scan-text", unread, tf!("让模型读页面：{unread} 篇读不出文字或只读了一部分", "Ask the model to read the pages of {unread} papers without readable text")).on_click(self.on(Cmd::WriteRun(true), cx))))
                    .when(running, |d| d.child(button("write-stop", t("停止").into()).on_click(self.on(Cmd::WriteStop, cx)))),
            )
            .when(sorting.total > 0 && (running || sorting.done > 0), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(Theme::line())
                        .bg(Theme::bg_body())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .text_size(px(12.))
                                .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(match (running, sorting.reading) {
                                    (true, true) => t("模型正在读页面"),
                                    (true, false) => t("模型正在分节"),
                                    (false, true) => t("页面读完了"),
                                    (false, false) => t("分节完成"),
                                }))
                                .child(div().flex_none().text_color(Theme::text_secondary()).child(format!("{} / {}", sorting.done, sorting.total)))
                                .when(sorting.failed > 0, |d| d.child(ui::chip(tf!("没成 {}", "{} failed", sorting.failed), Theme::danger(), Theme::danger_soft())))
                                .child(div().flex_1().min_w_0().truncate().text_right().text_size(px(11.)).text_color(Theme::text_tertiary()).child(sorting.model.clone())),
                        )
                        .child(crate::screen::progress_bar(sorting.done, sorting.total))
                        // A file longer than one run reads: how far it got, and that the next run goes on from there.
                        .when(!sorting.cut.is_empty(), |d| {
                            let first = &sorting.cut[0];
                            let more = if sorting.cut.len() > 1 { tf!("等 {} 篇", "{} papers waiting", sorting.cut.len()) } else { String::new() };
                            d.child(div().text_size(px(11.)).text_color(Theme::text_secondary()).child(tf!("「{}」{more}页数较多，这次只读了 {} / {} 页；再点「让模型读页面」从第 {} 页接着读。", "“{}”{more} has many pages; {} / {} were read. Click “Ask model to read” again to continue from page {}.", first.0, first.1, first.2, first.1 + 1)))
                        })
                        .children(titles.iter().enumerate().map(|(n, title)| {
                            let line = div().flex().items_center().gap(px(6.)).text_size(px(11.)).text_color(Theme::text_secondary()).child(ui::spinner(("write-working", n), 11., Theme::accent())).child(div().flex_1().min_w_0().truncate().child(title.clone()));
                            ui::fade_in(SharedString::from(format!("write-working-{title}")), line)
                        })),
                )
            })
            .when(!sorting.error.is_empty(), |d| d.child(ui::error_line(&sorting.error)))
    }
}

#[cfg(test)]
mod tests {
    use super::text_length;

    #[test]
    fn length_as_a_writer_counts_it() {
        assert_eq!(text_length("Roots adapt to soil."), (4, false));
        // Each Chinese letter is one, a Latin word among them one more.
        assert_eq!(text_length("单细胞 RNA-seq 技术"), (6, true));
        assert_eq!(text_length("  "), (0, false));
    }
}
