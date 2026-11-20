//! 对照译文 (#178): the PDF beside itself, the original at the left and at
//! the right the same pages with each paragraph replaced by its translation.
//! The paragraphs are `translation::passages` of the file's glyphs, each with
//! the box it stands in; each is translated on its own through
//! `translate_text`, a few at a time, so it is kept in the library as soon
//! as it is back, a run stopped halfway goes on from there, and what is kept
//! is shown without a model. The viewer draws them (`PdfView::set_facing`)
//! and scrolls its two halves as one.

use crate::app::{Cmd, Tab, Translated, Workspace};
use crate::i18n::t;
use crate::pdf::FacingPiece;
use crate::theme::{Theme, text_px};
use crate::translate::{Run, TranslationRun};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::HashMap;
use wisp_core::ai;
use wisp_core::settings::Task;
use wisp_core::store::translation_key;
use wisp_core::translation::{Batch, Passage};

/// The narrowest viewer that shows a page and its translation side by side; under it the translation stands below.
pub const FACING_MIN: f32 = 640.;

/// A run gives up after this many paragraphs failed with none translated: a missing key fails each the same way.
const GIVE_UP: usize = 3;

/// How the paragraphs of the file stand.
#[derive(PartialEq)]
pub enum Standing {
    Ready,
    /// The file's glyphs are being read in the background.
    Reading,
    /// No paragraphs came of the file.
    Failed,
}

/// The paragraphs of the file on show, with what their translations are kept under.
pub struct Facing {
    /// The file they are of: a resource id.
    pub file: String,
    pub standing: Standing,
    pub passages: Vec<Passage>,
    /// Each paragraph's `translation_key`, under the instructions in force (`Workspace::rekey_facing`).
    pub keys: Vec<String>,
}

impl Workspace {
    /// Keep the paragraphs of the file read while its translation is on show: read them when the switch
    /// is turned on or another file is shown, leave them alone otherwise.
    pub(crate) fn load_facing(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let file = if lib.facing && lib.tab == Tab::Pdf { lib.selected.as_deref().and_then(|id| lib.pdf_file(id)).cloned() } else { None };
        let Some(file) = file else {
            // Put away, or a paper without a PDF: nothing more is asked for; what was asked is still kept.
            (lib.facing_text, lib.facing_run, lib.facing_start) = (None, None, false);
            return self.sync_facing(cx);
        };
        if lib.facing_text.as_ref().is_some_and(|facing| facing.file == file.id) { return self.sync_facing(cx) }
        lib.facing_run = None;
        lib.facing_text = Some(Facing { file: file.id.clone(), standing: Standing::Reading, passages: vec![], keys: vec![] });
        let (library, path, id) = (lib.info.id.clone(), lib.store.file_path(&file), file.id);
        self.sync_facing(cx);
        cx.spawn(async move |this, cx| {
            let passages = cx.background_executor().spawn(async move { std::fs::read(&path).ok().and_then(wisp_core::pdftext::file_marks).map(|pages| wisp_core::translation::passages(&pages)) }).await;
            this.update(cx, |this, cx| {
                // Another file, another library, or the switch turned off since.
                let Some(facing) = this.lib.as_mut().filter(|lib| lib.info.id == library).and_then(|lib| lib.facing_text.as_mut()).filter(|facing| facing.file == id) else { return };
                facing.passages = passages.unwrap_or_default();
                facing.standing = if facing.passages.is_empty() { Standing::Failed } else { Standing::Ready };
                let failed = facing.standing == Standing::Failed;
                this.rekey_facing();
                let start = this.lib.as_mut().is_some_and(|lib| std::mem::take(&mut lib.facing_start));
                if failed { this.fail(t("没有从这个 PDF 里读出成段的文字，无法对照翻译。扫描件要先有文字层。"), cx) } else if start { this.run_facing(cx) }
                this.sync_facing(cx);
                cx.notify();
            }).ok();
        }).detach();
    }

    /// What each paragraph's translation is kept under, with the instructions now in force, and those
    /// an earlier session made: they are there at once, without a model.
    fn rekey_facing(&mut self) {
        let prompt = ai::translation_prompt(&self.settings.translation_prompt).to_string();
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(facing) = lib.facing_text.as_mut() else { return };
        facing.keys = facing.passages.iter().map(|passage| translation_key(&passage.text, &prompt)).collect();
        for key in &facing.keys {
            if !lib.translations.contains_key(key) && let Ok(Some(text)) = lib.store.translation(key) {
                lib.translations.insert(key.clone(), Translated::Done(text));
            }
        }
    }

    /// Hand the viewer the paragraphs as they stand: translated, being asked for, or still the page's own.
    pub(crate) fn sync_facing(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(pdf) = lib.pdf.clone() else { return };
        let pieces = lib.facing_text.as_ref().filter(|facing| pdf.read(cx).resource_id() == facing.file).map(|facing| {
            let mut pages = HashMap::<usize, Vec<FacingPiece>>::new();
            for (passage, key) in facing.passages.iter().zip(&facing.keys) {
                let state = lib.translations.get(key);
                let text = match state { Some(Translated::Done(text)) => Some(SharedString::from(text.clone())), _ => None };
                pages.entry(passage.page - 1).or_default().push(FacingPiece { rect: passage.rect, size: passage.size, text, busy: matches!(state, Some(Translated::Busy)) });
            }
            pages
        });
        if pieces.is_none() && !pdf.read(cx).facing() { return }
        pdf.update(cx, |pdf, cx| pdf.set_facing(pieces, cx));
    }

    /// Show the translation beside the pages and begin translating, or put it away. Put away, nothing more is asked for.
    pub(crate) fn toggle_facing(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.facing = !lib.facing;
        // The click is the request: this paper is translated as soon as its paragraphs are read.
        lib.facing_start = lib.facing;
        if lib.facing && lib.selected.is_some() { lib.tab = Tab::Pdf }
        let ready = lib.facing && lib.facing_text.as_ref().is_some_and(|facing| facing.standing == Standing::Ready);
        self.ensure_pdf(cx);
        if ready { self.run_facing(cx) }
        cx.notify();
    }

    /// Translate the paragraphs that are not translated yet, a few at a time, from the page being read
    /// onwards and then from the start. Those kept already cost nothing: this is also how a run goes on.
    pub(crate) fn run_facing(&mut self, cx: &mut Context<Self>) {
        self.rekey_facing();
        let profile = self.settings.profile_for_or_default(Task::Translate);
        let prompt = self.settings.translation_prompt.clone();
        let workers = self.settings.translate_workers;
        let Some(lib) = self.lib.as_mut() else { return };
        lib.facing_start = false;
        if lib.facing_run.as_ref().is_some_and(|run| run.batch.running()) { return }
        let Some(facing) = lib.facing_text.as_ref().filter(|facing| facing.standing == Standing::Ready) else { return };
        let page = lib.pdf.as_ref().map_or(1, |pdf| pdf.read(cx).current_page());
        let start = facing.passages.iter().position(|passage| passage.page >= page).unwrap_or(0);
        let order = (start..facing.passages.len()).chain(0..start);
        let batch = Batch::texts(order.map(|index| facing.passages[index].text.as_str()), &prompt);
        if batch.total == 0 { return }
        lib.facing_run = Some(TranslationRun { batch, model: profile.labelled(), profile, prompt, workers });
        self.translation_next(Run::Facing, cx);
        self.sync_facing(cx);
        cx.notify();
    }

    pub(crate) fn stop_facing(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = self.lib.as_mut().and_then(|lib| lib.facing_run.as_mut()) { run.batch.stop() }
        cx.notify();
    }

    /// After every answer of the run: one that fails from its first paragraphs on sends nothing more.
    pub(crate) fn facing_gave_up(&mut self) {
        if let Some(run) = self.lib.as_mut().and_then(|lib| lib.facing_run.as_mut()) && run.batch.failed >= GIVE_UP && run.batch.translated == 0 { run.batch.stop() }
    }

    /// Beside the switch on the PDF's bar while it is on: how far the translation is, and the one thing to do next.
    pub(crate) fn facing_controls(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref().filter(|lib| lib.facing) else { return vec![] };
        let Some(facing) = lib.facing_text.as_ref() else { return vec![] };
        let small = |id: &'static str, label: &str| ui::ghost_button(id, None, Some(label.to_string().into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.));
        if facing.standing == Standing::Reading {
            return vec![ui::spinner("facing-reading", 13., Theme::text_tertiary()).into_any_element(), div().flex_none().child(t("正在读取段落…")).into_any_element()];
        }
        let total = facing.keys.len();
        if total == 0 { return vec![] }
        let done = facing.keys.iter().filter(|key| matches!(lib.translations.get(*key), Some(Translated::Done(_)))).count();
        let run = lib.facing_run.as_ref();
        let running = run.is_some_and(|run| run.batch.running());
        let stopping = running && run.is_some_and(|run| run.batch.stopped);
        let error = run.filter(|run| !running && !run.batch.error.is_empty()).map(|run| run.batch.error.clone());
        let with = tf!("{} · 同时 {} 段", "{} · {} paragraphs at a time", run.filter(|_| running).map_or_else(|| self.settings.profile_for_or_default(Task::Translate).labelled(), |run| run.model.clone()), self.settings.translate_workers);
        let mut out = vec![];
        if running { out.push(ui::spinner("facing-running", 13., Theme::accent()).into_any_element()) }
        out.push(div().id("facing-count").flex_none().child(tf!("{done} / {total} 段", "{done} / {total} paragraphs")).tooltip(ui::tip(with)).into_any_element());
        if stopping {
            out.push(div().flex_none().child(t("正在停止…")).into_any_element());
        } else if running {
            out.push(small("facing-stop", t("停止")).on_click(self.on(Cmd::FacingStop, cx)).into_any_element());
        } else if done < total {
            // What went wrong is said where the way on is: on the button that tries again.
            out.push(small("facing-run", if error.is_some() { t("重试") } else if done == 0 { t("翻译全文") } else { t("继续翻译") }).text_color(if error.is_some() { Theme::danger() } else { Theme::accent() }).when_some(error, |d, error| d.tooltip(ui::tip(error))).on_click(self.on(Cmd::FacingRun, cx)).into_any_element());
        }
        out
    }
}
