//! 设置: a view of its own, with a sidebar of pages (通用, 外观, 快捷键, AI,
//! 网络, 关于) and 返回 at the bottom. AI has three tabs: 模型 (the providers,
//! a list at the left and the one picked at the right), 任务 (which model
//! does what, with its instructions) and 命令 (the person's own slash commands). The library that was open stays open
//! underneath. Every change is saved at once; there is no 保存 button.
//!
//! The layout follows quiet-md's settings (MIT): grouped rows in rounded
//! cards, the title on the left and the control on the right.

use crate::i18n::t;
use crate::app::{Cmd, MenuKind, Workspace, default_root};
use crate::input::{InputEvent, TextInput};
use crate::shortcuts::{self, GROUPS, SHORTCUTS};
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use wisp_core::ai::{self, Protocol};
use wisp_core::provider::{self, Provider};
use wisp_core::libraries;
use wisp_core::settings::{Language, Task};
use wisp_core::update::{self, Release};

const SIDEBAR_W: f32 = 224.;
const COLUMN_W: f32 = 640.;
/// The AI page is wider: 模型 sets its list beside the provider's fields.
const AI_COLUMN_W: f32 = 880.;
/// The list of providers on 模型.
const PROVIDER_LIST_W: f32 = 232.;
/// A model's name on 模型.
const MODEL_W: f32 = 240.;
const INPUT_W: f32 = 320.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Appearance,
    Shortcuts,
    Ai,
    Network,
    About,
}

impl SettingsPage {
    const ALL: [SettingsPage; 6] = [SettingsPage::General, SettingsPage::Appearance, SettingsPage::Shortcuts, SettingsPage::Ai, SettingsPage::Network, SettingsPage::About];

    fn title(self) -> &'static str {
        match self {
            SettingsPage::General => t("通用"),
            SettingsPage::Appearance => t("外观"),
            SettingsPage::Shortcuts => t("快捷键"),
            SettingsPage::Ai => "AI",
            SettingsPage::Network => t("网络"),
            SettingsPage::About => t("关于"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SettingsPage::General => "sliders",
            SettingsPage::Appearance => "palette",
            SettingsPage::Shortcuts => "keyboard",
            SettingsPage::Ai => "sparkle",
            SettingsPage::Network => "link",
            SettingsPage::About => "info",
        }
    }
}

/// The tabs of 设置 › AI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AiTab {
    #[default]
    Models,
    Tasks,
    Commands,
}

/// The open settings view.
pub struct SettingsView {
    pub page: SettingsPage,
    pub ai_tab: AiTab,
    scroll: ScrollHandle,
    /// The provider 模型 shows at the right, as an index into `Settings::providers`.
    provider: usize,
    protocol: Protocol,
    fields: ProviderFields,
    /// The proxy for everything.
    proxy: Entity<TextInput>,
    /// Whose name the comments and tracked changes of a Word export carry.
    author: Entity<TextInput>,
    translation_prompt: Entity<TextInput>,
    /// The command of the person's own whose fields 命令 shows, as an index into `Settings::commands`.
    command: Option<(usize, CommandFields)>,
    /// The shortcut waiting for its new key. While it waits, every key
    /// binding is cleared, so the keys pressed reach `record_key`.
    recording: Option<String>,
    /// A shortcut and why the key pressed for it was refused.
    shortcut_error: Option<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// The provider on show on 模型.
    pub(crate) fn provider(&self) -> usize {
        self.provider
    }
}

/// The text fields of one provider and of each of its models. They are made
/// anew for the provider on show, so a change always belongs to that one.
struct ProviderFields {
    name: Entity<TextInput>,
    base_url: Entity<TextInput>,
    api_key: Entity<TextInput>,
    /// The service's own proxy.
    proxy: Entity<TextInput>,
    /// The name of each model, in the order of `Provider::models`.
    models: Vec<Entity<TextInput>>,
    /// The name of the model to add.
    new_model: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

impl ProviderFields {
    fn new(provider: &Provider, cx: &mut Context<Workspace>) -> ProviderFields {
        let mut input = |placeholder: SharedString, text: &str| cx.new(|cx| TextInput::new(placeholder, cx).with_text(text));
        let preset = provider.preset();
        let name = input(preset.map_or_else(|| t("服务名称").into(), |p| p.name.into()), &provider.name);
        let base_url = input("https://…/v1".into(), &provider.base_url);
        let api_key = input(tf!("留空则读取环境变量 {}", "Leave blank to read {}", provider.key_env()).into(), &provider.api_key);
        let proxy = input(t("留空则使用全局代理").into(), &provider.proxy);
        let models: Vec<Entity<TextInput>> = provider.models.iter().map(|model| input(t("模型名称").into(), &model.model)).collect();
        let example = preset.and_then(|p| p.models.first().copied()).unwrap_or("gpt-5.5");
        let new_model = input(tf!("添加模型，例如 {example}", "Add a model, e.g. {example}").into(), "");
        api_key.update(cx, |input, _| input.masked = true);
        let mut subscriptions: Vec<Subscription> = [&name, &base_url, &api_key, &proxy]
            .into_iter()
            .chain(models.iter())
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Changed { this.save_provider(cx) }
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(&new_model, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.add_model(cx),
            // The button beside it appears once there is something to add.
            InputEvent::Changed => cx.notify(),
            _ => {}
        }));
        ProviderFields { name, base_url, api_key, proxy, models, new_model, _subscriptions: subscriptions }
    }
}

/// The text fields of one command of the person's own, made anew for the one on show.
struct CommandFields {
    name: Entity<TextInput>,
    about: Entity<TextInput>,
    template: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

impl CommandFields {
    fn new(command: &ai::Command, cx: &mut Context<Workspace>) -> CommandFields {
        let name = cx.new(|cx| TextInput::new("style-check", cx).with_text(&command.name));
        let about = cx.new(|cx| TextInput::new("显示在命令列表里的一句说明", cx).with_text(&command.about));
        let template = cx.new(|cx| TextInput::new("要发给模型的话，例如：指出下面这段文字里空泛、重复的表达，逐条说明理由并给出改法，保留数值、术语和引用：{}", cx).multiline(6).with_text(&command.template));
        let subscriptions = [&name, &about, &template]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Changed { this.save_command(cx) }
                })
            })
            .collect();
        CommandFields { name, about, template, _subscriptions: subscriptions }
    }

    /// The name as it would be kept: without the slash, in lower case.
    fn typed_name(&self, cx: &App) -> String {
        self.name.read(cx).text().trim().trim_start_matches('/').to_lowercase()
    }
}

/// What is wrong with the name typed for the `index`-th command, if anything.
fn name_problem(name: &str, index: usize, commands: &[ai::Command]) -> Option<String> {
    ai::command_name_problem(name, commands.iter().enumerate().filter(|(i, _)| *i != index).map(|(_, command)| command.name.as_str()))
}

/// Where the check for a newer release stands.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    Current,
    Available(Release),
    Failed(String),
}

// ---- controls

/// Rows in one rounded card split by hairlines, under an optional caption.
fn group(title: Option<&'static str>, rows: Vec<AnyElement>) -> Div {
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap(px(8.))
        .children(title.map(|t| div().px(px(4.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text_secondary()).child(t)))
        .child(div().flex().flex_col().w_full().rounded(px(12.)).border_1().border_color(Theme::line()).overflow_hidden().children(rows.into_iter().enumerate().map(move |(i, row)| div().flex().flex_col().w_full().when(i + 1 < count, |d| d.border_b_1().border_color(Theme::line())).child(row))))
}

/// One setting: its name and an optional explanation on the left, the control on the right.
fn row(title: impl Into<SharedString>, detail: Option<SharedString>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(24.))
        .w_full()
        .min_h(px(48.))
        .px(px(16.))
        .py(px(10.))
        .child(div().flex().flex_col().flex_1().min_w(px(120.)).gap(px(2.)).child(div().child(title.into())).children(detail.map(|d| div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(d))))
        .child(div().flex().flex_none().max_w_full().items_center().gap(px(8.)).child(control))
}

/// The small print under a group.
fn note(text: &'static str) -> Div {
    div().px(px(4.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(text)
}

fn switch(id: &'static str, on: bool) -> Stateful<Div> {
    div().id(id).flex().flex_none().items_center().w(px(34.)).h(px(20.)).p(px(2.)).rounded_full().cursor_pointer().when(on, |d| d.bg(Theme::accent()).justify_end()).when(!on, |d| d.bg(Theme::groove()).justify_start()).child(div().size(px(16.)).rounded_full().bg(gpui::white()).shadow(theme::shadow_card()))
}

/// A choice among a few: a groove with the chosen one raised.
pub(crate) fn segmented(id: &'static str, choices: Vec<(&'static str, Cmd)>, chosen: usize, this: &Workspace, cx: &mut Context<Workspace>) -> Div {
    div().flex().flex_none().flex_wrap().max_w_full().gap(px(2.)).min_h(px(28.)).p(px(2.)).rounded(px(9.)).bg(Theme::groove()).children(choices.into_iter().enumerate().map(|(i, (label, cmd))| {
        div()
            .id((id, i))
            .flex()
            .items_center()
            .justify_center()
            .h(px(24.))
            .px(px(12.))
            .rounded(px(7.))
            .cursor_pointer()
            .when(i == chosen, |d| d.bg(Theme::segment()).shadow(theme::shadow_card()))
            .when(i != chosen, |d| d.text_color(Theme::text_secondary()).hover(|s| s.text_color(Theme::text())))
            .on_click(this.on(cmd, cx))
            .child(label)
    }))
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    ui::secondary_button(id, label).h(px(28.))
}

/// A text button in the accent (恢复默认 …).
fn link_button(id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
    div().id(id).flex().flex_none().items_center().h(px(26.)).px(px(8.)).rounded(px(7.)).cursor_pointer().text_size(px(12.)).text_color(Theme::accent()).hover(|s| s.bg(Theme::hover())).child(label)
}

/// A shortcut's key; `recording` shows it waiting for the new one.
fn keycap(id: impl Into<ElementId>, key: &str, recording: bool) -> Stateful<Div> {
    let text = if recording { t("按下新的快捷键…").to_string() } else if key.is_empty() { t("未设置").to_string() } else { shortcuts::pretty(key) };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(148.))
        .h(px(28.))
        .px(px(12.))
        .rounded(px(7.))
        .border_1()
        .cursor_pointer()
        .text_size(px(12.))
        .when(recording, |d| d.border_color(Theme::accent()).text_color(Theme::accent()))
        .when(!recording, |d| d.border_color(gpui::transparent_black()).bg(Theme::hover()).hover(|s| s.bg(Theme::pressed())).text_color(if key.is_empty() { Theme::text_tertiary() } else { Theme::text() }))
        .child(text)
}

/// The app icon resampled once to 176px, its size at 88pt on a 2x screen:
/// GPUI samples images bilinearly without mipmaps, so shrinking the large
/// file on the GPU leaves jagged edges.
fn about_icon() -> ImageSource {
    const PATH: &str = "branding/app-icon.png";
    static ICON: LazyLock<Option<Arc<RenderImage>>> = LazyLock::new(|| {
        let bytes = crate::assets::Assets.load(PATH).ok()??;
        let mut small = image::imageops::resize(&image::load_from_memory(&bytes).ok()?.into_rgba8(), 176, 176, image::imageops::FilterType::Lanczos3);
        // GPUI wants BGRA.
        for pixel in small.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Some(Arc::new(RenderImage::new(vec![image::Frame::new(small)])))
    });
    match &*ICON {
        Some(icon) => icon.clone().into(),
        None => PATH.into(),
    }
}

impl Workspace {
    // ---- opening and closing

    pub fn open_settings(&mut self, page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.modal_subscriptions.clear();
        match self.settings_view.as_mut() {
            Some(view) => {
                if view.page != page { view.scroll = ScrollHandle::new() }
                view.page = page;
                view.shortcut_error = None;
            }
            None => {
                // 模型 opens on the provider of the model questions go to.
                let chat = self.settings.profile_for(Task::Chat).map(|p| p.id).unwrap_or_default();
                let index = self.settings.providers.iter().position(|p| p.models.iter().any(|m| m.id == chat)).unwrap_or(0);
                let shown = self.settings.providers.get(index).cloned().unwrap_or_default();
                let fields = ProviderFields::new(&shown, cx);
                let proxy = cx.new(|cx| TextInput::new("http://127.0.0.1:7890", cx).with_text(&self.settings.proxy));
                let author = cx.new(|cx| TextInput::new(wisp_core::settings::Settings::default().author_name(), cx).with_text(&self.settings.author));
                let translation_prompt = cx.new(|cx| TextInput::new("留空则使用默认要求：译成简体中文，保留术语、基因名、物种学名和数字原样，只输出译文。", cx).multiline(3).with_text(&self.settings.translation_prompt));
                let subscriptions = vec![
                    cx.subscribe(&proxy, |this, _, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed { this.save_proxy(cx) }
                    }),
                    cx.subscribe(&author, |this, input, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed {
                            this.settings.author = input.read(cx).text().trim().to_string();
                            this.save_settings(cx);
                        }
                    }),
                    cx.subscribe(&translation_prompt, |this, input, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed {
                            this.settings.translation_prompt = input.read(cx).text().trim().to_string();
                            this.save_settings(cx);
                            cx.notify();
                        }
                    }),
                ];
                self.settings_view = Some(SettingsView { page, ai_tab: AiTab::default(), scroll: ScrollHandle::new(), provider: index, protocol: shown.protocol, fields, proxy, author, translation_prompt, command: None, recording: None, shortcut_error: None, _subscriptions: subscriptions });
            }
        }
        window.focus(&self.focus);
        cx.notify();
    }

    /// Back to where 设置 was opened from.
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.settings_view = None;
        window.focus(&self.focus);
        cx.notify();
    }

    // ---- 通用

    pub fn pick_data_dir(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(t("选择").into()), extensions: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await && let Some(dir) = paths.into_iter().next() {
                this.update(cx, |this, cx| this.set_data_dir(Some(dir), cx)).ok();
            }
        })
        .detach();
    }

    /// Keep the libraries in `dir`; `None` goes back to the default folder.
    /// Nothing is moved: the home page shows what the new folder holds.
    pub fn set_data_dir(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let default = default_root();
        let root = dir.unwrap_or_else(|| default.clone());
        if root == self.root { return }
        if let Err(err) = std::fs::create_dir_all(&root) {
            return self.fail(tf!("无法使用这个目录：{err}", "Cannot use this folder: {err}"), cx);
        }
        // The default folder is stored as "unset", so it follows the user's Documents folder.
        self.settings.data_dir = if root == default { String::new() } else { root.to_string_lossy().to_string() };
        self.root = root;
        // The open library belongs to the old folder.
        self.lib = None;
        theme::set_accent("blue");
        self.libraries = libraries::list(&self.root);
        self.save_settings(cx);
        self.say(tf!("数据目录已改为 {}", "Data folder changed to {}", self.root.display()), cx);
    }

    // ---- 更新

    /// Start the automatic check: shortly after launch, then twice a day, while 自动检查更新 is on.
    pub fn watch_updates(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            loop {
                let alive = this.update(cx, |this, cx| {
                    if this.settings.auto_update { this.check_update(false, cx) }
                });
                if alive.is_err() { break }
                cx.background_executor().timer(Duration::from_secs(12 * 3600)).await;
            }
        })
        .detach();
    }

    /// Ask which release is the newest. `manual` (检查更新) also reports a
    /// failure; the automatic check stays silent except for one toast per
    /// new version.
    pub fn check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update == UpdateState::Checking { return }
        if manual {
            self.update = UpdateState::Checking;
            cx.notify();
        }
        // WISP_PAPER_UPDATE_URL points the check elsewhere (testing).
        let url = std::env::var("WISP_PAPER_UPDATE_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(update::latest_url);
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { wisp_net::latest_release(&url) }).await;
            this.update(cx, |this, cx| {
                match found {
                    Ok(Some(release)) if update::newer(&release.version, env!("CARGO_PKG_VERSION")) => {
                        if !manual && this.update_told.as_ref() != Some(&release.version) {
                            this.say(tf!("Wisp Paper {} 已发布，可在「设置 › 关于」前往下载。", "Wisp Paper {} is available. Download it from Settings › About.", release.version), cx);
                        }
                        this.update_told = Some(release.version.clone());
                        this.update = UpdateState::Available(release);
                    }
                    Ok(_) => this.update = UpdateState::Current,
                    Err(err) if manual => this.update = UpdateState::Failed(err),
                    // A quiet retry at the next check; keep whatever was shown.
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Where the check stands and the one next step.
    fn update_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = tf!("当前版本 {}", "Current version {}", env!("CARGO_PKG_VERSION"));
        let check = |this: &Self, cx: &mut Context<Self>| Some(button("update-check", t("检查更新")).on_click(this.on(Cmd::CheckUpdate, cx)));
        let (title, detail, control): (String, Option<String>, Option<Stateful<Div>>) = match &self.update {
            UpdateState::Idle => (t("软件更新").into(), Some(current), check(self, cx)),
            UpdateState::Checking => (t("正在检查更新…").into(), Some(current), None),
            UpdateState::Current => (t("已是最新版本").into(), Some(current), check(self, cx)),
            UpdateState::Available(release) => (tf!("新版本 {} 可用", "Version {} available", release.version), Some(tf!("{current}。在发布页下载新版本后覆盖安装，文献库和设置不受影响。", "{current}. Download and install the new version from the releases page. Libraries and settings are kept.")), Some(button("update-open", t("前往下载")).on_click(self.on(Cmd::OpenUrl(release.url.clone()), cx)))),
            UpdateState::Failed(err) => (t("检查更新失败").into(), Some(err.clone()), check(self, cx)),
        };
        row(title, detail.map(Into::into), div().children(control)).into_any_element()
    }

    // ---- 外观

    /// Use the light or the dark colours: the chosen one, else the system's.
    pub fn apply_appearance(&mut self, window: &mut Window) {
        let dark = match self.settings.appearance.as_str() {
            "light" => false,
            "dark" => true,
            _ => matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark),
        };
        theme::set_dark(dark);
        window.refresh();
    }

    // ---- 快捷键

    pub fn start_recording(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_mut() else { return };
        view.recording = Some(id);
        view.shortcut_error = None;
        // The keys go to the workspace, and none of them runs anything meanwhile.
        window.focus(&self.focus);
        cx.clear_key_bindings();
        cx.notify();
    }

    /// Stop waiting for a key and put the key bindings back.
    pub fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.settings_view.as_mut().is_some_and(|view| view.recording.take().is_some()) {
            shortcuts::rebuild(&self.settings.shortcuts, cx);
            cx.notify();
        }
    }

    /// A key pressed while a shortcut waits for its new key.
    pub fn record_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.settings_view.as_ref().and_then(|view| view.recording.clone()) else { return };
        cx.stop_propagation();
        let stroke = &event.keystroke;
        if event.is_held || matches!(stroke.key.as_str(), "" | "shift" | "alt" | "ctrl" | "control" | "cmd" | "win" | "super" | "platform" | "fn" | "function") { return }
        if stroke.key == "escape" && !stroke.modifiers.modified() { return self.stop_recording(cx) }
        let key = stroke.unparse();
        match shortcuts::refusal(&id, &key, &self.settings.shortcuts) {
            Some(why) => {
                if let Some(view) = self.settings_view.as_mut() { view.shortcut_error = Some((id, why)) }
            }
            None => self.set_shortcut(&id, Some(key), cx),
        }
        cx.notify();
    }

    /// Give the shortcut a key (an empty one removes it); `None` restores its default.
    pub fn set_shortcut(&mut self, id: &str, key: Option<String>, cx: &mut Context<Self>) {
        match key {
            Some(key) if !shortcuts::is_default(id, &key) => {
                self.settings.shortcuts.insert(id.to_string(), key);
            }
            _ => {
                self.settings.shortcuts.remove(id);
            }
        }
        // A restored default may now collide with a key given to another shortcut meanwhile.
        let key = shortcuts::key_of(id, &self.settings.shortcuts);
        let clash = if key.is_empty() { None } else { shortcuts::refusal(id, &key, &self.settings.shortcuts) };
        if let Some(view) = self.settings_view.as_mut() {
            view.recording = None;
            view.shortcut_error = clash.map(|why| (id.to_string(), why));
        }
        self.save_settings(cx);
        shortcuts::rebuild(&self.settings.shortcuts, cx);
        cx.notify();
    }

    pub fn reset_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.settings.shortcuts.clear();
        if let Some(view) = self.settings_view.as_mut() {
            view.recording = None;
            view.shortcut_error = None;
        }
        self.save_settings(cx);
        shortcuts::rebuild(&self.settings.shortcuts, cx);
        cx.notify();
    }

    // ---- AI

    /// 设置 › AI on one of its tabs, opening 设置 if it is not open.
    pub fn open_ai_tab(&mut self, tab: AiTab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(SettingsPage::Ai, window, cx);
        let Some(view) = self.settings_view.as_mut() else { return };
        if view.ai_tab != tab { view.scroll = ScrollHandle::new() }
        view.ai_tab = tab;
        cx.notify();
    }

    pub fn pick_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_mut() else { return };
        if view.protocol == protocol { return }
        let same_service = view.protocol != Protocol::Anthropic && protocol != Protocol::Anthropic;
        view.protocol = protocol;
        // Chat completions and Responses are two interfaces of one service: its address and models stay.
        if same_service { return self.save_provider(cx) }
        // The other protocol's address would only fail: offer this one's default. The models stay to be renamed.
        let url = match protocol {
            Protocol::Anthropic => ai::ANTHROPIC_BASE,
            Protocol::OpenAI | Protocol::Responses => "https://api.openai.com/v1",
        };
        view.fields.base_url.update(cx, |input, cx| input.set_text(url, cx));
        self.save_provider(cx);
    }

    /// How hard the n-th model of the provider on show thinks.
    pub fn set_effort(&mut self, index: usize, effort: String, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_ref() else { return };
        let Some(model) = self.settings.providers.get_mut(view.provider).and_then(|p| p.models.get_mut(index)) else { return };
        if model.effort == effort { return }
        model.effort = effort;
        self.save_settings(cx);
        cx.notify();
    }

    /// Show a provider's fields at the right of 模型.
    pub fn select_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.settings.providers.len() { return }
        if let Some(view) = self.settings_view.as_mut() { view.ai_tab = AiTab::Models }
        self.show_provider(index, cx);
    }

    fn show_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        let shown = self.settings.providers.get(index).cloned().unwrap_or_default();
        let fields = ProviderFields::new(&shown, cx);
        if let Some(view) = self.settings_view.as_mut() { (view.provider, view.protocol, view.fields) = (index, shown.protocol, fields) }
        cx.notify();
    }

    /// One more provider, from a preset (`""` is a service set up by hand), and its fields on show.
    pub fn add_provider(&mut self, preset: &str, cx: &mut Context<Self>) {
        let provider = provider::preset(preset).map_or_else(|| Provider { name: tf!("服务 {}", "Provider {}", self.settings.providers.len() + 1), base_url: "https://".into(), ..Default::default() }, Provider::from_preset);
        let index = self.settings.add_provider(provider);
        self.save_settings(cx);
        self.show_provider(index, cx);
    }

    pub fn remove_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.settings.providers.len() { return }
        self.settings.remove_provider(index);
        self.save_settings(cx);
        let shown = self.settings_view.as_ref().map_or(0, |view| view.provider);
        self.show_provider(if shown > index { shown - 1 } else { shown.min(self.settings.providers.len().saturating_sub(1)) }, cx);
    }

    /// Add what the field under the models holds as one more model of the provider on show.
    pub fn add_model(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_ref() else { return };
        let (index, name) = (view.provider, view.fields.new_model.read(cx).text().trim().to_string());
        if name.is_empty() { return }
        // The same model twice would only be two rows to tell apart.
        if self.settings.providers.get(index).is_some_and(|p| p.models.iter().any(|m| m.model == name)) {
            return self.fail(tf!("{name} 已经在这个服务的模型里了。", "{name} is already one of this provider's models."), cx);
        }
        if self.settings.add_model(index, &name).is_none() { return }
        self.save_settings(cx);
        // The fields stay as they are, so the one typed in keeps the keyboard for the next model.
        let input = cx.new(|cx| TextInput::new(t("模型名称"), cx).with_text(&name));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if *event == InputEvent::Changed { this.save_provider(cx) }
        });
        let Some(view) = self.settings_view.as_mut() else { return };
        view.fields.models.push(input);
        view.fields._subscriptions.push(subscription);
        view.fields.new_model.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    pub fn remove_model(&mut self, model: usize, cx: &mut Context<Self>) {
        let Some(index) = self.settings_view.as_ref().map(|view| view.provider) else { return };
        self.settings.remove_model(index, model);
        self.save_settings(cx);
        self.show_provider(index, cx);
    }

    pub fn reset_translation_prompt(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.settings_view { view.translation_prompt.update(cx, |input, cx| input.set_text("", cx)) }
        self.settings.translation_prompt.clear();
        self.save_settings(cx);
        cx.notify();
    }

    // ---- 命令

    /// One more command, under a name to be changed, and its fields on show.
    pub fn add_command(&mut self, cx: &mut Context<Self>) {
        let index = self.settings.add_command();
        self.save_settings(cx);
        self.select_command(index, cx);
    }

    /// Show a command's fields under its row; a click on the one already open folds it up again.
    pub fn select_command(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(command) = self.settings.commands.get(index) else { return };
        if let Some(view) = self.settings_view.as_mut() { view.ai_tab = AiTab::Commands }
        let open = self.settings_view.as_ref().and_then(|view| view.command.as_ref()).map(|(open, _)| *open);
        let fields = (open != Some(index)).then(|| (index, CommandFields::new(command, cx)));
        if let Some(view) = self.settings_view.as_mut() { view.command = fields }
        cx.notify();
    }

    pub fn remove_command(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.settings.commands.len() { return }
        self.settings.commands.remove(index);
        if let Some(view) = self.settings_view.as_mut() { view.command = None }
        self.save_settings(cx);
        cx.notify();
    }

    /// Keep what 命令 shows as the command on show. A name that cannot be
    /// used is said beside the field, and the last good one stays.
    fn save_command(&mut self, cx: &mut Context<Self>) {
        let Some((index, fields)) = self.settings_view.as_ref().and_then(|view| view.command.as_ref()) else { return };
        let Some(mut command) = self.settings.commands.get(*index).cloned() else { return };
        let name = fields.typed_name(cx);
        if name_problem(&name, *index, &self.settings.commands).is_none() { command.name = name }
        command.about = fields.about.read(cx).text().trim().to_string();
        command.template = fields.template.read(cx).text().trim().to_string();
        cx.notify();
        if self.settings.commands[*index] == command { return }
        let index = *index;
        self.settings.commands[index] = command;
        self.save_settings(cx);
    }

    /// Keep the global proxy as typed, and put it to use once it is an
    /// address that can work; until then the last good one stays in force.
    fn save_proxy(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.settings_view else { return };
        let address = view.proxy.read(cx).text().trim().to_string();
        cx.notify();
        if wisp_core::settings::proxy_problem(&address, false).is_some() || self.settings.proxy == address { return }
        self.settings.proxy = address;
        wisp_net::set_proxy(&self.settings.proxy);
        self.save_settings(cx);
    }

    /// Keep what 模型 shows as the provider on show. The models keep their
    /// ids and thinking settings; only their names come from the fields.
    fn save_provider(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.settings_view else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let (fields, index) = (&view.fields, view.provider);
        let Some(saved) = self.settings.providers.get(index) else { return };
        let mut provider = saved.clone();
        (provider.name, provider.protocol, provider.base_url, provider.api_key, provider.proxy) = (text(&fields.name), view.protocol, text(&fields.base_url), text(&fields.api_key), text(&fields.proxy));
        for (model, input) in provider.models.iter_mut().zip(&fields.models) { model.model = text(input) }
        cx.notify();
        if *saved == provider { return }
        self.settings.providers[index] = provider;
        self.save_settings(cx);
    }

    // ---- drawing

    pub fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let page = view.page;
        let nav = |id: &'static str, icon: &'static str, label: &'static str, active: bool, cmd: Cmd, this: &Self, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(9.))
                .h(px(32.))
                .px(px(8.))
                .rounded(px(7.))
                .cursor_pointer()
                .when(active, |d| d.bg(Theme::pressed()).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
                .on_click(this.on(cmd, cx))
                .child(ui::icon(icon, 16., if active { Theme::text() } else { Theme::text_secondary() }))
                .child(label)
        };
        let pages: Vec<AnyElement> = SettingsPage::ALL.into_iter().map(|p| nav(p.icon(), p.icon(), p.title(), p == page, Cmd::OpenSettings(p), self, cx).into_any_element()).collect();
        let sidebar = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_W))
            .h_full()
            .px(px(10.))
            .pb(px(10.))
            .pt(px(if ui::custom_titlebar() { 4. } else { 10. }))
            .gap(px(2.))
            .child(ui::title_strip())
            .child(div().px(px(8.)).pt(px(6.)).pb(px(8.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text_secondary()).child(t("设置")))
            .children(pages)
            .child(div().flex_1())
            .child(nav("settings-back", "arrow-left", t("返回"), false, Cmd::CloseSettings, self, cx));

        let body = match page {
            SettingsPage::General => self.general_page(window, cx),
            SettingsPage::Appearance => self.appearance_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(cx),
            SettingsPage::Ai => self.ai_page(window, cx),
            SettingsPage::Network => self.network_page(window, cx),
            SettingsPage::About => self.about_page(cx),
        };
        let changed = page == SettingsPage::Shortcuts && !self.settings.shortcuts.is_empty();
        let heading = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(div().flex_1().min_w_0().px(px(4.)).text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(page.title()))
            .when(changed, |d| d.child(link_button("shortcuts-reset", t("全部恢复默认")).on_click(self.on(Cmd::ResetShortcuts, cx))));
        let content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(Theme::bg_body())
            .border_l_1()
            .border_color(Theme::line())
            .child(ui::title_strip())
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&view.scroll)
                    .child(div().flex().flex_col().items_center().w_full().px(px(32.)).pt(px(12.)).pb(px(64.)).child(div().flex().flex_col().w_full().max_w(px(if page == SettingsPage::Ai { AI_COLUMN_W } else { COLUMN_W })).gap(px(24.)).child(heading).child(body))),
            );
        div().flex().size_full().child(sidebar).child(content).into_any_element()
    }

    /// The width of the page's column in pixels. A wrapping text field needs
    /// to be told its width, since every width above it is a share of its parent's.
    fn column_width(&self, window: &Window) -> f32 {
        let page = self.settings_view.as_ref().map(|view| view.page);
        (f32::from(window.viewport_size().width) - SIDEBAR_W - 65.).min(if page == Some(SettingsPage::Ai) { AI_COLUMN_W } else { COLUMN_W })
    }

    fn general_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let custom = !self.settings.data_dir.trim().is_empty();
        let reveal = if cfg!(target_os = "macos") { t("在访达中显示") } else if cfg!(target_os = "windows") { t("在资源管理器中显示") } else { t("在文件管理器中显示") };
        let folder = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .when(custom, |d| d.child(link_button("data-reset", t("恢复默认")).on_click(self.on(Cmd::ResetDataDir, cx))))
            .child(button("data-reveal", reveal).on_click(self.on(Cmd::RevealDataDir, cx)))
            .child(button("data-pick", t("更改…")).on_click(self.on(Cmd::PickDataDir, cx)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(group(None, vec![row(t("界面语言"), Some(t("立即生效，自动保存。").into()), segmented("language", vec![("中文", Cmd::SetLanguage(Language::Zh)), ("English", Cmd::SetLanguage(Language::En))], usize::from(self.settings.language == Language::En), self, cx)).into_any_element()]))
            .child(
                group(Some(t("数据")), vec![row(t("数据目录"), Some(self.root.display().to_string().into()), folder).into_any_element()])
                    .child(note(t("所有文献库都在这个文件夹下，每个文献库是其中的一个子文件夹。改到别的目录后，首页显示那个目录里的文献库；原目录里的文献库不会被移动。"))),
            )
            .children(self.settings_view.as_ref().map(|view| group(Some(t("写作")), vec![row(t("署名"), Some(t("导出到 Word 的批注和修订用这个名字；留空用这台电脑的用户名。").into()), div().w(px(INPUT_W)).child(ui::field(&view.author, window, cx))).into_any_element()])))
            .when(crate::background::SUPPORTED, |d| {
                let detail = if cfg!(target_os = "macos") { t("关闭窗口后应用留在程序坞中，检索同步、筛选和导入照常进行；点程序坞图标回到窗口，⌘Q 退出。") } else { t("关闭窗口后应用留在通知区域，检索同步、筛选和导入照常进行；点托盘图标回到窗口，在它的菜单里退出。") };
                d.child(group(Some(t("后台")), vec![row(t("关闭窗口后在后台运行"), Some(detail.into()), switch("keep-running", self.settings.keep_running).on_click(self.on(Cmd::ToggleKeepRunning, cx))).into_any_element()]))
            })
            .child(group(
                Some(t("更新")),
                vec![
                    row(t("自动检查更新"), Some(t("只读取一份版本信息，不发送任何数据；新版本由你决定何时下载安装。").into()), switch("auto-update", self.settings.auto_update).on_click(self.on(Cmd::ToggleAutoUpdate, cx))).into_any_element(),
                    self.update_row(cx),
                ],
            ))
            .into_any_element()
    }

    fn appearance_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = match self.settings.appearance.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        };
        let compact = usize::from(self.settings.density == "compact");
        let set = |value: &str| Cmd::SetAppearance(value.to_string());
        let density = |value: &str| Cmd::SetDensity(value.to_string());
        group(
            None,
            vec![
                row(crate::i18n::tc("appearance.theme", "主题"), Some(t("跟随系统时，随系统的浅色 / 深色模式切换。").into()), segmented("appearance", vec![(t("跟随系统"), set("")), (t("浅色"), set("light")), (t("深色"), set("dark"))], mode, self, cx)).into_any_element(),
                row(t("文献列表"), Some(t("紧凑的行更矮，一屏能看到更多文献。").into()), segmented("density", vec![(t("标准"), density("")), (t("紧凑"), density("compact"))], compact, self, cx)).into_any_element(),
            ],
        )
        .child(note(t("标识色属于文献库：在文献库的「编辑名称与主题」里选择，打开文献库后界面随之变色。")))
        .into_any_element()
    }

    fn shortcuts_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let custom = &self.settings.shortcuts;
        let mut groups: Vec<Vec<AnyElement>> = GROUPS.iter().map(|_| vec![]).collect();
        for shortcut in &SHORTCUTS {
            let id = shortcut.id;
            let key = shortcuts::key_of(id, custom);
            let recording = view.recording.as_deref() == Some(id);
            let error = view.shortcut_error.as_ref().filter(|(e, _)| e == id).map(|(_, why)| why.clone());
            let line = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .w_full()
                .min_h(px(28.))
                .child(div().flex_1().min_w_0().child(t(shortcut.label)))
                .when(custom.contains_key(id) && !recording, |d| d.child(link_button(SharedString::from(format!("reset-{id}")), t("恢复默认")).on_click(self.on(Cmd::ResetShortcut(id.into()), cx))))
                .when(!key.is_empty() && !recording, |d| d.child(link_button(SharedString::from(format!("clear-{id}")), t("移除")).invisible().group_hover("shortcut-row", |s| s.visible()).on_click(self.on(Cmd::ClearShortcut(id.into()), cx))))
                .child(keycap(SharedString::from(format!("key-{id}")), &key, recording).on_click(self.on(Cmd::RecordShortcut(id.into()), cx)));
            groups[shortcut.group].push(
                div()
                    .group("shortcut-row")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .w_full()
                    .px(px(16.))
                    .py(px(8.))
                    .child(line)
                    .when(recording, |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("按下新的组合键即可保存 · Esc 取消"))))
                    .children(error.map(|why| div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::danger()).child(why)))
                    .into_any_element(),
            );
        }
        // Esc is not for sale: every dialog and menu relies on it.
        groups[0].push(div().flex().items_center().w_full().px(px(16.)).py(px(8.)).min_h(px(44.)).child(div().flex_1().min_w_0().child(t("关闭对话框和菜单，返回上一层"))).child(div().min_w(px(148.)).text_size(px(12.)).text_color(Theme::text_tertiary()).flex().justify_center().child("Esc")).into_any_element());
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .children(GROUPS.iter().zip(groups).map(|(title, rows)| group(Some(t(title)), rows)))
            .child(note(t("点一个快捷键，再按下新的组合键。文本框里的编辑按键（复制、粘贴、全选等）是固定的。")))
            // The keys of 筛选 are bare letters, and fixed: shown so that they can be found.
            .child(group(Some(t("筛选")), shortcuts::SCREEN_KEYS.iter().map(|(key, what)| div().flex().items_center().w_full().px(px(16.)).py(px(8.)).min_h(px(44.)).child(div().flex_1().min_w_0().child(t(what))).child(div().min_w(px(148.)).text_size(px(12.)).text_color(Theme::text_tertiary()).flex().justify_center().child(*key)).into_any_element()).collect()))
            .child(note(t("筛选页里的按键是固定的：在批量的队列里对点开的那一篇、在逐篇里对当前这一篇、在确认建议里对光标所在的一行生效。正在输入文字时它们不起作用。")))
            .into_any_element()
    }

    fn ai_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let tabs = [(AiTab::Models, crate::i18n::tc("ai.models", "模型")), (AiTab::Tasks, t("任务")), (AiTab::Commands, t("命令"))];
        let bar = div().flex().items_center().gap(px(4.)).children(tabs.into_iter().map(|(tab, label)| {
            let active = view.ai_tab == tab;
            div()
                .id(("ai-tab", tab as usize))
                .flex()
                .items_center()
                .h(px(32.))
                .px(px(14.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(active, |d| d.bg(Theme::pressed()).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()).text_color(Theme::text())))
                .on_click(self.on(Cmd::AiTab(tab), cx))
                .child(label)
        }));
        let body = match view.ai_tab {
            AiTab::Models => self.models_tab(window, cx),
            AiTab::Tasks => self.tasks_tab(window, cx),
            AiTab::Commands => self.commands_tab(window, cx),
        };
        div().flex().flex_col().w_full().gap(px(20.)).child(bar).child(body).into_any_element()
    }

    /// 模型: the providers in a list at the left, the one picked at the right
    /// with its address, key and models.
    fn models_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let settings = &self.settings;
        let providers = &settings.providers;
        let shown = view.provider.min(providers.len().saturating_sub(1));
        let warn = theme::status_colors("uncertain");
        // Which model each task ends up with, to say beside the providers and models what they do.
        let used: Vec<(Task, String)> = Task::ALL.into_iter().filter_map(|task| settings.profile_for(task).map(|p| (task, p.id))).collect();

        let rows: Vec<AnyElement> = providers
            .iter()
            .enumerate()
            .map(|(i, provider)| {
                let active = i == shown;
                let missing = provider.key().is_empty();
                let busy = used.iter().any(|(_, id)| provider.models.iter().any(|m| m.id == *id));
                let about: SharedString = if missing { t("缺 API Key").into() } else { tf!("{} 个模型", "{} models", provider.models.len()).into() };
                div()
                    .id(("provider", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w_full()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(Theme::pressed()))
                    .when(!active, |d| d.hover(|s| s.bg(Theme::hover())))
                    .on_click(self.on(Cmd::SelectProvider(i), cx))
                    .child(provider_tile(&provider.preset, 32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).child(provider.title()))
                            .child(div().truncate().text_size(px(12.)).text_color(if missing { warn.0 } else { Theme::text_tertiary() }).child(about)),
                    )
                    // In use: some task's model is one of its models.
                    .when(busy, |d| d.child(div().flex_none().size(px(7.)).rounded_full().bg(Theme::accent())))
                    .into_any_element()
            })
            .collect();
        let list = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PROVIDER_LIST_W))
            .p(px(6.))
            .gap(px(2.))
            .border_r_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .children(rows)
            .child(
                div()
                    .id("provider-add")
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w_full()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .text_color(Theme::text_secondary())
                    .hover(|s| s.bg(Theme::hover()).text_color(Theme::text()))
                    .on_click(self.on_menu(MenuKind::AddProvider, cx))
                    .child(div().flex().flex_none().items_center().justify_center().size(px(32.)).rounded(px(9.)).border_1().border_dashed().border_color(Theme::line_strong()).child(ui::icon("plus", 15., Theme::text_secondary())))
                    .child(t("添加服务")),
            );

        let detail = match providers.get(shown) {
            Some(provider) => self.provider_detail(provider, &used, window, cx),
            None => div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .p(px(32.))
                .child(ui::icon("sparkle", 22., Theme::text_tertiary()))
                .child(div().font_weight(FontWeight::MEDIUM).child(t("还没有模型服务")))
                .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("在左边添加一个服务，填上它的 API Key。")))
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(8.))
            .child(div().flex().w_full().min_h(px(520.)).rounded(px(12.)).border_1().border_color(Theme::line()).overflow_hidden().child(list).child(detail))
            .child(note(t("一个服务可以提供多个模型，各个任务用哪一个在「任务」里选。修改会自动保存在本机的 settings.json 里。")))
            .into_any_element()
    }

    /// The right of 模型: one provider's settings and its models.
    fn provider_detail(&self, provider: &Provider, used: &[(Task, String)], window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let fields = &view.fields;
        let protocol_name = |protocol: Protocol| match protocol {
            Protocol::Anthropic => "Claude API",
            Protocol::OpenAI => t("OpenAI 兼容"),
            Protocol::Responses => "OpenAI Responses",
        };
        let (privacy, thinking) = match view.protocol {
            Protocol::Anthropic => (t("用到这个服务的模型时，问题和相关文献的记录、笔记、全文页面（筛选时是待判断的文献，翻译时是题目和摘要）会发送给它。API Key 以明文保存在本机的 settings.json 里。"), t("思考：「默认」不发送思考参数，由模型自己决定；其余各档按 effort 发送。「关闭」让模型不思考，适合翻译这样的任务；Claude Opus 5.5 和 Fable 不能关闭思考，对它们改用最低一档。")),
            Protocol::Responses => (t("OpenAI 的 Responses 接口（/responses），地址写到 /v1。对话不会保存在服务端：每次请求带上完整的对话，模型的推理内容以加密形式随之传回。用到这个服务的模型时，问题和相关文献的内容会发送给它。API Key 以明文保存在本机的 settings.json 里。"), t("思考：「默认」不发送推理参数。各档按 reasoning.effort 发送，「极高」「最高」按「高」，「关闭」是 none。模型不认这个参数时服务会报错，改回「默认」即可。")),
            Protocol::OpenAI => (t("适用于任何兼容 chat/completions 的服务，地址写到 /v1。用到这个服务的模型时，问题和相关文献的内容会发送给它。API Key 以明文保存在本机的 settings.json 里。"), t("思考：「默认」不发送思考参数。各档按 reasoning_effort 发送，「极高」「最高」按「高」。「关闭」因服务而异：OpenAI 是 reasoning_effort: none，百炼和硅基流动是 enable_thinking: false，DeepSeek、火山方舟、智谱、Kimi 等是 thinking: disabled。模型不认这个参数时服务会报错，改回「默认」即可。")),
        };
        let input_w = px(INPUT_W - 40.);
        let field = |input: &Entity<TextInput>| div().w(input_w).child(ui::field(input, window, cx));
        let proxy_detail = wisp_core::settings::proxy_problem(fields.proxy.read(cx).text(), true).unwrap_or_else(|| t("留空则使用「网络」页的全局代理；填 direct 表示不走代理。").to_string());
        let key_detail = if !provider.api_key.trim().is_empty() { t("已保存在本机。").to_string() } else if provider.key().is_empty() { tf!("还没有填写，环境变量 {} 也是空的。", "Not set, and {} is empty too.", provider.key_env()) } else { tf!("正在使用环境变量 {}。", "Using environment variable {}.", provider.key_env()) };
        let (name, base_url, api_key, proxy) = (field(&fields.name), field(&fields.base_url), field(&fields.api_key), field(&fields.proxy));
        let settings_rows = [
            row(t("名称"), None, name),
            row(t("接口"), None, segmented("protocol", vec![("Claude API", Cmd::PickProtocol(Protocol::Anthropic)), (t("OpenAI 兼容"), Cmd::PickProtocol(Protocol::OpenAI)), ("Responses", Cmd::PickProtocol(Protocol::Responses))], match view.protocol { Protocol::Anthropic => 0, Protocol::OpenAI => 1, Protocol::Responses => 2 }, self, cx)),
            row(t("服务地址"), None, base_url),
            row("API Key", Some(key_detail.into()), api_key),
            row(t("代理"), Some(proxy_detail.into()), proxy),
        ];
        let console = provider.preset().map(|p| p.console);
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .pt(px(16.))
            .pb(px(12.))
            .child(provider_tile(&provider.preset, 40.))
            .child(div().flex().flex_col().flex_1().min_w_0().gap(px(1.)).child(div().truncate().text_size(px(16.)).line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).child(provider.title())).child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(format!("{} · {}", protocol_name(view.protocol), provider.base_url.trim()))))
            .children(console.map(|url| link_button("provider-console", t("获取 API Key")).on_click(self.on(Cmd::OpenUrl(url.to_string()), cx))));

        // The models, each with its thinking and the tasks it does.
        let models: Vec<AnyElement> = provider
            .models
            .iter()
            .zip(&fields.models)
            .enumerate()
            .map(|(i, (model, input))| {
                let effort = ai::EFFORTS.iter().find(|(key, _)| *key == model.effort).map_or(t("默认"), |(_, name)| t(name));
                let serves: Vec<&str> = used.iter().filter(|(_, id)| *id == model.id).map(|(task, _)| t(task.label())).collect();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .px(px(16.))
                    .py(px(6.))
                    // A field's width has to be given here: under these widths it would be measured as narrow.
                    .child(div().flex_none().w(px(MODEL_W)).child(ui::field(input, window, cx)))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(Theme::accent()).child(serves.join(" · ")))
                    .child(
                        div()
                            .id(("model-effort", i))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(4.))
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(7.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(Theme::text_secondary())
                            .bg(Theme::hover())
                            .hover(|s| s.bg(Theme::pressed()))
                            .on_click(self.on_menu(MenuKind::ModelEffort(i), cx))
                            .child(tf!("思考 · {effort}", "Thinking · {effort}"))
                            .child(ui::icon("chevron-down", 12., Theme::text_tertiary())),
                    )
                    .child(ui::ghost_button(("model-remove", i), Some("x"), None).tooltip(|_, cx| cx.new(|_| crate::write_assist::Tip(t("删除这个模型").to_string())).into()).on_click(self.on(Cmd::RemoveModel(i), cx)))
                    .into_any_element()
            })
            .collect();
        let typed = !fields.new_model.read(cx).text().trim().is_empty();
        let add = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .w_full()
            .px(px(16.))
            .py(px(6.))
            .child(div().flex_none().w(px(MODEL_W)).child(ui::field(&fields.new_model, window, cx)))
            .child(div().flex_1())
            .child(button("model-add", t("添加")).when(!typed, |d| d.opacity(0.5)).on_click(self.on(Cmd::AddModel, cx)));
        let caption = |text: &'static str| div().px(px(16.)).pt(px(18.)).pb(px(6.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text_secondary()).child(text);
        let small = |text: &'static str| div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(text);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(header)
            .children(settings_rows.into_iter().map(|row| row.border_t_1().border_color(Theme::line())))
            .child(div().border_t_1().border_color(Theme::line()).child(caption(t("模型"))))
            .when(provider.models.is_empty(), |d| d.child(div().px(px(16.)).pb(px(4.)).child(small(t("还没有模型：在下面写上模型名称，按 Enter 添加。")))))
            .children(models)
            .child(add)
            .child(div().flex().flex_col().gap(px(6.)).px(px(16.)).pt(px(14.)).pb(px(16.)).child(small(thinking)).child(small(privacy)))
            .child(div().flex_1())
            .child(row(t("删除这个服务"), Some(t("连同它的模型一起删除；用到它们的任务改用默认的模型。").into()), button("provider-remove", t("删除…")).text_color(Theme::danger()).on_click(self.on(Cmd::AskRemoveProvider(view.provider.min(self.settings.providers.len().saturating_sub(1))), cx))).border_t_1().border_color(Theme::line()))
            .into_any_element()
    }

    /// 任务: which model each task uses, and how it is asked.
    fn tasks_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let settings = &self.settings;
        let any = settings.providers.iter().any(|p| p.models.iter().any(|m| !m.model.trim().is_empty()));
        let warn = theme::status_colors("uncertain");
        let pick = |n: usize, task: Task, this: &Self, cx: &mut Context<Self>| {
            let used = settings.profile_for_or_default(task);
            let follows = task != Task::Chat && settings.tasks.get(task).is_empty();
            let shell = div()
                .id(("task-model", n))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(30.))
                .max_w(px(360.))
                .px(px(10.))
                .rounded(px(8.))
                .when(follows, |d| d.child(div().flex_none().text_color(Theme::text_tertiary()).child(t("同助手"))))
                .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(used.title()))
                // What still keeps the model from answering is said where it is chosen.
                .child(match used.missing() {
                    Some(what) => div().min_w_0().truncate().text_size(px(12.)).text_color(warn.0).child(if used.provider.is_empty() { t(what).to_string() } else { format!("{} · {}", used.provider, t(what)) }),
                    None => div().min_w_0().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(used.provider.clone()),
                });
            if any { shell.bg(Theme::hover()).hover(|s| s.bg(Theme::pressed())).cursor_pointer().child(ui::icon("chevron-down", 12., Theme::text_secondary())).on_click(this.on_menu(MenuKind::TaskModel(task), cx)) } else { shell.text_color(Theme::text_secondary()) }
        };
        // A task: its mark, its name and what uses it, and the model it is done with.
        let head = |n: usize, task: Task, icon: &'static str, about: &'static str, this: &Self, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(14.))
                .w_full()
                .px(px(16.))
                .py(px(14.))
                .child(div().flex().flex_none().items_center().justify_center().size(px(34.)).rounded(px(9.)).bg(Theme::accent_soft()).child(ui::icon(icon, 17., Theme::accent())))
                .child(div().flex().flex_col().flex_1().min_w(px(200.)).gap(px(2.)).child(div().font_weight(FontWeight::SEMIBOLD).child(t(task.label()))).child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(t(about))))
                .child(pick(n, task, this, cx))
                .into_any_element()
        };
        const WORKERS: [usize; 6] = [1, 2, 3, 4, 6, 8];
        let workers: Vec<(&'static str, Cmd)> = ["1", "2", "3", "4", "6", "8"].into_iter().zip(WORKERS).map(|(name, count)| (name, Cmd::SetScreenWorkers(count))).collect();
        let sub = |row: Div| row.pl(px(64.)).border_t_1().border_color(Theme::line()).into_any_element();
        // In pixels, less the card's border and padding: the instructions wrap.
        let wide = self.column_width(window) - 34.;
        let prompt = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(Theme::line())
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.))
                    .w(px(wide))
                    .child(div().flex().flex_col().flex_1().gap(px(2.)).child(t("提示词")).child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(t("留空则用默认的要求。可以指定别的目标语言、术语的译法或输出的格式；改了以后，同一段文字按新的要求重新翻译。"))))
                    .when(!settings.translation_prompt.is_empty(), |d| d.child(link_button("translation-reset", t("恢复默认")).on_click(self.on(Cmd::ResetTranslationPrompt, cx)))),
            )
            .child(div().w(px(wide)).child(ui::field(&view.translation_prompt, window, cx)))
            .into_any_element();
        // Each card is given its width in pixels: under widths that are shares, a
        // column guesses the translation card's height at another width and
        // leaves the difference under it.
        let column = self.column_width(window);
        let card = |rows: Vec<AnyElement>| group(None, rows).w(px(column));
        div()
            .flex()
            .flex_col()
            .w(px(column))
            .gap(px(16.))
            .child(card(vec![head(0, Task::Chat, "sparkle", "助手和单篇文献的对话。向导拟检索式、图片库里看图写图注和标签、写作页的改写、润色和配引用也用它；看图需要能读图片的模型。", self, cx)]))
            .child(card(vec![head(1, Task::Screen, "list-todo", "逐篇阅读文献：筛选页的批量建议、复核和单篇再问，以及提取页按词条填写。文献多的时候，可以给它一个更快、更便宜的模型。", self, cx), sub(row(t("同时阅读的篇数"), Some(t("批量筛选时同时发出的请求数。服务限速时调小。").into()), segmented("workers", workers, WORKERS.iter().position(|count| *count == settings.screen_workers).unwrap_or(usize::MAX), self, cx)))]))
            .child(card(vec![head(2, Task::Translate, "scan-text", "翻译题目、摘要和 PDF 里选中的文字；写作页把你写的话变成候选句的检索词也用它。翻译用不着思考：可以给它一个把「思考」设为关闭的小模型。", self, cx), prompt]))
            .child(card(vec![head(3, Task::Structure, "file-text", "写作页的准备工作：把全文按导言、方法、结果、讨论分开，以及读出扫描件页面上的文字（需要能看图的模型）。", self, cx)]))
            .child(note(t("没有单独指定模型的任务，使用助手的模型；助手没有指定时，用第一个填好了 API Key 的模型。译过的文字保存在文献库里，不会再次请求。")))
            .into_any_element()
    }

    fn commands_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let commands = &self.settings.commands;
        // In pixels, as for the translation instructions on 任务: the template wraps.
        let wide = self.column_width(window) - 34.;
        let mut open = view.command.as_ref().map(|(index, fields)| {
            let problem = name_problem(&fields.typed_name(cx), *index, commands);
            let wrong = problem.is_some();
            let field = |input: &Entity<TextInput>| div().w(px(INPUT_W)).child(ui::field(input, window, cx));
            let name_detail = problem.map_or_else(|| tf!("在提问框里输入 /{} 使用。", "Type /{} in the chat input to use it.", commands[*index].name), |why| tf!("{why}现在仍是 /{}。", "{why}The command is still /{}.", commands[*index].name));
            let name = div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(24.))
                .w_full()
                .min_h(px(48.))
                .px(px(16.))
                .py(px(10.))
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(t("名称")).child(div().text_size(px(12.)).line_height(px(17.)).text_color(if wrong { Theme::danger() } else { Theme::text_tertiary() }).child(name_detail)))
                .child(field(&fields.name));
            let template = div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(16.))
                .py(px(12.))
                .child(div().flex().flex_col().gap(px(2.)).w(px(wide)).child(t("模板")).child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(t("发给模型的就是这段话。写上 {} 的地方换成命令后面的内容，这时命令后面必须写内容；不写 {} 的模板可以单独使用，后面写了内容就接在末尾。"))))
                .child(div().w(px(wide)).child(ui::field(&fields.template, window, cx)));
            let about = row(t("说明"), None, field(&fields.about));
            let remove = row(t("删除这个命令"), Some(t("用过它的对话保留着当时发出的内容，不受影响。").into()), button("command-remove", t("删除…")).text_color(Theme::danger()).on_click(self.on(Cmd::AskRemoveCommand(*index), cx)));
            (*index, div().flex().flex_col().w_full().bg(Theme::bg_sunken()).children([name, about, template, remove].into_iter().map(|row| row.border_t_1().border_color(Theme::line()))))
        });
        let mut own: Vec<AnyElement> = commands
            .iter()
            .enumerate()
            .map(|(i, command)| {
                let shown = open.as_ref().is_some_and(|(index, _)| *index == i);
                let about = if command.template.trim().is_empty() { t("还没有写模板").to_string() } else if command.about.is_empty() { command.template.split_whitespace().collect::<Vec<_>>().join(" ") } else { command.about.clone() };
                let head = div()
                    .id(("command", i))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .min_h(px(48.))
                    .px(px(16.))
                    .py(px(9.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .on_click(self.on(Cmd::SelectCommand(i), cx))
                    .child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(Theme::accent()).child(format!("/{}", command.name)))
                    .child(div().flex_1().min_w_0().truncate().text_color(Theme::text_tertiary()).child(about))
                    .child(ui::icon(if shown { "chevron-up" } else { "chevron-down" }, 14., Theme::text_tertiary()));
                div().flex().flex_col().w_full().child(head).children(open.take_if(|_| shown).map(|(_, fields)| fields)).into_any_element()
            })
            .collect();
        own.push(div().flex().items_center().w_full().px(px(16.)).py(px(10.)).child(button("command-add", t("添加命令")).on_click(self.on(Cmd::AddCommand, cx))).into_any_element());
        let builtin = ai::SLASH.iter().map(|command| row(format!("/{}", command.name), Some(t(command.about).into()), div().max_w(px(280.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t(command.hint))).into_any_element()).collect();
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(group(Some(t("我的命令")), own).child(note(t("把常用的提问存成一条命令：在提问框里输入 / 就能选到它，选中后可以接着写内容，按 Enter 才发送。命令只是一段提示：它不运行脚本，也不会让助手多出工具或绕过确认；材料范围、模型和引文核对都和平常提问一样。修改会自动保存在本机的 settings.json 里，保存时不请求模型。"))))
            .child(group(Some(t("内置命令")), builtin).child(note(t("内置命令不能修改，也不能被同名的命令替换。"))))
            .into_any_element()
    }

    fn network_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let problem = wisp_core::settings::proxy_problem(view.proxy.read(cx).text(), false);
        let (state, colors) = match (&problem, self.settings.proxy.trim().is_empty()) {
            (Some(_), _) => (t("地址有误，未启用"), theme::status_colors("uncertain")),
            (None, true) => (t("未设置，跟随系统环境变量"), theme::status_colors("pending")),
            (None, false) => (t("已启用"), theme::status_colors("included")),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(
                group(
                    Some(t("代理")),
                    vec![
                        row(t("全局代理"), Some(t("文献检索、元数据查询、检查更新和模型请求都经过它。修改会自动保存，下一次请求起生效。").into()), ui::chip(state, colors.0, colors.1)).into_any_element(),
                        row(t("代理地址"), Some(problem.unwrap_or_else(|| t("支持 http、https、socks4、socks4a、socks5、socks5h，例如 http://127.0.0.1:7890 或 socks5://127.0.0.1:1080。").to_string()).into()), div().w(px(INPUT_W)).child(ui::field(&view.proxy, window, cx))).into_any_element(),
                    ],
                )
                .child(note(t("留空时使用环境变量 ALL_PROXY、HTTPS_PROXY、HTTP_PROXY（NO_PROXY 同样有效）。模型服务可以在「AI」页单独指定代理，或者填 direct 不走代理。命令行 wisp 使用同一个设置。代理地址连同其中的用户名和密码以明文保存在本机的 settings.json 里。"))),
            )
            .into_any_element()
    }

    fn about_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = |id: &'static str, label: &'static str, url: String, this: &Self, cx: &mut Context<Self>| button(id, label).on_click(this.on(Cmd::OpenUrl(url), cx));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(8.))
                    .child(img(about_icon()).size(px(88.)))
                    .child(div().pt(px(4.)).text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::SEMIBOLD).child("Wisp Paper"))
                    .child(div().text_color(Theme::text_secondary()).child(tf!("版本 {}", "Version {}", env!("CARGO_PKG_VERSION"))))
                    .child(div().text_color(Theme::text_tertiary()).child(t("本地优先的文献库：收集文献、整理、预览 PDF、对文献库提问。"))),
            )
            .child(group(None, vec![self.update_row(cx)]))
            .child(group(
                None,
                vec![
                    row(t("项目主页"), Some("github.com/xuzhougeng/WispPaper".into()), open("about-project", t("打开"), update::PROJECT_URL.to_string(), self, cx)).into_any_element(),
                    row(t("开源许可"), Some(t("Wisp Paper 以 MIT 协议发布；随附的第三方组件见 THIRD_PARTY.md。").into()), open("about-license", t("查看"), format!("{}/blob/main/THIRD_PARTY.md", update::PROJECT_URL), self, cx)).into_any_element(),
                ],
            ))
            .into_any_element()
    }
}

/// A provider's mark: white on a tile of the service's colour.
fn provider_tile(preset: &str, size: f32) -> Div {
    let (top, bottom) = theme::provider_tile(preset);
    let mark = if provider::preset(preset).is_some() { format!("provider-{preset}") } else { "provider-custom".to_string() };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(size * 0.28))
        .bg(linear_gradient(160., linear_color_stop(top, 0.), linear_color_stop(bottom, 1.)))
        .shadow(theme::shadow_card())
        .child(ui::icon(&mark, size * 0.56, gpui::white()))
}
