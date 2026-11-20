//! 设置 › 自定义: what the person adds to the assistant. 连接器 are MCP
//! servers (`wisp_core::connector`, the transports in `wisp_net::mcp`) whose
//! tools the assistant may call beside the library's; 技能 are folders with
//! a `SKILL.md` (`wisp_core::skill`) it reads when a request calls for one.
//! A connector's tool that does not say it only reads is asked about before
//! it runs, like the tools that change the library. A remote connector that
//! wants a sign-in is signed in to in the browser (`wisp_net::oauth`): 连接
//! asked for by the person goes on to the sign-in by itself, a connection at
//! start only says it is wanted.

use crate::app::{Cmd, Modal, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::settings::{button, group, note, row, switch};
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wisp_core::connector::{Connector, Tool};
use wisp_core::skill;
use wisp_net::mcp::Failure;

/// The two independent sub-panels of 设置 › 自定义.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Connectors,
    Skills,
}

/// Where a connector's connection stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Connecting,
    /// Its tools, and where the person signed in when that was in the browser.
    Ready { tools: Vec<Tool>, account: Option<String> },
    /// The server wants the person to sign in in the browser.
    SignIn,
    /// Waiting for the browser; the address it was sent to, once there is one.
    SigningIn { address: Option<String>, stop: Stop },
    Failed(String),
}

/// What stops a sign-in that waits for the browser, shared with the thread that waits.
#[derive(Clone, Debug, Default)]
pub struct Stop(Arc<AtomicBool>);

impl Stop {
    fn stop(&self) {
        self.0.store(true, Ordering::Relaxed)
    }
}

impl PartialEq for Stop {
    fn eq(&self, other: &Stop) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a tool from outside the catalog runs as.
#[derive(Clone, Debug)]
pub enum Outside {
    /// A connector's tool, by the server's name for it.
    Connector(Connector, String),
    /// Answered already: a skill's instructions.
    Said(Result<String, String>),
}

/// Every command of the page, as `Cmd::Custom`.
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    Tab(Tab),
    AddConnector,
    SelectConnector(usize),
    BackToConnectors,
    ToggleConfiguration,
    ToggleTool(String),
    ToggleToolSchema,
    CopyToolSchema(String),
    ToggleConnector(usize),
    Connect(usize),
    /// Sign in to the connector in the browser.
    SignIn(usize),
    CancelSignIn(usize),
    /// Copy the address of the sign-in under way, for another browser.
    CopySignIn(usize),
    SignOut(usize),
    AskRemoveConnector(usize),
    RemoveConnector(usize),
    /// Pick a skill to copy in: a folder with a `SKILL.md`, or one Markdown file.
    ImportSkill { folder: bool },
    ToggleSkill(String),
    OpenSkills,
}

/// The text fields of the connector on show.
pub struct Fields {
    name: Entity<TextInput>,
    target: Entity<TextInput>,
    token: Entity<TextInput>,
    search: Entity<TextInput>,
    configuration: bool,
    tool: Option<String>,
    schema: bool,
    _subscriptions: Vec<Subscription>,
}

impl Fields {
    fn new(connector: &Connector, cx: &mut Context<Workspace>) -> Fields {
        let name = cx.new(|cx| TextInput::new(t("输入 MCP 名称"), cx).with_text(&connector.name));
        let target = cx.new(|cx| TextInput::new(t("输入 MCP 服务地址或本机命令"), cx).with_text(&connector.target));
        let token = cx.new(|cx| TextInput::new(t("没有就留空"), cx).with_text(&connector.token));
        token.update(cx, |input, _| input.masked = true);
        let search = cx.new(|cx| TextInput::new(t("搜索工具名称或说明…"), cx));
        let mut subscriptions: Vec<Subscription> = [&name, &target, &token]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Changed { this.save_connector(cx) }
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if *event == InputEvent::Changed { cx.notify() }
        }));
        Fields { name, target, token, search, configuration: connector.target.trim().is_empty(), tool: None, schema: false, _subscriptions: subscriptions }
    }
}

/// Whether two connectors are one connection: the same target asked with the same token.
fn same(a: &Connector, b: &Connector) -> bool {
    a.target.trim() == b.target.trim() && a.token.trim() == b.token.trim()
}

fn connector_name(connector: &Connector) -> SharedString {
    if connector.name.trim().is_empty() { t("未命名的 MCP").into() } else { connector.name.clone().into() }
}

fn connector_icon() -> Div {
    div().flex().flex_none().items_center().justify_center().size(px(38.)).rounded(px(10.)).bg(Theme::accent_soft()).child(ui::icon("plug", 20., Theme::accent()))
}

/// Send the person's browser to a sign-in page. A scripted run (debug
/// builds) leaves the browser alone and prints where it would have gone.
fn open_in_browser(address: &str, cx: &mut App) {
    #[cfg(debug_assertions)]
    if wisp_core::settings::env("DEV").is_some() {
        eprintln!("sign-in: {address}");
        return;
    }
    cx.open_url(address);
}

impl Workspace {
    fn skills_folder(&self) -> std::path::PathBuf {
        skill::folder(&wisp_core::settings::settings_file())
    }

    /// Read the skills' folder again.
    pub fn load_skills(&mut self) {
        self.skills = skill::list(&self.skills_folder());
    }

    fn connection(&self, connector: &Connector) -> Option<&Connection> {
        self.connections.iter().find(|(known, _)| same(known, connector)).map(|(_, state)| state)
    }

    fn set_connection(&mut self, connector: &Connector, to: Connection) {
        match self.connections.iter_mut().find(|(known, _)| same(known, connector)) {
            Some((_, state)) => *state = to,
            None => self.connections.push((connector.clone(), to)),
        }
    }

    /// Forget the connections of connectors that are not in the settings any
    /// more, and the one of `also`; a sign-in among them stops waiting.
    fn prune(&mut self, also: Option<&Connector>) {
        let wanted = &self.settings.connectors;
        self.connections.retain(|(known, state)| {
            let keep = also.is_none_or(|also| !same(known, also)) && wanted.iter().any(|c| same(c, known));
            if !keep && let Connection::SigningIn { stop, .. } = state { stop.stop() }
            keep
        });
        wisp_net::mcp::keep_only(wanted);
    }

    /// Connect to every connector that is switched on: at start. One that
    /// wants a sign-in says so and waits for the person to ask for it.
    pub fn connect_all(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.settings.connectors.len() {
            if !self.settings.connectors[index].off { self.connect(index, false, cx) }
        }
    }

    /// Connect to a connector anew and list its tools, off the main thread.
    /// Only asked for (a button, the switch, the start): a command half typed
    /// is never run. `asked`: the person asked for it, so a server that wants
    /// a sign-in sends the browser there at once.
    pub fn connect(&mut self, index: usize, asked: bool, cx: &mut Context<Self>) {
        let Some(connector) = self.settings.connectors.get(index).cloned() else { return };
        self.connect_to(connector, asked, cx)
    }

    fn connect_to(&mut self, connector: Connector, asked: bool, cx: &mut Context<Self>) {
        if connector.target.trim().is_empty() || self.connection(&connector) == Some(&Connection::Connecting) { return }
        self.prune(Some(&connector));
        self.connections.push((connector.clone(), Connection::Connecting));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let given = connector.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    wisp_net::mcp::tools(&given).map(|tools| {
                        let account = (given.remote() && given.token.trim().is_empty()).then(|| wisp_net::oauth::account(&given.target)).flatten();
                        (tools, account)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                // Changed or connected again meanwhile: this answer is nobody's.
                let Some((_, state)) = this.connections.iter_mut().find(|(known, state)| same(known, &connector) && *state == Connection::Connecting) else { return };
                *state = match outcome {
                    Ok((tools, account)) => Connection::Ready { tools, account },
                    Err(Failure::SignIn) => Connection::SignIn,
                    Err(Failure::Failed(why)) => Connection::Failed(why),
                };
                if asked && *state == Connection::SignIn { this.sign_in(connector, cx) }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn signing_in(&mut self, connector: &Connector, stop: &Stop) -> Option<&mut Connection> {
        let waiting = |state: &Connection| matches!(state, Connection::SigningIn { stop: own, .. } if own == stop);
        self.connections.iter_mut().find(|(known, state)| same(known, connector) && waiting(state)).map(|(_, state)| state)
    }

    /// Sign in to a connector in the browser. Off the main thread the server
    /// and its authorization server are asked how and the app registers
    /// itself; the browser is sent to the sign-in page, and the answer that
    /// comes back to the loopback is waited for. Then it is connected to.
    pub fn sign_in(&mut self, connector: Connector, cx: &mut Context<Self>) {
        let stop = Stop::default();
        if let Some(Connection::SigningIn { stop: before, .. }) = self.connection(&connector) { before.stop() }
        self.set_connection(&connector, Connection::SigningIn { address: None, stop: stop.clone() });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = connector.target.clone();
            let begun = cx.background_executor().spawn(async move { wisp_net::oauth::begin(&target) }).await;
            let begun = match begun {
                Ok(begun) => begun,
                Err(why) => {
                    this.update(cx, |this, cx| {
                        if let Some(state) = this.signing_in(&connector, &stop) { *state = Connection::Failed(why) }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let address = begun.address().to_string();
            let waiting = this
                .update(cx, |this, cx| {
                    let Some(state) = this.signing_in(&connector, &stop) else { return false };
                    *state = Connection::SigningIn { address: Some(address.clone()), stop: stop.clone() };
                    open_in_browser(&address, cx);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            // Stopped or changed meanwhile: the loopback closes with `begun`.
            if !waiting { return }
            let flag = stop.0.clone();
            let outcome = cx.background_executor().spawn(async move { begun.finish(&flag) }).await;
            this.update(cx, |this, cx| {
                let Some(state) = this.signing_in(&connector, &stop) else { return };
                match outcome {
                    Ok(_) => {
                        this.connect_to(connector, false, cx);
                        // Back from the browser to the window that waited.
                        if wisp_core::settings::env("DEV").is_none() { cx.activate(true) }
                    }
                    Err(why) => *state = Connection::Failed(why),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The tools the assistant has beside the library's: those of the
    /// connectors switched on and connected, and the one that reads a skill.
    pub fn outside_tools(&self) -> Vec<(Tool, Option<Connector>)> {
        let mut tools: Vec<(Tool, Option<Connector>)> = vec![];
        for connector in self.settings.connectors.iter().filter(|c| !c.off) {
            let Some(Connection::Ready { tools: listed, .. }) = self.connection(connector) else { continue };
            for tool in listed {
                // Two connectors of one name: the first one's tool keeps the name.
                if tools.iter().all(|(known, _)| known.name != tool.name) { tools.push((tool.clone(), Some(connector.clone()))) }
            }
        }
        tools.extend(skill::tool(&self.skills_on()).map(|tool| (tool, None)));
        tools
    }

    fn skills_on(&self) -> Vec<skill::Skill> {
        self.skills.iter().filter(|skill| !self.settings.skills_off.contains(&skill.name)).cloned().collect()
    }

    /// What a call of an outside tool runs as, whether the person is asked
    /// first, its line for the window and what the card that asks shows.
    pub fn outside_call(&self, name: &str, args: &Result<serde_json::Value, String>) -> Option<(Outside, bool, String, Vec<String>)> {
        let (tool, connector) = self.outside_tools().into_iter().find(|(tool, _)| tool.name == name)?;
        let empty = serde_json::Value::Null;
        let given = args.as_ref().unwrap_or(&empty);
        let Some(connector) = connector else {
            let wanted = given["name"].as_str().unwrap_or("");
            return Some((Outside::Said(skill::read(&self.skills_on(), wanted)), false, tf!("使用技能：{wanted}", "Using skill: {wanted}"), vec![]));
        };
        let line = format!("{} · {}", connector.name.trim(), wisp_core::tools::describe_call(&tool.tool, given));
        // The arguments as they will be sent, for the card that asks.
        let details: Vec<String> = serde_json::to_string_pretty(given).unwrap_or_default().lines().take(40).map(|line| line.chars().take(200).collect()).collect();
        Some((Outside::Connector(connector, tool.tool), args.is_ok() && !tool.read_only, line, details))
    }

    pub fn run_custom(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Tab(tab) => self.open_custom_tab(tab, window, cx),
            Do::AddConnector => {
                self.settings.connectors.push(Connector::default());
                self.save_settings(cx);
                self.select_connector(self.settings.connectors.len() - 1, cx);
            }
            Do::SelectConnector(index) => self.select_connector(index, cx),
            Do::BackToConnectors => {
                if let Some(view) = self.settings_view.as_mut() {
                    view.connector = None;
                    view.reset_scroll();
                }
                cx.notify();
            }
            Do::ToggleConfiguration => {
                if let Some((_, fields)) = self.settings_view.as_mut().and_then(|view| view.connector.as_mut()) { fields.configuration = !fields.configuration }
                cx.notify();
            }
            Do::ToggleTool(name) => {
                if let Some((_, fields)) = self.settings_view.as_mut().and_then(|view| view.connector.as_mut()) {
                    fields.tool = (fields.tool.as_ref() != Some(&name)).then_some(name);
                    fields.schema = false;
                }
                cx.notify();
            }
            Do::ToggleToolSchema => {
                if let Some((_, fields)) = self.settings_view.as_mut().and_then(|view| view.connector.as_mut()) { fields.schema = !fields.schema }
                cx.notify();
            }
            Do::CopyToolSchema(name) => {
                let Some((index, _)) = self.settings_view.as_ref().and_then(|view| view.connector.as_ref()) else { return };
                let Some(connector) = self.settings.connectors.get(*index) else { return };
                let Some(Connection::Ready { tools, .. }) = self.connection(connector) else { return };
                let Some(tool) = tools.iter().find(|tool| tool.tool == name) else { return };
                cx.write_to_clipboard(ClipboardItem::new_string(serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default()));
                self.say(t("已复制参数结构"), cx);
            }
            Do::ToggleConnector(index) => {
                let Some(connector) = self.settings.connectors.get_mut(index) else { return };
                connector.off = !connector.off;
                let connect = !connector.off && self.connection(&self.settings.connectors[index]).is_none();
                self.save_settings(cx);
                if connect { self.connect(index, false, cx) }
                cx.notify();
            }
            Do::Connect(index) => {
                if let Some(connector) = self.settings.connectors.get_mut(index) { connector.off = false }
                self.save_settings(cx);
                self.connect(index, true, cx);
            }
            Do::SignIn(index) => {
                if let Some(connector) = self.settings.connectors.get_mut(index) { connector.off = false }
                self.save_settings(cx);
                let Some(connector) = self.settings.connectors.get(index).cloned() else { return };
                self.sign_in(connector, cx);
            }
            Do::CancelSignIn(index) => {
                let Some(connector) = self.settings.connectors.get(index).cloned() else { return };
                if let Some(Connection::SigningIn { stop, .. }) = self.connection(&connector) { stop.stop() }
                self.set_connection(&connector, Connection::SignIn);
                cx.notify();
            }
            Do::CopySignIn(index) => {
                let Some(connector) = self.settings.connectors.get(index) else { return };
                let Some(Connection::SigningIn { address: Some(address), .. }) = self.connection(connector) else { return };
                cx.write_to_clipboard(ClipboardItem::new_string(address.clone()));
                self.say(t("已复制登录链接"), cx);
            }
            Do::SignOut(index) => {
                let Some(connector) = self.settings.connectors.get(index).cloned() else { return };
                match wisp_net::oauth::sign_out(&connector.target, false) {
                    Err(why) => self.fail(why, cx),
                    Ok(grant) => {
                        wisp_net::mcp::disconnect(&connector);
                        self.set_connection(&connector, Connection::SignIn);
                        // The server is told too, where it listens: the tokens stop working there as well.
                        if let Some(grant) = grant { cx.background_executor().spawn(async move { wisp_net::oauth::revoke(&grant) }).detach() }
                    }
                }
                cx.notify();
            }
            Do::AskRemoveConnector(index) => {
                if self.settings.connector_is_template(index) { return }
                let Some(connector) = self.settings.connectors.get(index) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除 MCP「{}」？", "Delete MCP \"{}\"?", connector.name), message: t("它的地址和令牌会从 settings.json 里删除，助手不再能使用它的工具。").into(), action: t("删除").into(), cmd: Cmd::Custom(Do::RemoveConnector(index)) });
                cx.notify();
            }
            Do::RemoveConnector(index) => {
                self.run(Cmd::CloseModal, window, cx);
                let Some(removed) = self.settings.remove_connector(index) else { return };
                if let Some(view) = self.settings_view.as_mut() {
                    view.connector = None;
                    view.reset_scroll();
                }
                self.prune(None);
                // Its sign-in goes with it, unless another connector uses the same address.
                let shared = self.settings.connectors.iter().any(|c| wisp_core::oauth::key(&c.target) == wisp_core::oauth::key(&removed.target));
                if removed.remote() && !shared && let Ok(Some(grant)) = wisp_net::oauth::sign_out(&removed.target, true) {
                    cx.background_executor().spawn(async move { wisp_net::oauth::revoke(&grant) }).detach();
                }
                self.save_settings(cx);
                cx.notify();
            }
            Do::ImportSkill { folder } => {
                let picked = cx.prompt_for_paths(PathPromptOptions { files: !folder, directories: folder, multiple: true, prompt: Some(t("导入").into()), extensions: (!folder).then(|| vec!["md".into()]) });
                cx.spawn(async move |this, cx| {
                    let Ok(Ok(Some(paths))) = picked.await else { return };
                    this.update(cx, |this, cx| {
                        let dir = this.skills_folder();
                        let failed: Vec<String> = paths.iter().filter_map(|path| skill::import(&dir, path).err()).collect();
                        this.load_skills();
                        if !failed.is_empty() { this.fail(failed.join(" "), cx) }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Do::ToggleSkill(name) => {
                let off = &mut self.settings.skills_off;
                if off.contains(&name) { off.retain(|known| *known != name) } else { off.push(name) }
                self.save_settings(cx);
                cx.notify();
            }
            Do::OpenSkills => {
                let dir = self.skills_folder();
                let _ = std::fs::create_dir_all(&dir);
                cx.open_with_system(&dir);
            }
        }
    }

    /// Open a connector's detail page. Its configuration and tool browser stay together.
    fn select_connector(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(connector) = self.settings.connectors.get(index) else { return };
        let fields = Fields::new(connector, cx);
        if let Some(view) = self.settings_view.as_mut() {
            view.connector = Some((index, fields));
            view.reset_scroll();
        }
        cx.notify();
    }

    /// Keep what the fields show as the connector on show. Nothing is
    /// connected to by this: a changed connector waits for 连接.
    fn save_connector(&mut self, cx: &mut Context<Self>) {
        let Some((index, fields)) = self.settings_view.as_ref().and_then(|view| view.connector.as_ref()) else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let Some(mut connector) = self.settings.connectors.get(*index).cloned() else { return };
        if self.settings.connector_is_template(*index) {
            connector.token = text(&fields.token);
        } else {
            (connector.name, connector.target, connector.token) = (text(&fields.name), text(&fields.target), text(&fields.token));
        }
        cx.notify();
        if self.settings.connectors[*index] == connector { return }
        let index = *index;
        self.settings.connectors[index] = connector;
        // A sign-in begun for what the fields said before stops waiting.
        self.prune(None);
        self.save_settings(cx);
    }

    /// What the row 浏览器登录 says and offers, for a remote connector without a token of its own.
    fn sign_in_row(&self, index: usize, state: Option<&Connection>, cx: &mut Context<Self>) -> Option<(SharedString, AnyElement)> {
        Some(match state {
            Some(Connection::Ready { account: Some(account), .. }) => (
                tf!("已在 {} 登录。到期前会自动续期。", "Signed in at {}. Renewed before it runs out.", account).into(),
                button("connector-sign-out", t("退出登录")).on_click(self.on(Cmd::Custom(Do::SignOut(index)), cx)).into_any_element(),
            ),
            None | Some(Connection::SignIn | Connection::Failed(_)) => (
                t("这个服务需要在浏览器中登录。登录后，Wisp Study 保存它给的令牌，并在到期前自动续期。").into(),
                ui::primary_button("connector-sign-in", t("在浏览器中登录")).h(text_px(28.)).on_click(self.on(Cmd::Custom(Do::SignIn(index)), cx)).into_any_element(),
            ),
            Some(Connection::SigningIn { address, .. }) => (
                if address.is_some() { t("已在浏览器中打开登录页面，登录完成后会自动连接。浏览器没有打开的话，复制链接到浏览器里打开。") } else { t("正在准备浏览器登录…") }.into(),
                div()
                    .flex()
                    .gap(px(8.))
                    .children(address.is_some().then(|| button("connector-copy-sign-in", t("复制链接")).on_click(self.on(Cmd::Custom(Do::CopySignIn(index)), cx))))
                    .child(button("connector-cancel-sign-in", t("取消")).on_click(self.on(Cmd::Custom(Do::CancelSignIn(index)), cx)))
                    .into_any_element(),
            ),
            _ => return None,
        })
    }

    pub fn customize_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        let tabs = [(Tab::Connectors, "MCP"), (Tab::Skills, "SKILL")];
        let bar = div().flex().items_center().gap(px(4.)).children(tabs.into_iter().map(|(tab, label)| {
            let active = view.custom_tab == tab;
            div()
                .id(("custom-tab", tab as usize))
                .flex()
                .items_center()
                .h(text_px(32.))
                .px(px(14.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(active, |d| d.bg(Theme::pressed()).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()).text_color(Theme::text())))
                .on_click(self.on(Cmd::Custom(Do::Tab(tab)), cx))
                .child(label)
        }));
        let body = match view.custom_tab {
            Tab::Connectors => self.connectors_tab(window, cx),
            Tab::Skills => self.skills_tab(cx),
        };
        div().flex().flex_col().w_full().gap(px(20.)).child(bar).child(body).into_any_element()
    }

    fn connector_status(&self, connector: &Connector) -> (SharedString, Hsla) {
        match self.connection(connector) {
            _ if connector.off => (t("已停用").into(), Theme::text_tertiary()),
            _ if connector.target.trim().is_empty() => (t("待配置").into(), Theme::text_tertiary()),
            None => (t("未连接").into(), Theme::text_tertiary()),
            Some(Connection::Connecting) => (t("正在连接…").into(), Theme::text_tertiary()),
            Some(Connection::Ready { tools, .. }) => (tf!("已连接 · {} 个工具", "Connected · {} tools", tools.len()).into(), Theme::success()),
            Some(Connection::SignIn) => (t("需要在浏览器中登录").into(), Theme::warning()),
            Some(Connection::SigningIn { .. }) => (t("正在等待浏览器中的登录…").into(), Theme::text_tertiary()),
            Some(Connection::Failed(why)) => (why.clone().into(), Theme::danger()),
        }
    }

    fn connectors_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.settings_view else { return div().into_any_element() };
        if let Some((index, fields)) = &view.connector && let Some(connector) = self.settings.connectors.get(*index) {
            return self.connector_detail(*index, connector, fields, window, cx);
        }
        let connectors: Vec<AnyElement> = self.settings.connectors.iter().enumerate().map(|(i, connector)| {
            let (status, color) = self.connector_status(connector);
            div().flex().flex_none().items_center().w_full().gap(px(16.)).pr(px(20.))
                .child(div().id(("connector", i)).flex().items_center().flex_1().min_w_0().gap(px(14.)).p(px(18.))
                    .cursor_pointer().hover(|s| s.bg(Theme::hover()))
                    .on_click(self.on(Cmd::Custom(Do::SelectConnector(i)), cx))
                    .child(connector_icon())
                    .child(div().flex().flex_col().flex_1().min_w_0().gap(px(3.))
                        .child(div().flex().items_center().gap(px(8.)).font_weight(FontWeight::MEDIUM).child(connector_name(connector))
                            .when(self.settings.connector_is_template(i), |d| d.child(ui::chip(t("内置模板"), Theme::text_tertiary(), Theme::bg_sunken()))))
                        .child(div().text_size(text_px(12.)).text_color(color).child(status)))
                    .child(ui::icon("chevron-right", 16., Theme::text_tertiary())))
                .child(switch(("connector-on", i), !connector.off).on_click(self.on(Cmd::Custom(Do::ToggleConnector(i)), cx)))
                .into_any_element()
        }).collect();
        div().flex().flex_col().flex_none().w_full().gap(px(18.))
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(format!("MCP ({})", connectors.len())))
                .child(ui::primary_button("connector-add", t("添加 MCP")).on_click(self.on(Cmd::Custom(Do::AddConnector), cx))))
            .when(!connectors.is_empty(), |d| d.child(group(None, connectors)))
            .child(note(t("Notion 是内置示例，可直接登录，也可停用。点击 MCP 查看工具和连接设置。")))
            .into_any_element()
    }

    fn connector_detail(&self, index: usize, connector: &Connector, fields: &Fields, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.connection(connector);
        let template = self.settings.connector_is_template(index);
        let (status, color) = self.connector_status(connector);
        let busy = matches!(state, Some(Connection::Connecting | Connection::SigningIn { .. }));
        let back = ui::ghost_button("connector-back", Some("arrow-left"), Some("MCP".into()))
            .on_click(self.on(Cmd::Custom(Do::BackToConnectors), cx));
        let connect = button("connector-connect", if state.is_some() { t("重新连接") } else { t("连接") })
            .when(busy || connector.target.trim().is_empty(), |d| d.opacity(0.45).cursor_default())
            .when(!busy && !connector.target.trim().is_empty(), |d| d.on_click(self.on(Cmd::Custom(Do::Connect(index)), cx)));
        let header = div().flex().flex_col().gap(px(18.))
            .child(div().flex().items_center().gap(px(8.)).child(back).child(ui::icon("chevron-right", 12., Theme::text_tertiary()))
                .child(div().text_color(Theme::text_secondary()).child(connector_name(connector))))
            .child(div().flex().items_center().gap(px(14.)).child(connector_icon())
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(4.))
                    .child(div().text_size(text_px(22.)).line_height(text_px(28.)).font_weight(FontWeight::SEMIBOLD).child(connector_name(connector)))
                    .child(div().text_size(text_px(12.)).text_color(color).child(status)))
                .child(switch("connector-detail-on", !connector.off).on_click(self.on(Cmd::Custom(Do::ToggleConnector(index)), cx))))
            .when(template, |d| d.child(note(t("Notion 官方 MCP 模板，通过浏览器登录后即可使用。模板不能删除，可用开关停用。"))))
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(ui::chip(if connector.remote() { t("远程 MCP") } else { t("本机命令") }, Theme::text_secondary(), Theme::bg_sunken()))
                .child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(connector.target.clone()))
                .child(connect)
                .child(button("connector-configure", if fields.configuration { t("收起配置") } else { t("连接设置") })
                    .on_click(self.on(Cmd::Custom(Do::ToggleConfiguration), cx))));
        let mut body = div().flex().flex_col().flex_none().w_full().gap(px(22.)).child(header);
        if connector.remote() && connector.token.trim().is_empty() && let Some((detail, control)) = self.sign_in_row(index, state, cx) {
            body = body.child(group(None, vec![row(t("浏览器登录"), Some(detail), control).into_any_element()]));
        }
        if fields.configuration {
            let field = |input: &Entity<TextInput>| div().w(px(340.)).max_w_full().child(ui::field(input, window, cx));
            let token = if connector.remote() { t("OAuth 登录时留空；仅在服务提供 API 令牌时填写。") } else { t("本机命令使用自己的密钥配置。") };
            let mut rows = if template {
                vec![row(t("名称"), None, div().child(connector_name(connector))).into_any_element(), row(t("地址或命令"), None, div().child(connector.target.clone())).into_any_element()]
            } else {
                vec![row(t("名称"), None, field(&fields.name)).into_any_element(), row(t("地址或命令"), Some(t("填写远程 MCP 地址，或在本机运行的命令。").into()), field(&fields.target)).into_any_element()]
            };
            rows.push(row(t("令牌"), Some(token.into()), field(&fields.token)).into_any_element());
            if !template {
                rows.push(row(t("删除 MCP"), Some(t("移除配置并退出此 MCP 的登录。").into()), button("connector-remove", t("删除…")).text_color(Theme::danger()).on_click(self.on(Cmd::Custom(Do::AskRemoveConnector(index)), cx))).into_any_element());
            }
            body = body.child(group(Some(t("连接设置")), rows).child(note(t("登录信息保存在本机设置目录的 oauth.json；手动令牌保存在 settings.json。"))));
        }
        match state {
            Some(Connection::Ready { tools, .. }) => body.child(self.connector_tools(tools, fields, window, cx)).into_any_element(),
            _ => body.child(div().flex().flex_col().gap(px(6.)).p(px(24.)).rounded(px(12.)).bg(Theme::bg_sunken())
                .child(t("连接后查看工具"))
                .child(note(t("成功连接后，这里会列出服务提供的工具；点击工具可查看说明和输入参数。")))).into_any_element(),
        }
    }

    fn connector_tools(&self, tools: &[Tool], fields: &Fields, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let query = fields.search.read(cx).text().trim().to_lowercase();
        let filtered: Vec<&Tool> = tools.iter().filter(|tool| query.is_empty() || tool.tool.to_lowercase().contains(&query) || tool.description.to_lowercase().contains(&query)).collect();
        let count = if query.is_empty() { tf!("工具（{}）", "Tools ({})", tools.len()) } else { tf!("工具（{} / {}）", "Tools ({} / {})", filtered.len(), tools.len()) };
        let rows: Vec<Div> = filtered.iter().enumerate().map(|(i, tool)| {
            let expanded = fields.tool.as_deref() == Some(tool.tool.as_str());
            let header = div().id(SharedString::from(format!("connector-tool-{}", tool.tool))).flex().items_center().gap(px(10.)).w_full().px(px(16.)).py(px(14.))
                .cursor_pointer().hover(|s| s.bg(Theme::hover()))
                .on_click(self.on(Cmd::Custom(Do::ToggleTool(tool.tool.clone())), cx))
                .child(ui::icon(if expanded { "chevron-down" } else { "chevron-right" }, 14., Theme::text_tertiary()))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(tool.tool.clone()))
                .child(ui::chip(if tool.read_only { t("只读") } else { t("使用前确认") }, if tool.read_only { Theme::success() } else { Theme::text_secondary() }, Theme::bg_sunken()));
            div().flex().flex_col().flex_none().w_full()
                .when(i > 0, |d| d.border_t_1().border_color(Theme::line()))
                .child(header)
                .when(expanded, |d| d.child(self.connector_tool_detail(tool, fields.schema, cx)))
        }).collect();
        div().flex().flex_col().flex_none().w_full().gap(px(12.))
            .child(div().flex().flex_wrap().items_center().gap(px(12.))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(count))
                .child(div().w(px(280.)).max_w_full().child(ui::field(&fields.search, window, cx))))
            .child(note(t("展开工具可查看功能说明和输入参数。未声明只读的工具，使用前会请求确认。")))
            .when(!filtered.is_empty(), |d| d.child(div().flex().flex_col().flex_none().w_full().rounded(px(12.)).border_1().border_color(Theme::line()).overflow_hidden().children(rows)))
            .when(filtered.is_empty(), |d| d.child(div().p(px(24.)).text_color(Theme::text_tertiary()).child(if tools.is_empty() { t("这个 MCP 没有提供工具。") } else { t("没有匹配的工具。") })))
            .into_any_element()
    }

    fn connector_tool_detail(&self, tool: &Tool, show_schema: bool, cx: &mut Context<Self>) -> Div {
        let required = tool.input_schema["required"].as_array();
        let properties = tool.input_schema["properties"].as_object();
        let parameters = properties.into_iter().flatten().map(|(name, schema)| {
            let mandatory = required.is_some_and(|values| values.iter().any(|value| value.as_str() == Some(name.as_str())));
            let kind = match &schema["type"] {
                serde_json::Value::String(kind) => kind.clone(),
                serde_json::Value::Array(kinds) => kinds.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(" | "),
                _ => t("复合类型").into(),
            };
            div().flex().flex_col().flex_none().gap(px(4.)).py(px(10.)).border_b_1().border_color(Theme::line())
                .child(div().flex().flex_wrap().items_center().gap(px(8.))
                    .child(div().font_family(crate::theme::MONO_FONT).font_weight(FontWeight::MEDIUM).child(name.clone()))
                    .child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(kind))
                    .child(div().text_size(text_px(12.)).text_color(if mandatory { Theme::warning() } else { Theme::text_tertiary() }).child(if mandatory { t("必填") } else { t("可选") })))
                .when_some(schema["description"].as_str(), |d, description| d.child(div().text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_secondary()).child(description.to_string())))
        });
        div().flex().flex_col().flex_none().w_full().gap(px(14.)).px(px(20.)).pb(px(20.)).pt(px(4.))
            .child(div().text_color(Theme::text_secondary()).line_height(text_px(21.)).child(if tool.description.trim().is_empty() { t("服务未提供工具说明。").into() } else { tool.description.clone() }))
            .child(div().font_weight(FontWeight::MEDIUM).child(t("输入参数")))
            .when(properties.is_some_and(|p| !p.is_empty()), |d| d.child(div().flex().flex_col().flex_none().children(parameters)))
            .when(properties.is_none_or(|p| p.is_empty()), |d| d.child(note(t("没有列出顶层参数；可查看完整参数结构。"))))
            .child(div().flex().flex_wrap().items_center().gap(px(8.))
                .child(button("tool-schema-toggle", if show_schema { t("收起参数结构") } else { t("查看完整参数结构") }).on_click(self.on(Cmd::Custom(Do::ToggleToolSchema), cx)))
                .child(button("tool-schema-copy", t("复制参数结构")).on_click(self.on(Cmd::Custom(Do::CopyToolSchema(tool.tool.clone())), cx))))
            .when(show_schema, |d| {
                let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
                d.child(div().id("connector-tool-schema").w_full().max_h(px(360.)).overflow_scroll().rounded(px(8.)).bg(Theme::bg_sunken()).p(px(12.))
                    .child(div().flex().flex_col().flex_none().font_family(crate::theme::MONO_FONT).text_size(text_px(12.)).line_height(text_px(18.))
                        .children(schema.lines().map(|line| div().flex_none().whitespace_nowrap().child(line.to_string())))))
            })
    }

    fn skills_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut skills: Vec<AnyElement> = self
            .skills
            .iter()
            .enumerate()
            .map(|(i, skill)| {
                let about: SharedString = if skill.description.is_empty() { t("没有写描述，助手不知道什么时候用它").into() } else { skill.description.clone().into() };
                row(skill.name.clone(), Some(about), switch(("skill-on", i), !self.settings.skills_off.contains(&skill.name)).on_click(self.on(Cmd::Custom(Do::ToggleSkill(skill.name.clone())), cx))).into_any_element()
            })
            .collect();
        skills.push(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .w_full()
                .px(px(16.))
                .py(px(10.))
                .child(button("skill-import-folder", t("导入技能文件夹…")).on_click(self.on(Cmd::Custom(Do::ImportSkill { folder: true }), cx)))
                .child(button("skill-import-file", t("导入 Markdown 文件…")).on_click(self.on(Cmd::Custom(Do::ImportSkill { folder: false }), cx)))
                .child(div().flex_1())
                .child(button("skill-open", t("打开技能文件夹")).on_click(self.on(Cmd::Custom(Do::OpenSkills), cx)))
                .into_any_element(),
        );
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.))
            .child(group(None, skills).child(note(t("技能是一个带 SKILL.md 的文件夹，写法和 Claude 的技能相同：开头的 name 和 description 告诉助手它做什么，正文是做法。助手只在请求符合描述时读它。技能只是文字：文件夹里的脚本不会被运行。在文件夹里增删改以后，重新打开这一页就会读到。"))))
            .into_any_element()
    }
}
