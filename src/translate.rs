//! Single-paper and batch translations share requests and the library cache.

use crate::theme::text_px;
use crate::i18n::t;
use crate::app::{Cmd, Translated, Workspace};
use crate::{theme::Theme, ui};
use gpui::{prelude::FluentBuilder, *};
use std::sync::Arc;
use wisp_core::ai::{self, Profile};
use wisp_core::settings::Task;
use wisp_core::store::translation_key;
use wisp_core::translation::{Batch, Field};

pub struct TranslationRun {
    pub batch: Batch,
    profile: Profile,
    prompt: String,
    /// The model it was started with, as the card names it.
    model: String,
    /// How many requests are in flight at once (设置 › AI › 任务).
    workers: usize,
}

impl Workspace {
    /// Switching the list to translations also restores titles kept by an
    /// earlier session, without making any new requests for the list.
    pub(crate) fn load_title_translations(&mut self) {
        let prompt = ai::translation_prompt(&self.settings.translation_prompt);
        let Some(lib) = self.lib.as_mut().filter(|lib| lib.show_translation) else { return };
        for paper in &lib.papers {
            let key = translation_key(&paper.title, prompt);
            if !lib.translations.contains_key(&key) && let Ok(Some(text)) = lib.store.translation(&key) {
                lib.translations.insert(key, Translated::Done(text));
            }
        }
    }

    pub(crate) fn translate_selected(&mut self, retry: bool, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Translate);
        let prompt = self.settings.translation_prompt.clone();
        // The paper on show: the one open in the detail panel, or the one a round of 逐篇筛选 is at.
        let Some(paper) = self.lib.as_ref().filter(|lib| lib.show_translation).and_then(|lib| lib.selected_paper().or(lib.round_paper())) else { return };
        for text in [paper.title.clone(), paper.abstract_text.clone()] {
            self.translate_text(&text, &profile, &prompt, retry, false, cx);
        }
        cx.notify();
    }

    /// Snapshot the chosen field, model, instructions and how many requests go
    /// at once. Later selections or
    /// settings changes do not change what this run sends or where it is cached.
    pub(crate) fn translate_batch(&mut self, ids: Vec<String>, field: Field, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Translate);
        let prompt = self.settings.translation_prompt.clone();
        let workers = self.settings.translate_workers;
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.translation_run.as_ref().is_some_and(|run| run.batch.running()) {
            return self.say(t("已有批量翻译正在进行，请等这一轮结束。"), cx);
        }
        let batch = Batch::new(ids.iter().filter_map(|id| lib.paper(id)), field, &prompt);
        if batch.total == 0 { return }
        lib.show_translation = true;
        lib.translation_run = Some(TranslationRun { batch, model: format!("{} · {}", profile.title(), profile.model), profile, prompt, workers });
        self.batch_translation_next(cx);
        cx.notify();
    }

    /// Keep `workers` requests of the batch in flight. A request already
    /// made by the detail panel is awaited, not sent twice; a previous
    /// failure can be retried. Called again whenever an answer comes in.
    fn batch_translation_next(&mut self, cx: &mut Context<Self>) {
        // What came back since: this batch's answers, or the detail panel's for a text of it.
        if let Some(lib) = self.lib.as_mut() && let Some(run) = lib.translation_run.as_mut() {
            let settled: Vec<(String, Result<(), String>)> = run.batch.working.iter().filter_map(|job| match lib.translations.get(&job.key) {
                Some(Translated::Busy) => None,
                Some(Translated::Done(_)) => Some((job.key.clone(), Ok(()))),
                Some(Translated::Failed(error)) => Some((job.key.clone(), Err(error.clone()))),
                None => Some((job.key.clone(), Err(t("没有取得译文，请重试。").into()))),
            }).collect();
            for (key, result) in settled { run.batch.finish(&key, result, false) }
        }
        loop {
            let Some(run) = self.lib.as_mut().and_then(|lib| lib.translation_run.as_mut()) else { return };
            if run.batch.working.len() >= run.workers { return }
            let Some(job) = run.batch.take() else { return };
            let (profile, prompt) = (run.profile.clone(), run.prompt.clone());
            self.translate_text(&job.text, &profile, &prompt, true, false, cx);
            let lib = self.lib.as_mut().unwrap();
            let run = lib.translation_run.as_mut().unwrap();
            match lib.translations.get(&job.key) {
                // Asked for now, or already by the detail panel: its answer settles it.
                Some(Translated::Busy) => {}
                Some(Translated::Done(_)) => run.batch.finish(&job.key, Ok(()), true),
                Some(Translated::Failed(error)) => run.batch.finish(&job.key, Err(error.clone()), false),
                None => run.batch.finish(&job.key, Err(t("没有取得译文，请重试。").into()), false),
            }
        }
    }

    /// Ask for one text's translation, unless the window or the library
    /// already has it. `retry` asks again for what failed; `again` asks in
    /// any case and keeps the answer in place of what was kept: the person
    /// chose another model for this text once (a passage selected in the PDF).
    pub(crate) fn translate_text(&mut self, text: &str, profile: &Profile, prompt: &str, retry: bool, again: bool, cx: &mut Context<Self>) {
        if text.trim().is_empty() { return }
        let Some(lib) = self.lib.as_mut() else { return };
        let key = translation_key(text, ai::translation_prompt(prompt));
        if !again {
            match lib.translations.get(&key) {
                Some(Translated::Done(_) | Translated::Busy) => return,
                Some(Translated::Failed(_)) if !retry => return,
                _ => {}
            }
        }
        let request = if again { ai::translate_request(profile, prompt, text).map(Some) } else {
            lib.store.translation(&key).map_err(|e| tf!("无法读取译文：{e:#}", "Cannot read translation: {e:#}")).and_then(|cached| {
                if let Some(text) = cached {
                    lib.translations.insert(key.clone(), Translated::Done(text));
                    Ok(None)
                } else {
                    ai::translate_request(profile, prompt, text).map(Some)
                }
            })
        };
        let request = match request {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => { lib.translations.insert(key, Translated::Failed(error)); return }
        };
        lib.translations.insert(key.clone(), Translated::Busy);
        // The answer to an earlier request for this text, back after a later one, is dropped.
        let ask = lib.translation_asks.entry(key.clone()).or_default();
        *ask += 1;
        let ask = *ask;
        let (session, protocol, author) = (lib.translation_session.clone(), profile.protocol, format!("{} · {}", profile.title(), profile.model));
        let work = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol) });
        cx.spawn(async move |this, cx| {
            let translated = work.await;
            this.update(cx, |this, cx| {
                // Even closing and reopening the same library creates a new session.
                let Some(lib) = this.lib.as_mut().filter(|lib| Arc::ptr_eq(&lib.translation_session, &session)) else { return };
                if lib.translation_asks.get(&key) != Some(&ask) { return }
                let saved = translated.and_then(|text| {
                    if again { lib.store.replace_translation(&key, &text, &author) } else { lib.store.save_translation(&key, &text, &author) }.map_err(|e| tf!("无法保存译文：{e:#}", "Cannot save translation: {e:#}"))?;
                    Ok(text.trim().to_string())
                });
                lib.translations.insert(key, match saved { Ok(text) => Translated::Done(text), Err(error) => Translated::Failed(error) });
                this.sync_pdf_translation(cx);
                this.batch_translation_next(cx);
                cx.notify();
            }).ok();
        }).detach();
    }

    pub(crate) fn render_batch_translation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.lib.as_ref()?.translation_run.as_ref()?;
        let batch = &run.batch;
        let running = batch.running();
        let status = match (running, batch.stopped) {
            (true, true) => t("正在停止，等待已发出的请求"),
            (true, false) => t("正在翻译"),
            (false, true) => t("已停止"),
            (false, false) => t("已完成"),
        };
        let heading = tf!("批量翻译{} · {status} · {} / {} 篇", "Batch translation{} · {status} · {} / {} papers", t(batch.field.label()), batch.done(), batch.total);
        let mut summary = tf!("新译 {} · 已有译文 {} · 空内容跳过 {} · 失败 {}", "{} translated · {} cached · {} empty skipped · {} failed", batch.translated, batch.cached, batch.skipped, batch.failed);
        if batch.stopped && !running { summary.push_str(&tf!(" · 未处理 {}", " · {} unprocessed", batch.total - batch.done())) }
        Some(div().flex().flex_col().flex_none().gap(px(6.)).mx(px(16.)).my(px(8.)).p(px(10.)).rounded(px(8.)).bg(Theme::bg_sunken()).border_1().border_color(Theme::line())
            .child(div().flex().items_center().gap(px(8.))
                .when(running, |d| d.child(ui::spinner("batch-translating", 13., Theme::accent())))
                .child(div().flex_none().text_size(text_px(12.)).child(heading))
                .child(div().flex_1().min_w_0().truncate().text_right().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(tf!("{} · 同时 {} 篇", "{} · {} papers at a time", run.model, run.workers)))
                .when(running && !batch.stopped, |d| d.child(ui::ghost_button("translation-stop", None, Some(t("停止").into())).on_click(self.on(Cmd::StopBatchTranslation, cx))))
                .when(!running, |d| d.child(ui::ghost_button("translation-dismiss", Some("x"), None).on_click(self.on(Cmd::DismissBatchTranslation, cx)))))
            .child(crate::screen::progress_bar(batch.done(), batch.total))
            .child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(summary))
            .when(!batch.error.is_empty(), |d| d.child(ui::error_line(&tf!("最近一次失败：{}；可再次选择文献执行批量翻译。", "Latest failure: {}. Select papers again to retry batch translation.", batch.error))))
            .into_any_element())
    }
}
