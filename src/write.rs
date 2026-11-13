//! 写作: the manuscript on the left, one part at a time, and on the right
//! the sentences the library's papers have in the same part: how they open
//! it until something is asked, then those closest to the selection or the
//! paragraph at the caret, asked for by a shortcut (write_find.rs). A
//! sentence can be cited, copied, unfolded into the sentences around it,
//! or looked at where it stands in its PDF. The panel also says how much of the library there is
//! to draw on, and lets a model sort the full texts into parts or read the
//! pages of files that gave no text.

use crate::i18n::{label, t};
use crate::app::{Cmd, Workspace};
use crate::settings::SettingsPage;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use serde_json::Value;
use wisp_core::settings::Task;
use wisp_core::structure::SECTIONS;

/// The tags offered for narrowing the candidates: the commonest.
const NARROWING_TAGS: usize = 6;

impl Workspace {
    pub fn render_write(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let text = writing.editor.read(cx).text();
        let (words, characters) = (text.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count(), text.chars().filter(|c| !c.is_whitespace()).count());
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
                    .child(self.render_manuscript_bar(window, cx))
                    .when(!writing.reading, |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("写哪一节，右边就列出文献库里的文献怎样开始这一部分；按 {} 查找与选中的文字（没有选中时是光标所在的一段）相近的句子。用什么语言写都可以，模型会先把它变成几组英文检索词。句子是用来参考和引用的，不要照抄。", "The right panel shows how papers begin the section you are writing. Press {} to find sentences similar to your selection or current paragraph. Any writing language is supported; the model generates English search terms. Use sentences as references and cite them; do not copy them verbatim.", self.find_key())))),
            )
            .child(
                div().flex().flex_col().flex_1().min_h_0().px(px(20.)).pb(px(8.)).child(if writing.reading { self.render_manuscript_reading(cx) } else {
                    div()
                        .id("write-editor")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(if focused { Theme::accent().opacity(0.55) } else { Theme::line() })
                        .bg(Theme::bg_body())
                        .child(div().px(px(18.)).py(px(14.)).text_size(px(15.)).line_height(px(26.)).text_color(Theme::text()).child(writing.editor.clone()))
                        .into_any_element()
                }),
            )
            .child(self.render_assist_bar(cx))
            .child(self.render_references(window, cx))
            .child(div().flex().flex_none().px(px(22.)).pb(px(12.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child(div().flex_1().min_w_0().child(if writing.reading {
                let total: usize = writing.current().map_or(0, |m| m.sections.iter().map(|s| s.body.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count()).sum());
                tf!("全文 {total} 词 · 点一节的名称，回到那一节写", "Manuscript: {total} words · Click a section name to edit it")
            } else if characters == 0 {
                t("草稿随写随存，留在这个文献库里；停笔或换一节时会留一个历史版本。").to_string()
            } else {
                tf!("{words} 词 · {characters} 字符 · 已保存", "{words} words · {characters} characters · Saved")
            })));

        div().id("write-page").flex().size_full().when(stacked, |d| d.flex_col().overflow_y_scroll()).child(editor)
            .when(!writing.reading, |d| d.child(self.render_candidates(panel, window, cx).when(stacked, |d| d.flex_none().h(px(560.)))))
            .into_any_element()
    }

    /// The key that asks for candidates, as people read it.
    fn find_key(&self) -> String {
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
        let find_button = ui::secondary_button("write-find", t("查找候选句")).h(px(28.)).px(px(10.)).text_size(px(12.)).child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(key)).on_click(self.on(Cmd::WriteFind, cx));

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .border_l_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.))
                    .px(px(14.))
                    .pt(px(16.))
                    .pb(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_none().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(tf!("候选句 · {name}", "Candidates · {name}")))
                            .when(search.searching, |d| d.child(ui::spinner("write-searching", 13., Theme::accent())))
                            .child(div().flex_1())
                            .children(drawn_on.map(|text| div().min_w_0().max_w_full().text_size(px(11.)).text_color(Theme::text_tertiary()).child(text))),
                    )
                    .child(div().flex().flex_col().items_start().gap(px(6.)).child(find_button).child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(t("找选中的文字；没有选中时，找光标所在的一段。"))))
                    .child(ui::field(&writing.find, window, cx))
                    .when(!groups.is_empty(), |d| d.child(div().flex().flex_col().gap(px(3.)).child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(t("模型给出的检索词，各组分别查找后合并："))).children(groups)))
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
        let option = |id: SharedString, name: String, on: bool, cmd: Cmd, this: &Self, cx: &mut Context<Self>| {
            div().id(id).flex().flex_none().items_center().h(px(22.)).px(px(8.)).rounded(px(11.)).text_size(px(11.)).cursor_pointer().border_1().when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).border_color(Theme::accent().opacity(0.35))).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(this.on(cmd, cx)).child(name).into_any_element()
        };
        let mut options = vec![option("write-only-all".into(), t("全部文献").into(), search.tag.is_empty() && !search.starred, Cmd::WriteOnly(String::new(), false), self, cx)];
        if starred || search.starred { options.push(option("write-only-starred".into(), t("★ 星标").into(), search.starred, Cmd::WriteOnly(search.tag.clone(), !search.starred), self, cx)) }
        for (tag, count) in tags.iter().take(NARROWING_TAGS) {
            let on = search.tag == *tag;
            options.push(option(SharedString::from(format!("write-only-tag-{tag}")), format!("{tag} {count}"), on, Cmd::WriteOnly(if on { String::new() } else { tag.clone() }, search.starred), self, cx));
        }
        if options.len() == 1 { return div() }
        div().flex().items_center().flex_wrap().gap(px(4.)).children(options)
    }

    /// One candidate: the sentence, where it is from, and what can be done with it.
    fn render_candidate(&self, n: usize, sentence: &Value, cx: &mut Context<Self>) -> AnyElement {
        let field = |key: &str| sentence[key].as_str().unwrap_or("").to_string();
        let (paper, text, cite) = (field("paperId"), field("text"), field("cite"));
        let (before, after) = (field("before"), field("after"));
        let page = sentence["page"].as_u64().map(|page| page as usize);
        // Known by what it shows: the list is sorted anew with every search.
        let card = format!("{paper}-{}", sentence["position"].as_u64().unwrap_or(0));
        let id = |part: &str| SharedString::from(format!("write-{part}-{card}-{n}"));
        let unfolded = self.lib.as_ref().is_some_and(|lib| lib.writing.search.unfolded.contains(&card));
        let mut about = vec![cite.clone(), field("journal"), page.map(|page| tf!("第 {page} 页", "Page {page}")).unwrap_or_default(), field("heading")];
        about.retain(|part| !part.is_empty());
        let small = |id: SharedString, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(px(22.)).px(px(6.)).text_size(px(12.));
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
            .when(unfolded && !before.is_empty(), |d| d.child(around(before.clone())))
            .child(div().text_size(px(13.)).line_height(px(21.)).text_color(Theme::text()).child(text.clone()))
            .when(unfolded && !after.is_empty(), |d| d.child(around(after.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(Theme::text_tertiary()).child(about.join(" · ")))
                    .when(!before.is_empty() || !after.is_empty(), |d| d.child(small(id("around"), if unfolded { t("收起") } else { t("上下文") }).on_click(self.on(Cmd::WriteUnfold(card.clone()), cx))))
                    .child(small(id("cite"), t("引用")).on_click(self.on(Cmd::WriteCite(paper.clone()), cx)))
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
                    .child(div().flex_1().min_w(px(120.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child(facts.join(" · ")))
                    .when(!running && !configured && open + unread > 0, |d| d.child(ui::ghost_button("write-settings", Some("settings-gear"), Some(t("配置模型").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
                    .when(!running && configured && open > 0, |d| d.child(button("write-sort", tf!("让模型判断 {open} 篇", "Ask model to structure {open} papers")).on_click(self.on(Cmd::WriteRun(false), cx))))
                    .when(!running && configured && unread > 0, |d| d.child(button("write-read", tf!("让模型读页面 {unread} 篇", "Ask model to read {unread} papers")).on_click(self.on(Cmd::WriteRun(true), cx))))
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
