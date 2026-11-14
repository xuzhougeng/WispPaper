//! 设置 › 自定义: what the person adds to the assistant. 连接器 are MCP
//! servers (`wisp_core::connector`, the transports in `wisp_net::mcp`) whose
//! tools the assistant may call beside the library's; 技能 are folders with
//! a `SKILL.md` (`wisp_core::skill`) it reads when a request calls for one.
//! A connector's tool that does not say it only reads is asked about before
//! it runs, like the tools that change the library.

use crate::app::{Cmd, Modal, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::settings::{button, group, note, row, switch};
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::*;
use wisp_core::connector::{Connector, Tool};
use wisp_core::skill;

/// Where a connector's connection stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Connecting,
    Ready(Vec<Tool>),
    Failed(String),
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
        let name = cx.new(|cx| TextInput::new("Notion", cx).with_text(&connector.name));
        let target = cx.new(|cx| TextInput::new("https://example.com/mcp · npx -y some-mcp-server", cx).with_text(&connector.target));
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

    /// Connect to every connector that is switched on: at start.
    pub fn connect_all(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.settings.connectors.len() {
            if !self.settings.connectors[index].off { self.connect(index, cx) }
        }
    }

    /// Connect to a connector anew and list its tools, off the main thread.
    /// Only asked for (a button, the switch, the start): a command half typed is never run.
    pub fn connect(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(connector) = self.settings.connectors.get(index).cloned() else { return };
        if connector.target.trim().is_empty() { return }
        let wanted = &self.settings.connectors;
        self.connections.retain(|(known, _)| !same(known, &connector) && wanted.iter().any(|c| same(c, known)));
        wisp_net::mcp::keep_only(wanted);
        self.connections.push((connector.clone(), Connection::Connecting));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let asked = connector.clone();
            let outcome = cx.background_executor().spawn(async move { wisp_net::mcp::tools(&asked) }).await;
            this.update(cx, |this, cx| {
                // Changed or connected again meanwhile: this answer is nobody's.
                if let Some((_, state)) = this.connections.iter_mut().find(|(known, state)| same(known, &connector) && *state == Connection::Connecting) {
                    *state = outcome.map_or_else(Connection::Failed, Connection::Ready);
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
            let Some(Connection::Ready(listed)) = self.connection(connector) else { continue };
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
                if connect { self.connect(index, cx) }
                cx.notify();
            }
            Do::Connect(index) => self.connect(index, cx),
            Do::AskRemoveConnector(index) => {
                let Some(connector) = self.settings.connectors.get(index) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除连接器「{}」？", "Delete connector \"{}\"?", connector.name), message: t("它的地址和令牌会从 settings.json 里删除，助手不再能使用它的工具。").into(), action: t("删除").into(), cmd: Cmd::Custom(Do::RemoveConnector(index)) });
                cx.notify();
            }
            Do::RemoveConnector(index) => {
                self.run(Cmd::CloseModal, window, cx);
                if index >= self.settings.connectors.len() { return }
                self.settings.connectors.remove(index);
                if let Some(view) = self.settings_view.as_mut() { view.connector = None }
                wisp_net::mcp::keep_only(&self.settings.connectors);
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
        self.save_settings(cx);
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
                let (status, color): (SharedString, Hsla) = match state {
                    _ if connector.off => (t("已停用").into(), Theme::text_tertiary()),
                    _ if connector.target.trim().is_empty() => (t("还没有填写地址或命令").into(), Theme::text_tertiary()),
                    None => (t("未连接").into(), Theme::text_tertiary()),
                    Some(Connection::Connecting) => (t("正在连接…").into(), Theme::text_tertiary()),
                    Some(Connection::Ready(tools)) => (tf!("已连接 · {} 个工具", "Connected · {} tools", tools.len()).into(), Theme::text_secondary()),
                    Some(Connection::Failed(why)) => (why.clone().into(), Theme::danger()),
                };
                let name: SharedString = if connector.name.trim().is_empty() { t("未命名的连接器").into() } else { connector.name.clone().into() };
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
                    .child(switch(("connector-on", i), !connector.off).on_click(self.on(Cmd::Custom(Do::ToggleConnector(i)), cx)));
                let fields = shown.map(|(_, fields)| {
                    let field = |input: &Entity<TextInput>| div().w(px(360.)).child(ui::field(input, window, cx));
                    let token = if connector.remote() { t("作为 Authorization: Bearer 发送，和模型的密钥一样以明文保存在 settings.json 里。") } else { t("只对网址形式的连接器有用。命令需要的密钥写在命令里或它自己的配置里。") };
                    let target = t("以 http(s):// 开头的是远程 MCP 服务的地址；其它内容当作一条在本机运行的命令，通过标准输入输出通信。");
                    let mut rows = vec![row(t("名称"), None, field(&fields.name)), row(t("地址或命令"), Some(target.into()), field(&fields.target)), row(t("令牌"), Some(token.into()), field(&fields.token))];
                    if let Some(Connection::Ready(tools)) = state && !tools.is_empty() {
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
            .child(group(Some(t("连接器")), connectors).child(note(t("连接器是 MCP 服务：连上以后，助手在对话里可以使用它提供的工具，例如把笔记写进 Notion。声明只读的工具直接运行，其它工具每次使用前都会先问你。工具返回的内容会发给你选的模型；只添加你信任的服务。只支持浏览器登录（OAuth）的远程服务，可以用命令 npx -y mcp-remote <地址> 来连接。"))))
            .child(group(Some(t("技能")), skills).child(note(t("技能是一个带 SKILL.md 的文件夹，写法和 Claude 的技能相同：开头的 name 和 description 告诉助手它做什么，正文是做法。助手只在请求符合描述时读它。技能只是文字：文件夹里的脚本不会被运行。在文件夹里增删改以后，重新打开这一页就会读到。"))))
            .into_any_element()
    }
}
