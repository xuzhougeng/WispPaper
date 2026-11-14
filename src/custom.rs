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
    AddConnector,
    SelectConnector(usize),
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
    _subscriptions: Vec<Subscription>,
}

impl Fields {
    fn new(connector: &Connector, cx: &mut Context<Workspace>) -> Fields {
        let name = cx.new(|cx| TextInput::new(t("输入连接器名称"), cx).with_text(&connector.name));
        let target = cx.new(|cx| TextInput::new(t("输入 MCP 服务地址或本机命令"), cx).with_text(&connector.target));
        let token = cx.new(|cx| TextInput::new(t("没有就留空"), cx).with_text(&connector.token));
        let subscriptions = [&name, &target, &token]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Changed { this.save_connector(cx) }
                })
            })
            .collect();
        Fields { name, target, token, _subscriptions: subscriptions }
    }
}

/// Whether two connectors are one connection: the same target asked with the same token.
fn same(a: &Connector, b: &Connector) -> bool {
    a.target.trim() == b.target.trim() && a.token.trim() == b.token.trim()
}

/// Send the person's browser to a sign-in page. A scripted run (debug
/// builds) leaves the browser alone and prints where it would have gone.
fn open_in_browser(address: &str, cx: &mut App) {
    #[cfg(debug_assertions)]
    if std::env::var_os("WISP_PAPER_DEV").is_some() {
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
                        if std::env::var_os("WISP_PAPER_DEV").is_none() { cx.activate(true) }
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
            Do::AddConnector => {
                self.settings.connectors.push(Connector::default());
                self.save_settings(cx);
                self.select_connector(self.settings.connectors.len() - 1, cx);
            }
            Do::SelectConnector(index) => self.select_connector(index, cx),
            Do::ToggleConnector(index) => {
                let Some(connector) = self.settings.connectors.get_mut(index) else { return };
                connector.off = !connector.off;
                let connect = !connector.off && self.connection(&self.settings.connectors[index]).is_none();
                self.save_settings(cx);
                if connect { self.connect(index, false, cx) }
                cx.notify();
            }
            Do::Connect(index) => { if let Some(c) = self.settings.connectors.get(index) && let Some(url) = wisp_net::mcp::pending_notion_login(c) { cx.open_url(&url); } else { self.connect(index, true, cx); } }
            Do::SignIn(index) => {
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
                let Some(connector) = self.settings.connectors.get(index) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除连接器「{}」？", "Delete connector \"{}\"?", connector.name), message: t("它的地址和令牌会从 settings.json 里删除，助手不再能使用它的工具。").into(), action: t("删除").into(), cmd: Cmd::Custom(Do::RemoveConnector(index)) });
                cx.notify();
            }
            Do::RemoveConnector(index) => {
                self.run(Cmd::CloseModal, window, cx);
                if index >= self.settings.connectors.len() { return }
                let removed = self.settings.connectors.remove(index);
                if let Some(view) = self.settings_view.as_mut() { view.connector = None }
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

    /// Show a connector's fields under its row; a click on the one already open folds it up again.
    fn select_connector(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(connector) = self.settings.connectors.get(index) else { return };
        let open = self.settings_view.as_ref().and_then(|view| view.connector.as_ref()).map(|(open, _)| *open);
        let fields = (open != Some(index)).then(|| (index, Fields::new(connector, cx)));
        if let Some(view) = self.settings_view.as_mut() { view.connector = fields }
        cx.notify();
    }

    /// Keep what the fields show as the connector on show. Nothing is
    /// connected to by this: a changed connector waits for 连接.
    fn save_connector(&mut self, cx: &mut Context<Self>) {
        let Some((index, fields)) = self.settings_view.as_ref().and_then(|view| view.connector.as_ref()) else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let Some(mut connector) = self.settings.connectors.get(*index).cloned() else { return };
        (connector.name, connector.target, connector.token) = (text(&fields.name), text(&fields.target), text(&fields.token));
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
        let state = state?;
        Some(match state {
            Connection::Ready { account: Some(account), .. } => (
                tf!("已在 {} 登录。到期前会自动续期。", "Signed in at {}. Renewed before it runs out.", account).into(),
                button("connector-sign-out", t("退出登录")).on_click(self.on(Cmd::Custom(Do::SignOut(index)), cx)).into_any_element(),
            ),
            Connection::SignIn => (
                t("这个服务需要在浏览器中登录。登录后，Wisp Paper 保存它给的令牌，并在到期前自动续期。").into(),
                ui::primary_button("connector-sign-in", t("在浏览器中登录")).h(text_px(28.)).on_click(self.on(Cmd::Custom(Do::SignIn(index)), cx)).into_any_element(),
            ),
            Connection::SigningIn { address, .. } => (
                t("已在浏览器中打开登录页面，登录完成后会自动连接。浏览器没有打开的话，复制链接到浏览器里打开。").into(),
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
        let small = |text: SharedString, color: Hsla| div().text_size(text_px(12.)).line_height(text_px(17.)).text_color(color).child(text);
        let mut connectors: Vec<AnyElement> = self
            .settings
            .connectors
            .iter()
            .enumerate()
            .map(|(i, connector)| {
                let shown = view.connector.as_ref().filter(|(index, _)| *index == i);
                let state = self.connection(connector);
                let notion_target = wisp_net::notion::is_notion_target(&connector.target);
                let (status, color): (SharedString, Hsla) = match state {
                    _ if connector.off => (t("已停用").into(), Theme::text_tertiary()),
                    _ if connector.target.trim().is_empty() => (t("待配置").into(), Theme::text_tertiary()),
                    None => (t("未连接").into(), Theme::text_tertiary()),
                    Some(Connection::Connecting) => (t("正在连接…").into(), Theme::text_tertiary()),
                    Some(Connection::Ready { tools, .. }) => (tf!("已连接 · {} 个工具", "Connected · {} tools", tools.len()).into(), Theme::success()),
                    Some(Connection::SignIn) => (t("需要在浏览器中登录").into(), Theme::warning()),
                    Some(Connection::SigningIn { .. }) => (t("正在等待浏览器中的登录…").into(), Theme::text_tertiary()),
                    Some(Connection::Failed(why)) => (why.clone().into(), Theme::danger()),
                };
                let name: SharedString = if connector.name.trim().is_empty() && notion_target { "Notion".into() } else if connector.name.trim().is_empty() && connector.target.trim().is_empty() { t("新连接器").into() } else if connector.name.trim().is_empty() { t("未命名的连接器").into() } else { connector.name.clone().into() };
                // The row opens the fields; the switch beside it is not inside it, or the row would hear its click.
                let head = div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .pr(px(16.))
                    .child(
                        div()
                            .id(("connector", i))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .flex_1()
                            .min_w_0()
                            .min_h(px(48.))
                            .pl(px(16.))
                            .py(px(9.))
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::hover()))
                            .on_click(self.on(Cmd::Custom(Do::SelectConnector(i)), cx))
                            .child(ui::icon(if shown.is_some() { "chevron-up" } else { "chevron-down" }, 14., Theme::text_tertiary()))
                            .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(name))
                            .child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).text_color(color).child(status)),
                    )
                    .when(notion_target && !connector.off && matches!(state, Some(Connection::Ready { .. })), |d| d.child(
                        button(("notion-settings", i), t("设置 Notion")).flex_none().on_click(self.on(Cmd::OpenSettings(crate::settings::SettingsPage::Notion), cx))))
                    .child(switch(("connector-on", i), !connector.off).on_click(self.on(Cmd::Custom(Do::ToggleConnector(i)), cx)));
                let fields = shown.map(|(_, fields)| {
                    let field = |input: &Entity<TextInput>| div().w(px(360.)).child(ui::field(input, window, cx));
                    let token = if notion_target && !connector.remote() { t("浏览器授权，无需填写令牌。") } else if connector.remote() { t("服务给了你令牌（API key）才填：作为 Authorization: Bearer 发送，和模型的密钥一样以明文保存在 settings.json 里。要在浏览器中登录的服务留空。") } else { t("只对网址形式的连接器有用。命令需要的密钥写在命令里或它自己的配置里。") };
                    let target = t("以 http(s):// 开头的是远程 MCP 服务的地址；其它内容当作一条在本机运行的命令，通过标准输入输出通信。");
                    let mut rows = vec![row(t("名称"), None, field(&fields.name)), row(t("地址或命令"), Some(target.into()), field(&fields.target)), row(t("令牌"), Some(token.into()), field(&fields.token))];
                    if connector.remote() && connector.token.trim().is_empty() && let Some((detail, control)) = self.sign_in_row(i, state, cx) {
                        rows.push(row(t("浏览器登录"), Some(detail), control));
                    }
                    if let Some(Connection::Ready { tools, .. }) = state && !tools.is_empty() {
                        let listed = tools.iter().map(|tool| if tool.read_only { tool.tool.clone() } else { format!("{} *", tool.tool) }).collect::<Vec<_>>().join("  ·  ");
                        rows.push(div().flex().flex_col().gap(px(4.)).px(px(16.)).py(px(10.)).child(t("工具")).child(small(listed.into(), Theme::text_secondary())).child(small(t("带 * 的工具没有声明只读，助手每次使用前会先问你。").into(), Theme::text_tertiary())));
                    }
                    let connect = button("connector-connect", if state.is_some() { t("重新连接") } else { t("连接") }).on_click(self.on(Cmd::Custom(Do::Connect(i)), cx));
                    let remove = button("connector-remove", t("删除…")).text_color(Theme::danger()).on_click(self.on(Cmd::Custom(Do::AskRemoveConnector(i)), cx));
                    rows.push(div().flex().items_center().gap(px(8.)).px(px(16.)).py(px(10.)).child(connect).child(div().flex_1()).child(remove));
                    div().flex().flex_col().w_full().bg(Theme::bg_sunken()).children(rows.into_iter().map(|row| row.border_t_1().border_color(Theme::line())))
                });
                div().flex().flex_col().w_full().child(head).children(fields).into_any_element()
            })
            .collect();
        connectors.push(div().flex().items_center().w_full().px(px(16.)).py(px(10.)).child(button("connector-add", t("添加连接器")).on_click(self.on(Cmd::Custom(Do::AddConnector), cx))).into_any_element());

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
            .child(group(Some(t("连接器")), connectors).child(note(t("连接器是 MCP 服务：连上以后，助手在对话里可以使用它提供的工具，例如把笔记写进 Notion。声明只读的工具直接运行，其它工具每次使用前都会先问你。工具返回的内容会发给你选的模型；只添加你信任的服务。需要登录的远程服务，点“连接”会打开浏览器登录（OAuth）；登录信息以明文保存在设置文件旁的 oauth.json 里，到期前自动续期。"))))
            .child(group(Some(t("技能")), skills).child(note(t("技能是一个带 SKILL.md 的文件夹，写法和 Claude 的技能相同：开头的 name 和 description 告诉助手它做什么，正文是做法。助手只在请求符合描述时读它。技能只是文字：文件夹里的脚本不会被运行。在文件夹里增删改以后，重新打开这一页就会读到。"))))
            .into_any_element()
    }
}
