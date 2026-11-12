//! Single-paper and batch translations share requests and the library cache.

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
        let Some(paper) = self.lib.as_ref().filter(|lib| lib.show_translation).and_then(|lib| lib.selected_paper()) else { return };
        for text in [paper.title.clone(), paper.abstract_text.clone()] {
            self.translate_text(&text, &profile, &prompt, retry, cx);
        }
        cx.notify();
    }

    /// Snapshot the chosen field, model and instructions. Later selections or
    /// settings changes do not change what this run sends or where it is cached.
    pub(crate) fn translate_batch(&mut self, ids: Vec<String>, field: Field, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Translate);
        let prompt = self.settings.translation_prompt.clone();
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.translation_run.as_ref().is_some_and(|run| run.batch.running()) {
            return self.say("已有批量翻译正在进行，请等这一轮结束。", cx);
        }
        let batch = Batch::new(ids.iter().filter_map(|id| lib.paper(id)), field, &prompt);
        if batch.total == 0 { return }
        lib.show_translation = true;
        lib.translation_run = Some(TranslationRun { batch, profile, prompt });
        self.batch_translation_next(cx);
        cx.notify();
    }

    /// Only one batch request at a time. A request already made by the detail
    /// panel is awaited, not sent twice; a previous failure can be retried.
    fn batch_translation_next(&mut self, cx: &mut Context<Self>) {
        loop {
            let Some(run) = self.lib.as_ref().and_then(|lib| lib.translation_run.as_ref()) else { return };
            let Some(job) = run.batch.jobs.front().cloned() else { return };
            let active = run.batch.active;
            if !active {
                let (profile, prompt) = (run.profile.clone(), run.prompt.clone());
                self.translate_text(&job.text, &profile, &prompt, true, cx);
            }
            let lib = self.lib.as_mut().unwrap();
            let run = lib.translation_run.as_mut().unwrap();
            match lib.translations.get(&job.key) {
                Some(Translated::Busy) => {
                    run.batch.active = true;
                    return;
                }
                Some(Translated::Done(_)) => run.batch.finish(Ok(()), !active),
                Some(Translated::Failed(error)) => run.batch.finish(Err(error.clone()), false),
                None => run.batch.finish(Err("没有取得译文，请重试。".into()), false),
            }
        }
    }

    fn translate_text(&mut self, text: &str, profile: &Profile, prompt: &str, retry: bool, cx: &mut Context<Self>) {
        if text.trim().is_empty() { return }
        let Some(lib) = self.lib.as_mut() else { return };
        let key = translation_key(text, ai::translation_prompt(prompt));
        match lib.translations.get(&key) {
            Some(Translated::Done(_) | Translated::Busy) => return,
            Some(Translated::Failed(_)) if !retry => return,
            _ => {}
        }
        let request = lib.store.translation(&key).map_err(|e| format!("无法读取译文：{e:#}")).and_then(|cached| {
            if let Some(text) = cached {
                lib.translations.insert(key.clone(), Translated::Done(text));
                Ok(None)
            } else {
                ai::translate_request(profile, prompt, text).map(Some)
            }
        });
        let request = match request {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => { lib.translations.insert(key, Translated::Failed(error)); return }
        };
        lib.translations.insert(key.clone(), Translated::Busy);
        let (session, protocol, author) = (lib.translation_session.clone(), profile.protocol, format!("{} · {}", profile.title(), profile.model));
        let work = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol) });
        cx.spawn(async move |this, cx| {
            let translated = work.await;
            this.update(cx, |this, cx| {
                // Even closing and reopening the same library creates a new session.
                let Some(lib) = this.lib.as_mut().filter(|lib| Arc::ptr_eq(&lib.translation_session, &session)) else { return };
                let saved = translated.and_then(|text| {
                    lib.store.save_translation(&key, &text, &author).map_err(|e| format!("无法保存译文：{e:#}"))?;
                    Ok(text.trim().to_string())
                });
                lib.translations.insert(key, match saved { Ok(text) => Translated::Done(text), Err(error) => Translated::Failed(error) });
                this.batch_translation_next(cx);
                cx.notify();
            }).ok();
        }).detach();
    }

    pub(crate) fn render_batch_translation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let batch = &self.lib.as_ref()?.translation_run.as_ref()?.batch;
        let running = batch.running();
        let status = match (running, batch.stopped) {
            (true, true) => "正在停止，等待当前请求",
            (true, false) => "正在翻译",
            (false, true) => "已停止",
            (false, false) => "已完成",
        };
        let heading = format!("批量翻译{} · {status} · {} / {} 篇", batch.field.label(), batch.done(), batch.total);
        let mut summary = format!("新译 {} · 已有译文 {} · 空内容跳过 {} · 失败 {}", batch.translated, batch.cached, batch.skipped, batch.failed);
        if batch.stopped && !running { summary.push_str(&format!(" · 未处理 {}", batch.total - batch.done())) }
        Some(div().flex().flex_col().flex_none().gap(px(6.)).mx(px(16.)).my(px(8.)).p(px(10.)).rounded(px(8.)).bg(Theme::bg_sunken()).border_1().border_color(Theme::line())
            .child(div().flex().items_center().gap(px(8.))
                .when(running, |d| d.child(ui::spinner("batch-translating", 13., Theme::accent())))
                .child(div().flex_1().min_w_0().text_size(px(12.)).child(heading))
                .when(running && !batch.stopped, |d| d.child(ui::ghost_button("translation-stop", None, Some("停止".into())).on_click(self.on(Cmd::StopBatchTranslation, cx))))
                .when(!running, |d| d.child(ui::ghost_button("translation-dismiss", Some("x"), None).on_click(self.on(Cmd::DismissBatchTranslation, cx)))))
            .child(crate::screen::progress_bar(batch.done(), batch.total))
            .child(div().text_size(px(12.)).text_color(Theme::text_secondary()).child(summary))
            .when(!batch.error.is_empty(), |d| d.child(ui::error_line(&format!("最近一次失败：{}；可再次选择文献执行批量翻译。", batch.error))))
            .into_any_element())
    }
}
