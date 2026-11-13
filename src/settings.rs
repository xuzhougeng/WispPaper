//! 设置: a view of its own, with a sidebar of pages (通用, 外观, 快捷键, AI,
//! 关于) and 返回 at the bottom. The library that was open stays open
//! underneath. Every change is saved at once; there is no 保存 button.
//!
//! The layout follows quiet-md's settings (MIT): grouped rows in rounded
//! cards, the title on the left and the control on the right.

use crate::app::{Cmd, MenuKind, Workspace, default_root};
use crate::input::{InputEvent, TextInput};
use crate::shortcuts::{self, GROUPS, SHORTCUTS};
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use wisp_core::ai::{self, Profile, Protocol};
use wisp_core::libraries;
use wisp_core::settings::Task;
use wisp_core::update::{self, Release};

const SIDEBAR_W: f32 = 224.;
const COLUMN_W: f32 = 640.;
const INPUT_W: f32 = 320.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Appearance,
    Shortcuts,
    Ai,
    Commands,
    Network,
    About,
}

impl SettingsPage {
    const ALL: [SettingsPage; 7] = [SettingsPage::General, SettingsPage::Appearance, SettingsPage::Shortcuts, SettingsPage::Ai, SettingsPage::Commands, SettingsPage::Network, SettingsPage::About];

    fn title(self) -> &'static str {
        match self {
            SettingsPage::General => "通用",
            SettingsPage::Appearance => "外观",
            SettingsPage::Shortcuts => "快捷键",
            SettingsPage::Ai => "AI",
            SettingsPage::Commands => "命令",
            SettingsPage::Network => "网络",
            SettingsPage::About => "关于",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SettingsPage::General => "sliders",
            SettingsPage::Appearance => "palette",
            SettingsPage::Shortcuts => "keyboard",
            SettingsPage::Ai => "sparkle",
            SettingsPage::Commands => "list",
            SettingsPage::Network => "link",
            SettingsPage::About => "info",
        }
    }
}

/// The open settings view.
pub struct SettingsView {
    pub page: SettingsPage,
    scroll: ScrollHandle,
    /// The model service the AI page shows the fields of, as an index into `Settings::profiles`.
    editing: usize,
    /// Whether those fields are on show, under the model's row.
    expanded: bool,
    protocol: Protocol,
    /// Its thinking setting, a key of `ai::EFFORTS`.
    effort: String,
    fields: ProfileFields,
    /// The proxy for everything.
    proxy: Entity<TextInput>,
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

/// The text fields of one model service. They are made anew for the
/// service on show, so a change always belongs to that one.
struct ProfileFields {
    name: Entity<TextInput>,
    base_url: Entity<TextInput>,
    model: Entity<TextInput>,
    api_key: Entity<TextInput>,
    /// The service's own proxy.
    proxy: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

impl ProfileFields {
    fn new(profile: &Profile, cx: &mut Context<Workspace>) -> ProfileFields {
        let mut input = |placeholder: &'static str, text: &str| cx.new(|cx| TextInput::new(placeholder, cx).with_text(text));
        let name = input("留空则显示模型名称", &profile.name);
        let base_url = input("https://…", &profile.base_url);
        let model = input("模型名称", &profile.model);
        let api_key = input("留空则读取环境变量", &profile.api_key);
        let proxy = input("留空则使用全局代理", &profile.proxy);
        api_key.update(cx, |input, _| input.masked = true);
        let subscriptions = [&name, &base_url, &model, &api_key, &proxy]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Changed { this.save_profile(cx) }
                })
            })
            .collect();
        ProfileFields { name, base_url, model, api_key, proxy, _subscriptions: subscriptions }
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
        .items_center()
        .justify_between()
        .gap(px(24.))
        .w_full()
        .min_h(px(48.))
        .px(px(16.))
        .py(px(10.))
        .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(div().child(title.into())).children(detail.map(|d| div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(d))))
        .child(div().flex().flex_none().items_center().gap(px(8.)).child(control))
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
    div().flex().flex_none().gap(px(2.)).h(px(28.)).p(px(2.)).rounded(px(9.)).bg(Theme::groove()).children(choices.into_iter().enumerate().map(|(i, (label, cmd))| {
        div()
            .id((id, i))
            .flex()
            .items_center()
            .justify_center()
            .h_full()
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
    let text = if recording { "按下新的快捷键…".to_string() } else if key.is_empty() { "未设置".to_string() } else { shortcuts::pretty(key) };
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
                // The AI page opens on the model questions go to.
                let profile = self.settings.profile_for_or_default(Task::Chat);
                let editing = self.settings.profiles.iter().position(|p| p.id == profile.id).unwrap_or(0);
                let fields = ProfileFields::new(&profile, cx);
                let proxy = cx.new(|cx| TextInput::new("http://127.0.0.1:7890", cx).with_text(&self.settings.proxy));
                let translation_prompt = cx.new(|cx| TextInput::new("留空则使用默认要求：译成简体中文，保留术语、基因名、物种学名和数字原样，只输出译文。", cx).multiline(3).with_text(&self.settings.translation_prompt));
                let subscriptions = vec![
                    cx.subscribe(&proxy, |this, _, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed { this.save_proxy(cx) }
                    }),
                    cx.subscribe(&translation_prompt, |this, input, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed {
                            this.settings.translation_prompt = input.read(cx).text().trim().to_string();
                            this.save_settings(cx);
                            cx.notify();
                        }
                    }),
                ];
                // A model that cannot answer yet opens with its fields showing.
                let expanded = profile.missing().is_some();
                self.settings_view = Some(SettingsView { page, scroll: ScrollHandle::new(), editing, expanded, protocol: profile.protocol, effort: profile.effort, fields, proxy, translation_prompt, command: None, recording: None, shortcut_error: None, _subscriptions: subscriptions });
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
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("选择".into()), extensions: None });
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
            return self.fail(format!("无法使用这个目录：{err}"), cx);
        }
        // The default folder is stored as "unset", so it follows the user's Documents folder.
        self.settings.data_dir = if root == default { String::new() } else { root.to_string_lossy().to_string() };
        self.root = root;
        // The open library belongs to the old folder.
        self.lib = None;
        theme::set_accent("blue");
        self.libraries = libraries::list(&self.root);
        self.save_settings(cx);
        self.say(format!("数据目录已改为 {}", self.root.display()), cx);
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
                            this.say(format!("Wisp Paper {} 已发布，可在「设置 › 关于」前往下载。", release.version), cx);
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
        let current = format!("当前版本 {}", env!("CARGO_PKG_VERSION"));
        let check = |this: &Self, cx: &mut Context<Self>| Some(button("update-check", "检查更新").on_click(this.on(Cmd::CheckUpdate, cx)));
        let (title, detail, control): (String, Option<String>, Option<Stateful<Div>>) = match &self.update {
            UpdateState::Idle => ("软件更新".into(), Some(current), check(self, cx)),
            UpdateState::Checking => ("正在检查更新…".into(), Some(current), None),
            UpdateState::Current => ("已是最新版本".into(), Some(current), check(self, cx)),
            UpdateState::Available(release) => (format!("新版本 {} 可用", release.version), Some(format!("{current}。在发布页下载新版本后覆盖安装，文献库和设置不受影响。")), Some(button("update-open", "前往下载").on_click(self.on(Cmd::OpenUrl(release.url.clone()), cx)))),
            UpdateState::Failed(err) => ("检查更新失败".into(), Some(err.clone()), check(self, cx)),
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

    pub fn pick_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_mut() else { return };
        if view.protocol == protocol { return }
        let same_service = view.protocol != Protocol::Anthropic && protocol != Protocol::Anthropic;
        view.protocol = protocol;
        // Chat completions and Responses are two interfaces of one service: its address and model stay.
        if same_service { return self.save_profile(cx) }
        // The other protocol's address and model would only fail: offer this one's defaults.
        let (url, model) = match protocol {
            Protocol::Anthropic => (ai::ANTHROPIC_BASE, ai::DEFAULT_MODEL),
            Protocol::OpenAI | Protocol::Responses => ("https://api.openai.com/v1", ""),
        };
        view.fields.base_url.update(cx, |input, cx| input.set_text(url, cx));
        view.fields.model.update(cx, |input, cx| input.set_text(model, cx));
        self.save_profile(cx);
    }

    pub fn set_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_mut() else { return };
        view.effort = effort;
        self.save_profile(cx);
    }

    /// Show a model service's fields under its row; a click on the one
    /// already open folds it up again.
    pub fn select_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(view) = self.settings_view.as_mut() else { return };
        if index >= self.settings.profiles.len().max(1) { return }
        if view.editing == index {
            view.expanded = !view.expanded;
            return cx.notify();
        }
        self.show_profile(index, cx);
    }

    fn show_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let profile = self.settings.profiles.get(index).cloned().unwrap_or_default();
        let fields = ProfileFields::new(&profile, cx);
        if let Some(view) = self.settings_view.as_mut() { (view.editing, view.expanded, view.protocol, view.effort, view.fields) = (index, true, profile.protocol, profile.effort, fields) }
        cx.notify();
    }

    /// One more model service, starting from the defaults, and its fields on show.
    pub fn add_profile(&mut self, cx: &mut Context<Self>) {
        // The built-in default becomes a saved one first, so that it stays beside the new one.
        if self.settings.profiles.is_empty() { self.settings.add_profile(Profile::default()); }
        let name = format!("模型 {}", self.settings.profiles.len() + 1);
        let index = self.settings.add_profile(Profile { name, ..Default::default() });
        self.save_settings(cx);
        self.show_profile(index, cx);
    }

    pub fn remove_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        // The last one stays: something has to answer.
        if index >= self.settings.profiles.len() || self.settings.profiles.len() < 2 { return }
        self.settings.remove_profile(index);
        self.save_settings(cx);
        let editing = self.settings_view.as_ref().map_or(0, |view| view.editing);
        self.show_profile(if editing > index { editing - 1 } else { editing.min(self.settings.profiles.len() - 1) }, cx);
        if let Some(view) = self.settings_view.as_mut() { view.expanded = false }
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

    /// Keep what the AI page shows as the model service on show.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.settings_view else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let (fields, editing) = (&view.fields, view.editing);
        let id = self.settings.profiles.get(editing).map(|p| p.id.clone()).unwrap_or_default();
        let profile = Profile { id, name: text(&fields.name), protocol: view.protocol, base_url: text(&fields.base_url), api_key: text(&fields.api_key), model: text(&fields.model), effort: view.effort.clone(), proxy: text(&fields.proxy) };
        match self.settings.profiles.get_mut(editing) {
            Some(saved) if *saved == profile => return,
            Some(saved) => *saved = profile,
            // The first change to the built-in default makes it a saved profile.
            None if profile == Profile::default() => return,
            None => {
                self.settings.add_profile(profile);
            }
        }
        self.save_settings(cx);
        cx.notify();
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
            .child(div().px(px(8.)).pt(px(6.)).pb(px(8.)).font_weight(FontWeight::SEMIBOLD).text_color(Theme::text_secondary()).child("设置"))
            .children(pages)
            .child(div().flex_1())
            .child(nav("settings-back", "arrow-left", "返回", false, Cmd::CloseSettings, self, cx));

        let body = match page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Appearance => self.appearance_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(cx),
            SettingsPage::Ai => self.ai_page(window, cx),
            SettingsPage::Commands => self.commands_page(window, cx),
            SettingsPage::Network => self.network_page(window, cx),
            SettingsPage::About => self.about_page(cx),
        };
        let changed = page == SettingsPage::Shortcuts && !self.settings.shortcuts.is_empty();
        let heading = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(div().flex_1().min_w_0().px(px(4.)).text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(page.title()))
            .when(changed, |d| d.child(link_button("shortcuts-reset", "全部恢复默认").on_click(self.on(Cmd::ResetShortcuts, cx))));
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
                    .child(div().flex().flex_col().items_center().w_full().px(px(32.)).pt(px(12.)).pb(px(64.)).child(div().flex().flex_col().w_full().max_w(px(COLUMN_W)).gap(px(24.)).child(heading).child(body))),
            );
        div().flex().size_full().child(sidebar).child(content).into_any_element()
    }

    fn general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let custom = !self.settings.data_dir.trim().is_empty();
        let reveal = if cfg!(target_os = "macos") { "在访达中显示" } else if cfg!(target_os = "windows") { "在资源管理器中显示" } else { "在文件管理器中显示" };
        let folder = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .when(custom, |d| d.child(link_button("data-reset", "恢复默认").on_click(self.on(Cmd::ResetDataDir, cx))))
            .child(button("data-reveal", reveal).on_click(self.on(Cmd::RevealDataDir, cx)))
            .child(button("data-pick", "更改…").on_click(self.on(Cmd::PickDataDir, cx)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(
                group(Some("数据"), vec![row("数据目录", Some(self.root.display().to_string().into()), folder).into_any_element()])
                    .child(note("所有文献库都在这个文件夹下，每个文献库是其中的一个子文件夹。改到别的目录后，首页显示那个目录里的文献库；原目录里的文献库不会被移动。")),
            )
            .child(group(
                Some("更新"),
                vec![
                    row("自动检查更新", Some("只读取一份版本信息，不发送任何数据；新版本由你决定何时下载安装。".into()), switch("auto-update", self.settings.auto_update).on_click(self.on(Cmd::ToggleAutoUpdate, cx))).into_any_element(),
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
                row("主题", Some("跟随系统时，随系统的浅色 / 深色模式切换。".into()), segmented("appearance", vec![("跟随系统", set("")), ("浅色", set("light")), ("深色", set("dark"))], mode, self, cx)).into_any_element(),
                row("文献列表", Some("紧凑的行更矮，一屏能看到更多文献。".into()), segmented("density", vec![("标准", density("")), ("紧凑", density("compact"))], compact, self, cx)).into_any_element(),
            ],
        )
        .child(note("标识色属于文献库：在文献库的「编辑名称与主题」里选择，打开文献库后界面随之变色。"))
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
                .child(div().flex_1().min_w_0().child(shortcut.label))
                .when(custom.contains_key(id) && !recording, |d| d.child(link_button(SharedString::from(format!("reset-{id}")), "恢复默认").on_click(self.on(Cmd::ResetShortcut(id.into()), cx))))
                .when(!key.is_empty() && !recording, |d| d.child(link_button(SharedString::from(format!("clear-{id}")), "移除").invisible().group_hover("shortcut-row", |s| s.visible()).on_click(self.on(Cmd::ClearShortcut(id.into()), cx))))
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
                    .when(recording, |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child("按下新的组合键即可保存 · Esc 取消")))
                    .children(error.map(|why| div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::danger()).child(why)))
                    .into_any_element(),
            );
        }
        // Esc is not for sale: every dialog and menu relies on it.
        groups[0].push(div().flex().items_center().w_full().px(px(16.)).py(px(8.)).min_h(px(44.)).child(div().flex_1().min_w_0().child("关闭对话框和菜单，返回上一层")).child(div().min_w(px(148.)).text_size(px(12.)).text_color(Theme::text_tertiary()).flex().justify_center().child("Esc")).into_any_element());
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .children(GROUPS.iter().zip(groups).map(|(title, rows)| group(Some(*title), rows)))
            .child(note("点一个快捷键，再按下新的组合键。文本框里的编辑按键（复制、粘贴、全选等）是固定的。"))
            // The keys of 筛选 are bare letters, and fixed: shown so that they can be found.
            .child(group(Some("筛选"), shortcuts::SCREEN_KEYS.iter().map(|(key, what)| div().flex().items_center().w_full().px(px(16.)).py(px(8.)).min_h(px(44.)).child(div().flex_1().min_w_0().child(*what)).child(div().min_w(px(148.)).text_size(px(12.)).text_color(Theme::text_tertiary()).flex().justify_center().child(*key)).into_any_element()).collect()))
            .child(note("筛选页里的按键是固定的：在批量的队列里对点开的那一篇、在逐篇里对当前这一篇、在确认建议里对光标所在的一行生效。正在输入文字时它们不起作用。"))
            .into_any_element()
    }

    fn ai_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let settings = &self.settings;
        // Before anything is saved, the built-in default is the one model there is.
        let builtin = [Profile::default()];
        let profiles: &[Profile] = if settings.profiles.is_empty() { &builtin } else { &settings.profiles };
        let saved = !settings.profiles.is_empty();
        let editing = view.editing.min(profiles.len() - 1);
        let protocol_name = |protocol: Protocol| match protocol {
            Protocol::Anthropic => "Claude API",
            Protocol::OpenAI => "OpenAI 兼容",
            Protocol::Responses => "OpenAI Responses",
        };
        let warn = theme::status_colors("uncertain");

        // The fields of the model that is open, drawn under its row.
        let (variable, privacy) = match view.protocol {
            Protocol::Anthropic => ("ANTHROPIC_API_KEY", "用到这个模型时，问题和相关文献的记录、笔记、全文页面（筛选时是待判断的文献，翻译时是题目和摘要）会发送给这个服务。API Key 以明文保存在本机的 settings.json 里。"),
            Protocol::Responses => ("OPENAI_API_KEY", "OpenAI 的 Responses 接口（/responses），地址写到 /v1。对话不会保存在服务端：每次请求带上完整的对话，模型的推理内容以加密形式随之传回。用到这个模型时，问题和相关文献的内容会发送给这个服务。API Key 以明文保存在本机的 settings.json 里。"),
            Protocol::OpenAI => ("OPENAI_API_KEY", "适用于任何兼容 chat/completions 的服务，地址写到 /v1。用到这个模型时，问题和相关文献的内容会发送给这个服务。API Key 以明文保存在本机的 settings.json 里。"),
        };
        let thinking = match view.protocol {
            Protocol::Anthropic => "思考：「默认」不发送思考参数，由模型自己决定；其余各档按 effort 发送。「关闭」让模型不思考，适合翻译这样的任务；Claude Opus 5.5 和 Fable 不能关闭思考，对它们改用最低一档。",
            Protocol::OpenAI => "思考：「默认」不发送思考参数。各档按 reasoning_effort 发送，「极高」「最高」按「高」。「关闭」因服务而异：OpenAI 是 reasoning_effort: none，百炼和硅基流动是 enable_thinking: false，DeepSeek、火山方舟、智谱、Kimi 等是 thinking: disabled。模型不认这个参数时服务会报错，改回「默认」即可。",
            Protocol::Responses => "思考：「默认」不发送推理参数。各档按 reasoning.effort 发送，「极高」「最高」按「高」，「关闭」是 none。模型不认这个参数时服务会报错，改回「默认」即可。",
        };
        let fields = &view.fields;
        let field = |input: &Entity<TextInput>| div().w(px(INPUT_W)).child(ui::field(input, window, cx));
        let (name, base_url, model, api_key, profile_proxy) = (field(&fields.name), field(&fields.base_url), field(&fields.model), field(&fields.api_key), field(&fields.proxy));
        // A bad address is said where it is typed; the request would only fail later.
        let proxy_detail = wisp_core::settings::proxy_problem(fields.proxy.read(cx).text(), true).unwrap_or_else(|| "只用于这个模型服务。留空则使用「网络」页的全局代理；填 direct 表示不走代理。".to_string());
        let efforts: Vec<(&'static str, Cmd)> = ai::EFFORTS.iter().map(|(key, name)| (*name, Cmd::SetEffort(key.to_string()))).collect();
        let effort = ai::EFFORTS.iter().position(|(key, _)| *key == view.effort).unwrap_or(0);
        let mut service = vec![
            row("名称", None, name),
            row("接口", None, segmented("protocol", vec![("Claude API", Cmd::PickProtocol(Protocol::Anthropic)), ("OpenAI 兼容", Cmd::PickProtocol(Protocol::OpenAI)), ("OpenAI Responses", Cmd::PickProtocol(Protocol::Responses))], match view.protocol { Protocol::Anthropic => 0, Protocol::OpenAI => 1, Protocol::Responses => 2 }, self, cx)),
            row("服务地址", None, base_url),
            row("模型", None, model),
            row("API Key", Some(format!("留空则读取环境变量 {variable}。").into()), api_key),
            row("思考", None, segmented("effort", efforts, effort, self, cx)),
            row("代理", Some(proxy_detail.into()), profile_proxy),
        ];
        if profiles.len() > 1 {
            service.push(row("删除这个模型", Some("用到它的任务改用默认的模型。".into()), button("model-remove", "删除…").text_color(Theme::danger()).on_click(self.on(Cmd::AskRemoveProfile(editing), cx))));
        }
        let small = |text: &'static str| div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(text);
        let mut open = Some(div().flex().flex_col().w_full().bg(Theme::bg_sunken()).children(service.into_iter().map(|row| row.border_t_1().border_color(Theme::line()))).child(div().flex().flex_col().gap(px(6.)).px(px(16.)).py(px(12.)).border_t_1().border_color(Theme::line()).child(small(thinking)).child(small(privacy))));

        // The models, each with the tasks it does; the open one with its fields below.
        let mut models: Vec<AnyElement> = profiles
            .iter()
            .enumerate()
            .map(|(i, profile)| {
                let shown = i == editing && view.expanded;
                let serves = Task::ALL.into_iter().filter(|task| settings.profile_for(*task).is_none_or(|used| used.id == profile.id));
                let effort = ai::EFFORTS.iter().find(|(key, _)| *key == profile.effort).map_or("默认", |(_, name)| *name);
                let about = format!("{} · {} · 思考：{effort}", if profile.model.trim().is_empty() { "未填模型" } else { profile.model.trim() }, protocol_name(profile.protocol));
                let head = div()
                    .id(("model", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .min_h(px(52.))
                    .px(px(16.))
                    .py(px(9.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .on_click(self.on(Cmd::SelectProfile(i), cx))
                    .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(div().truncate().font_weight(FontWeight::MEDIUM).child(profile.title())).child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about)))
                    .children(serves.map(|task| ui::chip(task.label(), Theme::accent(), Theme::accent_soft())))
                    .children(profile.missing().map(|what| ui::chip(what, warn.0, warn.1)))
                    .child(ui::icon(if shown { "chevron-up" } else { "chevron-down" }, 14., Theme::text_tertiary()));
                div().flex().flex_col().w_full().child(head).children(open.take_if(|_| shown)).into_any_element()
            })
            .collect();
        models.push(div().flex().items_center().w_full().px(px(16.)).py(px(10.)).child(button("model-add", "添加模型").on_click(self.on(Cmd::AddProfile, cx))).into_any_element());

        // Which model does what.
        let pick = |n: usize, task: Task, this: &Self, cx: &mut Context<Self>| {
            let used = settings.profile_for_or_default(task).title();
            let text = if task != Task::Chat && settings.tasks.get(task).is_empty() { format!("与提问相同（{used}）") } else { used };
            let shell = div().id(("task-model", n)).flex().flex_none().items_center().gap(px(6.)).h(px(28.)).max_w(px(300.)).px(px(10.)).rounded(px(7.)).child(div().truncate().child(text));
            // With only the built-in default there is nothing to choose from.
            if saved { shell.bg(Theme::hover()).hover(|s| s.bg(Theme::pressed())).cursor_pointer().child(ui::icon("chevron-down", 12., Theme::text_secondary())).on_click(this.on_menu(MenuKind::TaskModel(task), cx)) } else { shell.text_color(Theme::text_secondary()) }
        };
        const WORKERS: [usize; 6] = [1, 2, 3, 4, 6, 8];
        let workers: Vec<(&'static str, Cmd)> = ["1", "2", "3", "4", "6", "8"].into_iter().zip(WORKERS).map(|(name, count)| (name, Cmd::SetScreenWorkers(count))).collect();
        let tasks = vec![
            row(Task::Chat.label(), Some("向文献库或单篇文献提问，以及向导里编写检索式。".into()), pick(0, Task::Chat, self, cx)).into_any_element(),
            row(Task::Screen.label(), Some("筛选页里替你逐篇阅读待判断文献的助手。文献多的时候，可以给它一个更快、更便宜的模型。".into()), pick(1, Task::Screen, self, cx)).into_any_element(),
            row("同时阅读的篇数", Some("批量筛选时同时发出的请求数。服务限速时调小。".into()), segmented("workers", workers, WORKERS.iter().position(|count| *count == settings.screen_workers).unwrap_or(usize::MAX), self, cx)).into_any_element(),
            row(Task::Translate.label(), Some("翻译题目和摘要。翻译用不着思考：可以给它一个把「思考」设为关闭的小模型。".into()), pick(2, Task::Translate, self, cx)).into_any_element(),
            row(Task::Structure.label(), Some("写作页的准备工作：把全文按导言、方法、结果、讨论分开，以及读出扫描件页面上的文字（需要能看图的模型）。".into()), pick(3, Task::Structure, self, cx)).into_any_element(),
        ];
        // The column's width in pixels, less the card's border and padding: a
        // wrapping text field needs to be told its width here, where every
        // width above it is a share of its parent's.
        let wide = (f32::from(window.viewport_size().width) - SIDEBAR_W - 65.).min(COLUMN_W) - 34.;
        let translation = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .child(div().flex().items_center().justify_between().w(px(wide)).min_h(px(26.)).child("翻译要求").when(!settings.translation_prompt.is_empty(), |d| d.child(link_button("translation-reset", "恢复默认").on_click(self.on(Cmd::ResetTranslationPrompt, cx)))))
            .child(div().w(px(wide)).child(ui::field(&view.translation_prompt, window, cx)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(group(Some("模型"), models).child(note("可以放进多个模型，各做各的事：点一个模型展开它的设置，修改会自动保存。下面的「任务」决定谁做什么。")))
            .child(group(Some("任务"), tasks).child(note("没有单独指定模型的任务，使用提问的模型。")))
            .child(group(Some("翻译"), vec![translation.into_any_element()]).child(note("可以在这里指定别的目标语言、术语的译法或输出的格式。要求改了以后，同一段文字会按新的要求重新翻译；译过的文字保存在文献库里，不会再次请求。")))
            .into_any_element()
    }

    fn commands_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let commands = &self.settings.commands;
        // In pixels, as for the translation instructions on the AI page: the template wraps.
        let wide = (f32::from(window.viewport_size().width) - SIDEBAR_W - 65.).min(COLUMN_W) - 34.;
        let mut open = view.command.as_ref().map(|(index, fields)| {
            let problem = name_problem(&fields.typed_name(cx), *index, commands);
            let wrong = problem.is_some();
            let field = |input: &Entity<TextInput>| div().w(px(INPUT_W)).child(ui::field(input, window, cx));
            let name_detail = problem.map_or_else(|| format!("在提问框里输入 /{} 使用。", commands[*index].name), |why| format!("{why}现在仍是 /{}。", commands[*index].name));
            let name = div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(24.))
                .w_full()
                .min_h(px(48.))
                .px(px(16.))
                .py(px(10.))
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child("名称").child(div().text_size(px(12.)).line_height(px(17.)).text_color(if wrong { Theme::danger() } else { Theme::text_tertiary() }).child(name_detail)))
                .child(field(&fields.name));
            let template = div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(16.))
                .py(px(12.))
                .child(div().flex().flex_col().gap(px(2.)).w(px(wide)).child("模板").child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child("发给模型的就是这段话。写上 {} 的地方换成命令后面的内容，这时命令后面必须写内容；不写 {} 的模板可以单独使用，后面写了内容就接在末尾。")))
                .child(div().w(px(wide)).child(ui::field(&fields.template, window, cx)));
            let about = row("说明", None, field(&fields.about));
            let remove = row("删除这个命令", Some("用过它的对话保留着当时发出的内容，不受影响。".into()), button("command-remove", "删除…").text_color(Theme::danger()).on_click(self.on(Cmd::AskRemoveCommand(*index), cx)));
            (*index, div().flex().flex_col().w_full().bg(Theme::bg_sunken()).children([name, about, template, remove].into_iter().map(|row| row.border_t_1().border_color(Theme::line()))))
        });
        let mut own: Vec<AnyElement> = commands
            .iter()
            .enumerate()
            .map(|(i, command)| {
                let shown = open.as_ref().is_some_and(|(index, _)| *index == i);
                let about = if command.template.trim().is_empty() { "还没有写模板".to_string() } else if command.about.is_empty() { command.template.split_whitespace().collect::<Vec<_>>().join(" ") } else { command.about.clone() };
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
        own.push(div().flex().items_center().w_full().px(px(16.)).py(px(10.)).child(button("command-add", "添加命令").on_click(self.on(Cmd::AddCommand, cx))).into_any_element());
        let builtin = ai::SLASH.iter().map(|command| row(format!("/{}", command.name), Some(command.about.into()), div().max_w(px(280.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(command.hint)).into_any_element()).collect();
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(group(Some("我的命令"), own).child(note("把常用的提问存成一条命令：在提问框里输入 / 就能选到它，选中后可以接着写内容，按 Enter 才发送。命令只是一段提示：它不运行脚本，也不会让助手多出工具或绕过确认；材料范围、模型和引文核对都和平常提问一样。修改会自动保存在本机的 settings.json 里，保存时不请求模型。")))
            .child(group(Some("内置命令"), builtin).child(note("内置命令不能修改，也不能被同名的命令替换。")))
            .into_any_element()
    }

    fn network_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let problem = wisp_core::settings::proxy_problem(view.proxy.read(cx).text(), false);
        let (state, colors) = match (&problem, self.settings.proxy.trim().is_empty()) {
            (Some(_), _) => ("地址有误，未启用", theme::status_colors("uncertain")),
            (None, true) => ("未设置，跟随系统环境变量", theme::status_colors("pending")),
            (None, false) => ("已启用", theme::status_colors("included")),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(
                group(
                    Some("代理"),
                    vec![
                        row("全局代理", Some("文献检索、元数据查询、检查更新和模型请求都经过它。修改会自动保存，下一次请求起生效。".into()), ui::chip(state, colors.0, colors.1)).into_any_element(),
                        row("代理地址", Some(problem.unwrap_or_else(|| "支持 http、https、socks4、socks4a、socks5、socks5h，例如 http://127.0.0.1:7890 或 socks5://127.0.0.1:1080。".to_string()).into()), div().w(px(INPUT_W)).child(ui::field(&view.proxy, window, cx))).into_any_element(),
                    ],
                )
                .child(note("留空时使用环境变量 ALL_PROXY、HTTPS_PROXY、HTTP_PROXY（NO_PROXY 同样有效）。模型服务可以在「AI」页单独指定代理，或者填 direct 不走代理。命令行 wisp 使用同一个设置。代理地址连同其中的用户名和密码以明文保存在本机的 settings.json 里。")),
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
                    .child(div().text_color(Theme::text_secondary()).child(format!("版本 {}", env!("CARGO_PKG_VERSION"))))
                    .child(div().text_color(Theme::text_tertiary()).child("本地优先的文献库：收集文献、整理、预览 PDF、对文献库提问。")),
            )
            .child(group(None, vec![self.update_row(cx)]))
            .child(group(
                None,
                vec![
                    row("项目主页", Some("github.com/xuzhougeng/WispPaper".into()), open("about-project", "打开", update::PROJECT_URL.to_string(), self, cx)).into_any_element(),
                    row("开源许可", Some("Wisp Paper 以 MIT 协议发布；随附的第三方组件见 THIRD_PARTY.md。".into()), open("about-license", "查看", format!("{}/blob/main/THIRD_PARTY.md", update::PROJECT_URL), self, cx)).into_any_element(),
                ],
            ))
            .into_any_element()
    }
}
