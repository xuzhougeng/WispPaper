//! User-initiated Notion saves over the official hosted MCP. No model decides
//! where to write. Authentication uses the configured MCP connector.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::{Path, PathBuf}, time::Duration};
use url::Url;
use wisp_core::{ai, libraries::LibraryInfo, model::Paper, notion::{self, Entry, Index, Page, Picture}, store::Store};

const ENDPOINT: &str = "https://mcp.notion.com/mcp";
fn agent() -> Result<ureq::Agent, String> { super::agent(Some(Duration::from_secs(90)), "") }
/// Only profile fields actually returned by Notion. Never derive an email from a domain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub workspace_id: String,
    pub workspace_name: String,
}

impl Account {
    fn read(identity: &Value) -> Result<Self, String> {
        let user = &identity["user"];
        let workspace = &identity["workspace"];
        let string = |value: &Value| value.as_str().unwrap_or("").to_string();
        let account = Self { user_id: string(&user["id"]), name: string(&user["name"]), email: string(user.get("email").filter(|v| v.is_string()).unwrap_or(&user["person"]["email"])), workspace_id: string(&workspace["id"]), workspace_name: string(&workspace["name"]) };
        if account.user_id.is_empty() || account.workspace_id.is_empty() { return Err("Notion 未返回完整的账号和工作区标识，无法确认笔记接收账户。".into()) }
        Ok(account)
    }
}

fn connector(path: &Path) -> Result<wisp_core::connector::Connector, String> {
    let settings = wisp_core::settings::Settings::load(path).map_err(|_| "无法读取连接器设置。")?;
    select_connector(settings.connectors)
}
pub(crate) fn notion_bridge(target: &str) -> bool {
    let words: Vec<_> = target.split_ascii_whitespace().collect();
    matches!(words.as_slice(), ["npx", "-y", "mcp-remote", url] if url.trim_end_matches('/') == ENDPOINT)
}
pub fn is_notion_target(target: &str) -> bool {
    target.trim().trim_end_matches('/') == ENDPOINT || notion_bridge(target)
}
fn select_connector(connectors: Vec<wisp_core::connector::Connector>) -> Result<wisp_core::connector::Connector, String> {
    let mut matches = connectors.into_iter().filter(|c| !c.off && is_notion_target(&c.target));
    let connector = matches.next().ok_or("请在「设置 → 自定义 → 添加连接器」填写 npx -y mcp-remote https://mcp.notion.com/mcp，留空令牌并连接。")?;
    if matches.next().is_some() { return Err("有多个已启用的 Notion 连接器，请只启用要保存到的那个。".into()) }
    Ok(connector)
}

fn connection(path: &Path) -> Result<(Client, Account), String> {
    let c = connector(path)?;
    let mut client = if c.remote() && !c.token.trim().is_empty() { Client::at(c.target.trim(), c.token.trim())? } else {
        Client::bridged(super::mcp::Session::existing(&c)?)?
    };
    let identity = Account::read(&client.call("notion-get-self", json!({}))?)?;
    Ok((client, identity))
}
pub fn account_info(path: &Path) -> Result<Account, String> { connection(path).map(|(_, account)| account) }

fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "无法生成安全登录参数。")?;
    Ok(base64url(&bytes))
}
fn base64url(bytes: &[u8]) -> String { ai::base64(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_") }

struct Client { bridge: Option<super::mcp::Session>, agent: ureq::Agent, endpoint: String, token: String, session: Option<String>, version: String, id: u64, tools: BTreeMap<String, Value> }

impl Client {
    fn at(endpoint: &str, token: &str) -> Result<Self, String> {
        let mut this = Self { bridge: None, agent: agent()?, endpoint: endpoint.into(), token: token.into(), session: None, version: "2025-11-25".into(), id: 0, tools: BTreeMap::new() };
        let hello = this.rpc("initialize", json!({"protocolVersion":this.version, "capabilities":{}, "clientInfo":{"name":"WispPaper","version":env!("CARGO_PKG_VERSION")}}))?;
        this.version = hello["protocolVersion"].as_str().ok_or("Notion MCP 未返回协议版本。")?.into();
        if !["2025-11-25", "2025-06-18", "2025-03-26"].contains(&this.version.as_str()) { return Err("Notion MCP 协议版本不受支持，请更新 WispPaper。".into()) }
        this.notify_initialized()?;
        this.discover()?;
        Ok(this)
    }

    fn bridged(session: super::mcp::Session) -> Result<Self, String> {
        let mut this = Self { bridge: Some(session), agent: agent()?, endpoint: String::new(), token: String::new(), session: None, version: String::new(), id: 0, tools: BTreeMap::new() };
        this.discover()?;
        Ok(this)
    }

    fn discover(&mut self) -> Result<(), String> {
        let mut cursor = None;
        loop {
            let list = self.rpc("tools/list", cursor.map_or(json!({}), |cursor: String| json!({"cursor":cursor})))?;
            for tool in list["tools"].as_array().ok_or("Notion MCP 工具清单无效。")? {
                if let Some(name) = tool["name"].as_str() { self.tools.insert(name.into(), tool["inputSchema"].clone()); }
            }
            cursor = list["nextCursor"].as_str().map(String::from);
            if cursor.is_none() { break }
        }
        Ok(())
    }

    fn request(&self) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut request = self.agent.post(&self.endpoint).header("Authorization", &format!("Bearer {}", self.token)).header("Accept", "application/json, text/event-stream").header("MCP-Protocol-Version", &self.version);
        if let Some(session) = &self.session { request = request.header("Mcp-Session-Id", session); }
        request
    }

    fn notify_initialized(&self) -> Result<(), String> {
        let response = self.request().send_json(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).map_err(|_| "Notion MCP 初始化失败。")?;
        if !response.status().is_success() { return Err(format!("Notion MCP 初始化失败（HTTP {}）。", response.status().as_u16())) }
        Ok(())
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if let Some(session) = &self.bridge { return session.ask(method, params) }
        self.id += 1;
        let mut response = self.request().send_json(json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})).map_err(|_| "Notion MCP 请求中断；写入结果可能尚未确认，请检查后重试。")?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) { return Err(format!("Notion MCP 返回 HTTP {status}；请检查连接与页面权限。")) }
        if let Some(session) = response.headers().get("mcp-session-id").and_then(|s| s.to_str().ok()) { self.session = Some(session.into()); }
        let streaming = response.headers().get("content-type").and_then(|s| s.to_str().ok()).is_some_and(|s| s.starts_with("text/event-stream"));
        let body = response.body_mut().with_config().limit(32 << 20).read_to_string().map_err(|_| "Notion MCP 响应中断；请检查远端保存结果。")?;
        rpc_result(&body, streaming, self.id)
    }

    fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let schema = self.tools.get(name).ok_or_else(|| format!("当前 Notion 连接未提供 {name}，请检查账号权限。"))?;
        // Earlier hosted servers wrapped arguments in `data`; discover this from the actual schema.
        let args = match schema["properties"].get("data") {
            Some(data) if data["type"] == "string" => json!({"data":args.to_string()}),
            Some(_) => json!({"data":args}), None => args,
        };
        let result = self.rpc("tools/call", json!({"name":name, "arguments":args}))?;
        let result = tool_result(&result)?;
        if let Some(task) = result.get("async_task").filter(|task| !task.is_null()) {
            let id = task["id"].as_str().ok_or("Notion 异步任务缺少 ID。")?.to_string();
            for _ in 0..60 {
                std::thread::sleep(Duration::from_secs(1));
                let task = self.call("notion-get-async-task", json!({"task_id":id}))?;
                match task["status"].as_str() {
                    Some("succeeded") => return Ok(task["result"].clone()),
                    Some("failed") => return Err("Notion 异步写入失败，请检查笔记页面。".into()), _ => {}
                }
            }
            return Err("Notion 仍在处理写入，请检查远端结果后重试。".into());
        }
        Ok(result)
    }

    fn fetch(&mut self, id: &str) -> Result<Value, String> { self.call("notion-fetch", json!({"id":id})) }

    fn set_title(&mut self, page: &Page, fetched: &Value, title: &str) -> Result<(), String> {
        if fetched_title(fetched).as_deref() == Some(title) { return Ok(()) }
        self.call("notion-update-page", json!({"page_id":page.id,"command":"update_properties","properties":{"title":title},"allow_async":false}))?;
        Ok(())
    }

    fn has_parent(&mut self, fetched: &Value, page_id: &str, parent: &str) -> Result<bool, String> {
        if parent_matches(fetched, parent) { return Ok(true) }
        // Some hosted fetch responses omit ancestry. A native child-page block
        // in the configured parent still proves membership; a plain link does not.
        Ok(has_child_page(&self.fetch(parent)?, page_id))
    }

    fn root(&mut self, index: &Index, key: &str, settings: &notion::Settings) -> Result<Page, String> {
        let title = settings.root_title.trim();
        if settings.parent_url.trim().is_empty() {
            if let Some(page) = index.page(key)?.filter(|page| !page.id.is_empty()) && deleted_page(&self.fetch(&page.id)?) { index.forget_deleted(key, &page.id)?; }
            return self.create(index, key, None, title, "");
        }
        let id = notion::page_id(&settings.parent_url)?;
        let parent = self.fetch(&id)?;
        if deleted_page(&parent) { return Err("设置的 Notion 父页面已被删除，请先修改父页面链接。".into()) }
        if fetched_title(&parent).as_deref() == Some(title) {
            let page = Page { url: format!("https://www.notion.so/{}", id.replace('-', "")), id };
            index.keep(key, &page)?;
            return Ok(page);
        }
        if let Some(page) = index.page(key)?.filter(|page| !page.id.is_empty()) {
            let content = self.fetch(&page.id)?;
            if deleted_page(&content) { index.forget_deleted(key, &page.id)?; }
            else if self.has_parent(&content, &page.id, &id)? { return Ok(page) }
            else { return Err("无法确认根页面仍在指定父页面内，请检查 Notion 页面位置。".into()) }
        }
        // Reuse only an exact, unique native child of the chosen parent. Never
        // adopt a workspace search result or a same-named page elsewhere.
        let roots = child_pages(&parent).into_iter().filter(|(_, name)| name == title).collect::<Vec<_>>();
        if roots.len() > 1 { return Err("父页面下有多个同名根页面，请直接填写需要使用的根页面链接。".into()) }
        if let Some((page, _)) = roots.into_iter().next() { self.fetch(&page.id)?; index.keep(key, &page)?; return Ok(page) }
        self.create(index, key, Some(&id), title, "")
    }

    fn library_page(&mut self, index: &Index, destination: &str, account: &str, settings: &notion::Settings, library: &LibraryInfo) -> Result<(String, Page), String> {
        let (root, parent) = if settings.root_title.trim().is_empty() {
            if settings.parent_url.trim().is_empty() { (notion::PRIVATE_ROOT.into(), None) } else {
                let id = notion::page_id(&settings.parent_url)?;
                if deleted_page(&self.fetch(&id)?) { return Err("设置的 Notion 父页面已被删除，请先修改父页面链接。".into()) }
                (id.clone(), Some(id))
            }
        } else {
            let root = self.root(index, destination, settings)?;
            (root.id.clone(), Some(root.id))
        };
        let path = format!("{account}/root:{root}/library:{}", notion::fingerprint(&library.id));
        let page = self.create(index, &path, parent.as_deref(), &library.name, "")?;
        Ok((root, page))
    }

    #[cfg(test)]
    fn append(&mut self, index: &Index, entry_key: &str, page_id: &str, markdown: &str) -> Result<(), String> {
        // insert_content takes `content`; new_str belongs to replacement commands.
        self.write_note(index, entry_key, entry_key, json!({"page_id":page_id,"command":"insert_content","content":markdown,"position":{"type":"end"},"allow_async":false}))
    }

    fn write_note(&mut self, index: &Index, entry_key: &str, source_key: &str, args: Value) -> Result<(), String> {
        index.begin_note(entry_key, source_key)?;
        if let Err(error) = self.call("notion-update-page", args) {
            if definitely_rejected(&error) { index.confirm_entry(entry_key)?; }
            return Err(error);
        }
        index.finish_note(entry_key, source_key)
    }

    fn append_entry(&mut self, index: &Index, scope: &str, page: &Page, entry: &Entry, markdown: &str) -> Result<(), String> {
        self.write_note(index, &format!("{scope}/{}", entry.key), &format!("{scope}/{}", entry.source_key),
            json!({"page_id":page.id,"command":"insert_content","content":markdown,"position":{"type":"end"},"allow_async":false}))
    }

    fn create(&mut self, index: &Index, key: &str, parent: Option<&str>, title: &str, body: &str) -> Result<Page, String> {
        let marker = format!("wisppaper-page-{}", notion::fingerprint(key));
        if let Some(page) = index.page(key)? {
            if page.id.is_empty() {
                // A lost create response can be recovered by its page marker, never by title alone.
                let found = self.call("notion-search", json!({"query":title,"query_type":"internal"}))?;
                let mut recovered = vec![];
                for result in found["results"].as_array().into_iter().flatten() {
                    if let Some(id) = result["id"].as_str() {
                        let content = self.fetch(id)?;
                        if content.to_string().contains(&marker) && match parent { Some(parent) => self.has_parent(&content, id, parent)?, None => true } {
                            let id = notion::page_id(id)?;
                            recovered.push(Page { url: format!("https://www.notion.so/{}", id.replace('-', "")), id });
                        }
                    }
                }
                if recovered.len() == 1 { index.keep(key, &recovered[0])?; return Ok(recovered.remove(0)); }
                return Err(format!("页面「{title}」上次创建结果待核实；未找到唯一匹配页面，不会自动重复创建。请检查 Notion 后重试。"));
            }
            let content = self.fetch(&page.id)?;
            if let Some(parent) = parent && !self.has_parent(&content, &page.id, parent)? { return Err("无法确认笔记页面仍在配置的父页面内，已停止写入。请检查 Notion 页面位置。".into()) }
            return Ok(page);
        }
        let mut args = json!({"pages":[{"properties":{"title":title},"content":body}], "allow_async":false});
        if let Some(id) = parent { args["parent"] = json!({"page_id":id}); } else { args["creation_mode"] = json!("draft"); }
        index.creating(key)?;
        let result = match self.call("notion-create-pages", args) {
            Ok(result) => result,
            Err(error) => { if definitely_rejected(&error) { index.forget_uncreated(key)?; } return Err(error) }
        };
        let first = result["pages"].as_array().and_then(|p| p.first()).ok_or("Notion 未确认新页面，请检查远端结果。")?;
        let id = notion::page_id(first["id"].as_str().ok_or("Notion 未返回页面 ID。")?)?;
        let url = format!("https://www.notion.so/{}", id.replace('-', ""));
        let page = Page { id, url };
        index.keep(key, &page)?;
        Ok(page)
    }

    fn upload(&mut self, picture: &Picture) -> Result<String, String> {
        if picture.bytes.len() > 20 << 20 { return Err(format!("图片 {} 超过 Notion 单次上传的 20 MiB 限制。", picture.id)) }
        let extension = match picture.media_type.as_str() { "image/png" => "png", "image/jpeg" => "jpg", "image/gif" => "gif", "image/webp" => "webp", _ => return Err("Notion 图片格式不受支持。".into()) };
        let filename = format!("wisppaper-{}.{}", notion::fingerprint(&picture.id), extension);
        let upload = self.call("notion-create-file-upload", json!({"filename":filename,"content_type":picture.media_type}))?;
        let url = upload["upload_url"].as_str().ok_or("Notion 未返回图片上传地址。")?;
        let target = Url::parse(url).map_err(|_| "Notion 图片上传地址无效。")?;
        let host = target.host_str().unwrap_or("");
        if target.scheme() != "https" || !(host == "notion.com" || host.ends_with(".notion.com") || host == "notion.so" || host.ends_with(".notion.so")) { return Err("Notion 图片上传地址不受信任。".into()) }
        let boundary = format!("WispPaper{}", random()?);
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n", picture.media_type).into_bytes();
        body.extend_from_slice(&picture.bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut request = self.agent.post(url);
        for (name, value) in upload["upload_headers"].as_object().ok_or("Notion 未返回图片上传请求头。")? {
            if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") { return Err("Notion 图片上传请求头无效。".into()) }
            request = request.header(name, value.as_str().ok_or("Notion 图片上传请求头无效。")?);
        }
        let mut response = request.header("Content-Type", &format!("multipart/form-data; boundary={boundary}")).send(body.as_slice()).map_err(|_| "Notion 图片上传中断，笔记未完整保存。")?;
        if !response.status().is_success() { return Err(format!("Notion 图片上传失败（HTTP {}），笔记未完整保存。", response.status().as_u16())) }
        let response: Value = response.body_mut().read_json().map_err(|_| "Notion 图片上传响应无效。")?;
        response["suggested_markdown"].as_str().map(String::from).ok_or("Notion 未返回持久图片引用。".into())
    }
}

fn rpc_result(body: &str, streaming: bool, id: u64) -> Result<Value, String> {
    let candidates = if streaming {
        body.replace("\r\n", "\n").split("\n\n").filter_map(|event| {
            let text = event.lines().filter_map(|line| line.strip_prefix("data:").map(str::trim_start)).collect::<Vec<_>>().join("\n");
            serde_json::from_str::<Value>(&text).ok()
        }).collect::<Vec<_>>()
    } else { vec![serde_json::from_str(body).map_err(|_| "Notion MCP 返回了无效 JSON。")?] };
    let result = candidates.into_iter().find(|v| v["id"].as_u64() == Some(id)).ok_or("Notion MCP 未返回匹配的结果。")?;
    if let Some(error) = result.get("error") { return Err(format!("Notion MCP：{}", error["message"].as_str().unwrap_or("协议错误"))) }
    result.get("result").cloned().ok_or("Notion MCP 缺少结果。".into())
}

fn tool_result(result: &Value) -> Result<Value, String> {
    let text = result["content"].as_array().map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    if result["isError"] == true {
        let error = serde_json::from_str::<Value>(&text).unwrap_or_default();
        let message = error["message"].as_str().unwrap_or(&text);
        let code = error["code"].as_str().unwrap_or("tool_error");
        return Err(format!("Notion 工具执行失败（{code}）：{message}"));
    }
    let value = result.get("structuredContent").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| serde_json::from_str(&text).unwrap_or(json!({"text":text})));
    if value["warnings"].as_array().is_some_and(|w| !w.is_empty()) { return Err("Notion 返回了内容转换警告，请检查笔记；本次未标记为完整保存。".into()) }
    Ok(value)
}

fn parent_matches(fetched: &Value, expected: &str) -> bool {
    let compact = expected.replace('-', "");
    if let Some(id) = fetched["parent"]["page_id"].as_str() { return id.replace('-', "") == compact }
    // Hosted fetch returns the ancestry as Notion-flavored XML inside its text field.
    let text = fetched["text"].as_str().unwrap_or("");
    let ancestors = text.split_once("<ancestor-path>").and_then(|(_, rest)| rest.split_once("</ancestor-path>")).map(|(path, _)| path).unwrap_or("");
    ancestors.replace('-', "").contains(&compact)
}

fn has_child_page(fetched: &Value, expected: &str) -> bool {
    child_pages(fetched).iter().any(|(page, _)| page.id.replace('-', "") == expected.replace('-', ""))
}

fn fetched_title(fetched: &Value) -> Option<String> {
    if let Some(title) = fetched["properties"]["title"].as_str() { return Some(title.to_string()) }
    let properties = fetched["text"].as_str().and_then(|text| text.split_once("<properties>")).and_then(|(_, rest)| rest.split_once("</properties>")).and_then(|(properties, _)| serde_json::from_str::<Value>(properties.trim()).ok());
    properties.and_then(|p| p["title"].as_str().map(String::from)).or_else(|| fetched["title"].as_str().map(String::from))
}

fn deleted_page(fetched: &Value) -> bool {
    fetched["in_trash"] == true || fetched["archived"] == true || fetched["text"].as_str().and_then(|text| text.split_once("<page ")).and_then(|(_, rest)| rest.split_once('>')).is_some_and(|(tag, _)| tag.split_whitespace().any(|part| part == "deleted"))
}

fn child_pages(fetched: &Value) -> Vec<(Page, String)> {
    let text = fetched["text"].as_str().unwrap_or("");
    let content = text.split_once("<content>").and_then(|(_, rest)| rest.rsplit_once("</content>")).map(|(content, _)| content).unwrap_or("");
    let mut in_code = false;
    let mut pages = vec![];
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```") { in_code = !in_code; continue }
        if in_code { continue }
        let Some((url, rest)) = line.strip_prefix("<page url=\"").and_then(|rest| rest.split_once('"')) else { continue };
        let Ok(id) = notion::page_id(url) else { continue };
        let Some((_, rest)) = rest.split_once('>') else { continue };
        let mut title = rest.to_string();
        while !title.contains("</page>") {
            let Some(line) = lines.next() else { break };
            title.push('\n');
            title.push_str(line);
        }
        if let Some((title, _)) = title.split_once("</page>") { pages.push((Page { id, url: url.to_string() }, title.trim().to_string())); }
    }
    pages
}

/// These errors reject the request before a write. A dropped connection, parser
/// warning or server failure can follow a write and must keep the pending record.
fn definitely_rejected(error: &str) -> bool {
    error.contains("validation_error") || error.starts_with("当前 Notion 连接未提供 ") || [400, 401, 403, 404, 405, 429].iter().any(|status| error.starts_with(&format!("Notion MCP 返回 HTTP {status}；")))
}

#[derive(Clone)]
pub struct Save {
    pub settings: notion::Settings,
    pub settings_path: PathBuf,
    pub library: LibraryInfo,
    pub paper: Option<Paper>,
    /// Literal title confirmed in this save's dialog; never stored as a global setting.
    pub title: Option<String>,
    pub entries: Vec<Entry>,
    pub pictures: Vec<Picture>,
}

impl Save {
    pub fn requires_title(&self) -> bool { self.paper.is_some() && self.settings.custom_title() }

    fn note_title(&self) -> Result<String, String> {
        match &self.paper {
            Some(_) if self.requires_title() => notion::manual_title(self.title.as_deref().unwrap_or("")),
            Some(paper) => Ok(self.settings.paper_title(paper)),
            None => Ok("文献库问答".into()),
        }
    }
}

pub struct Saved { pub url: String, pub added: usize }

pub fn check(path: &Path, settings: &notion::Settings) -> Result<Account, String> {
    settings.validate()?;
    let (mut client, identity) = connection(path)?;
    for tool in ["notion-fetch", "notion-create-pages", "notion-update-page", "notion-create-file-upload"] {
        if !client.tools.contains_key(tool) { return Err(format!("Notion 连接缺少必要工具：{tool}")) }
    }
    if !settings.parent_url.trim().is_empty() { client.fetch(&notion::page_id(&settings.parent_url)?)?; }
    Ok(identity)
}

pub fn save(job: Save) -> Result<Saved, String> {
    job.settings.validate()?;
    let title = job.note_title()?;
    if job.entries.is_empty() { return Err("请至少选择一条笔记。".into()) }
    let (mut client, identity) = connection(&job.settings_path)?;
    let account = notion::fingerprint(&format!("{}/{}", identity.workspace_id, identity.user_id));
    for tool in ["notion-fetch", "notion-create-pages", "notion-update-page"] {
        if !client.tools.contains_key(tool) { return Err(format!("当前 Notion 连接未提供 {tool}，尚未写入笔记。")) }
    }
    if !job.pictures.is_empty() && !client.tools.contains_key("notion-create-file-upload") { return Err("Notion 连接缺少图片上传工具，尚未写入笔记。".into()) }
    let index = Index::open(&job.settings_path.with_file_name("notion.sqlite"))?;
    let destination = format!("{}/{}", account, job.settings.destination()?);
    let (root, page) = client.library_page(&index, &destination, &account, &job.settings, &job.library)?;
    // The actual root ID keeps the same note mapping when users switch between
    // a root URL and its containing parent URL in settings.
    let paper_id = job.paper.as_ref().map(|p| p.id.as_str()).unwrap_or("");
    let path = notion::note_scope(&account, &root, &job.library.id, paper_id);
    let page = client.create(&index, &path, Some(&page.id), &title, &job.settings.header(&job.library, job.paper.as_ref()))?;
    // Keep the access link even when a later image upload fails.
    Store::open(&job.library.dir).and_then(|store| store.keep_notion_link(&path, paper_id, &page.url)).map_err(|e| e.to_string())?;
    let mut fetched = client.fetch(&page.id)?;
    if job.requires_title() { client.set_title(&page, &fetched, &title)?; }
    let mut images: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut added = 0;
    for (position, entry) in job.entries.iter().enumerate() {
        let entry_key = format!("{path}/{}", entry.key);
        let source_key = format!("{path}/{}", entry.source_key);
        if index.entry_saved(&entry_key)? || fetched.to_string().contains(&entry.marker()) { index.finish_note(&entry_key, &source_key)?; continue }
        if index.entry_pending(&entry_key)? || index.source_pending(&source_key)? { return Err("上次追加笔记的结果待核实；为避免重复，已停止追加。请检查 Notion 页面。".into()) }
        let conversation = entry.group.as_ref().map_or("", |group| group.id.as_str());
        let mut entry_images = BTreeMap::new();
        for id in &entry.images {
            let image_id = (conversation.to_string(), id.clone());
            if let Some(markdown) = images.get(&image_id) { entry_images.insert(id.clone(), markdown.clone()); continue }
            let picture = job.pictures.iter().find(|p| p.conversation == conversation && &p.id == id).ok_or_else(|| format!("缺少图片附件 {id}，笔记未完整保存。"))?;
            // Uploaded files belong to the connection that created them. Only reuse
            // confirmed, attached files from this OAuth client, never orphan uploads.
            let image_key = format!("{}/{}/{:x}", account, notion::fingerprint(&client.token), Sha256::digest(&picture.bytes));
            let markdown = match index.image(&image_key)? { Some(markdown) => markdown, None => client.upload(picture)? };
            entry_images.insert(id.clone(), markdown.clone());
            images.insert(image_id, markdown);
        }
        let markdown = entry.markdown(&entry_images)?;
        client.append_entry(&index, &path, &page, entry, &markdown)?;
        for id in &entry.images {
            let picture = job.pictures.iter().find(|p| p.conversation == conversation && &p.id == id).ok_or("图片附件已不可用。")?;
            let image_key = format!("{}/{}/{:x}", account, notion::fingerprint(&client.token), Sha256::digest(&picture.bytes));
            index.keep_image(&image_key, &entry_images[id])?;
        }
        added += 1;
        if position + 1 < job.entries.len() { fetched = client.fetch(&page.id)?; }
    }
    Ok(Saved { url: page.url, added })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Opt-in only. Uses this local app's configured MCP connector
    /// token, and writes a clearly labelled test page under its configured root.
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires WISP_NOTION_SMOKE_SETTINGS and an explicitly authorized live Notion write"]
    fn live_multi_history_write_and_readback() {
        use wisp_core::{model::Paper, store::Conversation};
        let settings_path = PathBuf::from(std::env::var_os("WISP_NOTION_SMOKE_SETTINGS").expect("set the local app settings path explicitly"));
        let app_settings: wisp_core::settings::Settings = serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        let mut settings = app_settings.notion;
        settings.title_mode = "parts".into();
        settings.title_parts = vec!["title".into()];
        settings.fields = vec!["library".into(), "title".into()];
        let dir = settings_path.parent().unwrap().join("notion-verification");
        std::fs::create_dir_all(&dir).unwrap();
        let library = LibraryInfo { id: "L-notion-write-verification".into(), name: "Notion 写入验收（测试）".into(), dir, ..Default::default() };
        let paper = Paper { id: "P-notion-write-verification".into(), title: "[测试] WispPaper 多历史对话写入验收".into(), ..Default::default() };
        let run = wisp_core::model::new_id("test");
        let created_at = wisp_core::time::now_iso();
        let user = |text: &str, image: bool| ai::Said::User { asked_at: String::new(), text: text.into(), images: if image { vec!["shared-image-id".into()] } else { vec![] }, prompt: String::new() };
        let answer = |text: &str| ai::Said::Answer { text: text.into() };
        let mut a = Conversation { id: format!("CA-{run}"), created_at: created_at.clone(), transcript: ai::Transcript { items: vec![user("验收对话 A：图文保存", true), answer("原始回答"), user("不应保存的未选择问题", false), answer("不应保存的回答")], ..Default::default() }, ..Default::default() };
        let b = Conversation { id: format!("CB-{run}"), created_at, transcript: ai::Transcript { items: vec![user("验收对话 B：另一张原图", true), answer("B 的图文回答")], ..Default::default() }, ..Default::default() };
        let first = notion::conversation_entries(&a, Some(0)).remove(0);
        let first = first.edited(&first.draft().replace("原始回答", "已编辑的验收回答")).unwrap();
        let second = notion::conversation_entries(&b, Some(0)).remove(0);
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let pictures = vec![
            Picture { conversation: a.id.clone(), id: "shared-image-id".into(), media_type: "image/png".into(), bytes: std::fs::read(assets.join("branding/app-icon.png")).unwrap() },
            Picture { conversation: b.id.clone(), id: "shared-image-id".into(), media_type: "image/png".into(), bytes: std::fs::read(assets.join("plugins/notion.png")).unwrap() },
        ];
        let mut job = Save { settings, settings_path: settings_path.clone(), library, paper: Some(paper), title: None, entries: vec![first, second], pictures };
        let saved = save(job.clone()).unwrap();
        assert_eq!(saved.added, 2);
        let repeated = save(job.clone()).unwrap();
        assert_eq!(repeated.added, 0);
        assert_eq!(saved.url, repeated.url);
        let (mut client, _) = connection(&settings_path).unwrap();
        let id = notion::page_id(&saved.url).unwrap();
        client.call("notion-update-page", json!({"page_id":id,"command":"insert_content","content":"## 验收手工内容\n\n这段文字必须保留。","position":{"type":"end"},"allow_async":false})).unwrap();
        a.transcript.items.extend([user("验收 A 的新增问题", false), answer("新增问题应位于 A 分组内部。")]);
        job.entries = notion::conversation_entries(&a, Some(2));
        let appended = save(job).unwrap();
        assert_eq!(appended.added, 1);
        assert_eq!(saved.url, appended.url);
        let fetched = client.fetch(&id).unwrap();
        let text = fetched["text"].as_str().unwrap();
        assert!(text.contains("已编辑的验收回答") && text.contains("这段文字必须保留。"));
        assert!(!text.contains("不应保存的未选择问题") && !text.contains("不应保存的回答"));
        assert!(text.find("验收 A 的新增问题").unwrap() > text.find("B 的图文回答").unwrap());
        assert!(!text.contains("## 对话："));
        assert!(text.matches("notion-file-block://").count() >= 2);
        assert!(text.contains("题目") && !text.contains("完整题目"));
        assert!(text.find("来源文献库").unwrap() < text.find("题目").unwrap());
        assert!(!text.contains("来自 WispPaper") && !text.contains("引用文献"));
        println!("NOTION_VERIFIED_URL={}", saved.url);
        println!("Verified: two conversations, two scoped images, edited text, selective questions, append, duplicate skip and preserved manual content.");
    }
    #[cfg(unix)]
    #[test]
    fn manual_note_uses_the_existing_stdio_session_for_identity_and_write() {
        use wisp_core::connector::Connector;
        // The mock refuses the write unless get-self ran in this very process.
        let command = r#"python3 -uc 'import sys,json
authorized=False
for line in sys.stdin:
 m=json.loads(line); method=m.get("method"); result={}
 if "id" not in m: continue
 if method=="tools/list": result={"tools":[{"name":n,"inputSchema":{}} for n in ["notion-get-self","notion-update-page"]]}
 if method=="tools/call":
  if m["params"]["name"]=="notion-get-self":
   authorized=True; result={"structuredContent":{"user":{"id":"U"},"workspace":{"id":"W"}}}
  else: result={"structuredContent":{"saved":authorized}}
 print(json.dumps({"jsonrpc":"2.0","id":m["id"],"result":result}))
'"#;
        let c = Connector { target: command.into(), ..Default::default() };
        assert!(super::super::mcp::Session::existing(&c).is_err());
        super::super::mcp::tools(&c).unwrap();
        let mut client = Client::bridged(super::super::mcp::Session::existing(&c).unwrap()).unwrap();
        let account = Account::read(&client.call("notion-get-self", json!({})).unwrap()).unwrap();
        assert_eq!(account.workspace_id, "W");
        assert_eq!(client.call("notion-update-page", json!({})).unwrap()["saved"], true);
    }
    #[test]
    fn manual_saves_require_one_enabled_official_connector() {
        use wisp_core::connector::Connector;
        let bridge = Connector { target: format!("npx -y mcp-remote {ENDPOINT}"), ..Default::default() };
        assert_eq!(select_connector(vec![bridge.clone()]).unwrap(), bridge);
        assert!(select_connector(vec![Connector { target: format!("{} && echo other", bridge.target), ..Default::default() }]).is_err());
        let chosen = Connector { target: ENDPOINT.into(), token: "sample".into(), ..Default::default() };
        let disabled = Connector { off: true, ..chosen.clone() };
        let other = Connector { target: "https://example.org/mcp".into(), name: "Notion".into(), ..chosen.clone() };
        assert!(select_connector(vec![disabled.clone(), other.clone()]).is_err());
        assert!(select_connector(vec![chosen.clone(), chosen.clone()]).is_err());
        assert!(select_connector(vec![Connector { token: String::new(), ..chosen.clone() }]).is_ok());
        assert_eq!(select_connector(vec![other, disabled, chosen.clone()]).unwrap(), chosen);
    }
    #[test]
    fn account_profile_is_attributed_and_missing_email_is_not_inferred() {
        let profile = Account::read(&json!({"user":{"id":"U1","name":"Test Person","person":{"email":"test@example.org"}},"workspace":{"id":"W1","name":"Research"}})).unwrap();
        assert_eq!(profile.email, "test@example.org");
        assert_eq!(profile.workspace_name, "Research");
        let bot = Account::read(&json!({"user":{"id":"U2","type":"bot"},"workspace":{"id":"W1"},"email_domain":"example.org"})).unwrap();
        assert!(bot.name.is_empty() && bot.email.is_empty() && bot.workspace_name.is_empty());
        assert_eq!(bot.user_id, "U2");
        assert!(Account::read(&json!({"user":{"name":"Someone"}})).is_err());
    }
    #[test]
    fn sse_matches_request_and_preserves_multiline_data() {
        let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\n\r\nevent: message\r\ndata: {\"id\":7,\r\ndata: \"result\":{\"ok\":true}}\r\n\r\n";
        assert_eq!(rpc_result(body, true, 7).unwrap(), json!({"ok":true}));
        assert!(rpc_result(body, true, 8).is_err());
    }
    #[test]
    fn tool_errors_and_parser_warnings_are_not_success() {
        assert!(tool_result(&json!({"isError":true,"content":[{"text":"denied"}]})).is_err());
        assert!(tool_result(&json!({"structuredContent":{"warnings":[{"code":"dropped"}]}})).is_err());
    }
    #[test]
    fn rejected_append_can_retry_content_on_the_same_page() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let page = Page { id: "418b89d0-80ff-81e1-839d-e2d1110533a8".into(), url: "https://www.notion.so/418b89d080ff81e1839de2d1110533a8".into() };
        index.keep("paper", &page).unwrap();
        let message = "The \"insert_content\" command requires a \"content\" parameter.";
        let error = json!({"name":"APIResponseError","code":"validation_error","status":400,"message":message,"body":"verbose escaped server response"});
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"isError":true,"content":[{"type":"text","text":error.to_string()}]}}), ""),
            reply(json!({"id":2,"result":{"structuredContent":{"page_id":page.id}}}), ""),
        ]);
        let mut client = Client { bridge: None, agent: agent().unwrap(), endpoint: base, token: "example".into(), session: None, version: "2025-11-25".into(), id: 0, tools: BTreeMap::from([("notion-update-page".into(), json!({"properties":{"content":{"type":"string"}}}))]) };
        let markdown = "### Q：图片说明？\n\n![原图](file-upload://example)\n\nA：示例回答";
        let error = client.append(&index, "entry", &page.id, markdown).unwrap_err();
        assert!(error.contains(message) && error.contains("validation_error"));
        assert!(!error.contains("verbose escaped"));
        assert!(!index.entry_pending("entry").unwrap());
        assert!(!index.entry_saved("entry").unwrap());
        assert_eq!(index.page("paper").unwrap().unwrap().id, page.id);
        client.append(&index, "entry", &page.id, markdown).unwrap();
        assert!(!index.entry_pending("entry").unwrap());
        assert!(index.entry_saved("entry").unwrap());
        let sent = server.join().unwrap();
        let request = request_body(&sent);
        let args = &request["params"]["arguments"];
        assert_eq!(request["params"]["name"], "notion-update-page");
        assert_eq!(args["page_id"], page.id);
        assert_eq!(args["command"], "insert_content");
        assert_eq!(args["content"], markdown);
        assert_eq!(args["position"]["type"], "end");
        assert!(args.get("new_str").is_none());
    }
    #[test]
    fn append_with_an_uncertain_result_keeps_its_pending_record() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"warnings":[{"code":"unsupported_tag"}]}}}), "")]);
        let mut client = Client { bridge: None, agent: agent().unwrap(), endpoint: base, token: "example".into(), session: None, version: "2025-11-25".into(), id: 0, tools: BTreeMap::from([("notion-update-page".into(), json!({"properties":{}}))]) };
        assert!(client.append(&index, "entry", "page", "content").is_err());
        assert!(index.entry_pending("entry").unwrap());
        server.join().unwrap();
    }
    fn reply(value: Value, headers: &str) -> &'static str {
        let body = value.to_string();
        Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).into_boxed_str())
    }
    fn request_body(request: &str) -> Value {
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        if !headers.to_ascii_lowercase().contains("transfer-encoding: chunked") { return serde_json::from_str(body).unwrap() }
        let mut input = body.as_bytes();
        let mut decoded = vec![];
        loop {
            let end = input.windows(2).position(|w| w == b"\r\n").unwrap();
            let length = usize::from_str_radix(std::str::from_utf8(&input[..end]).unwrap(), 16).unwrap();
            if length == 0 { break }
            input = &input[end + 2..];
            decoded.extend_from_slice(&input[..length]);
            input = &input[length + 2..];
        }
        serde_json::from_slice(&decoded).unwrap()
    }
    #[test]
    fn mcp_discovers_wrapped_arguments_and_carries_session() {
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"protocolVersion":"2025-06-18"}}), "Mcp-Session-Id: example-session\r\n"),
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            reply(json!({"id":2,"result":{"tools":[{"name":"notion-fetch","inputSchema":{"properties":{"data":{"type":"object"}}}}]}}), ""),
            reply(json!({"id":3,"result":{"content":[{"type":"text","text":"{\"text\":\"a note\"}"}]}}), ""),
        ]);
        let mut client = Client::at(&base, "example-token").unwrap();
        assert_eq!(client.fetch("page").unwrap()["text"], "a note");
        let sent = server.join().unwrap();
        assert!(sent.to_lowercase().contains("mcp-session-id: example-session"));
        assert!(sent.to_lowercase().contains("mcp-protocol-version: 2025-06-18"));
        assert_eq!(request_body(&sent)["params"]["arguments"], json!({"data":{"id":"page"}}));
    }
    #[test]
    fn uncertain_page_creation_cannot_issue_another_create() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        index.creating("key").unwrap();
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"protocolVersion":"2025-11-25"}}), ""),
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            reply(json!({"id":2,"result":{"tools":[{"name":"notion-search","inputSchema":{"properties":{}}}]}}), ""),
            reply(json!({"id":3,"result":{"structuredContent":{"results":[]}}}), ""),
        ]);
        let mut client = Client::at(&base, "token").unwrap();
        let error = client.create(&index, "key", None, "Title", "body").err().unwrap();
        assert!(error.contains("待核实"));
        let sent = server.join().unwrap();
        assert_eq!(request_body(&sent)["params"]["name"], "notion-search");
    }
    #[test]
    fn moved_page_is_refused_even_if_its_body_mentions_the_old_parent() {
        let id = "418b89d0-80ff-8185-925f-e63b08fcd400";
        assert!(parent_matches(&json!({"text":format!("<ancestor-path><parent-page url=\"https://www.notion.so/{}\" /></ancestor-path>",id.replace('-',""))}), id));
        assert!(!parent_matches(&json!({"text":format!("<ancestor-path></ancestor-path>\nbody link {id}")}), id));
    }
    #[test]
    fn missing_ancestry_is_checked_against_native_child_pages() {
        let id = "418b89d0-80ff-81e1-839d-e2d1110533a8";
        let child = format!("<page url=\"https://app.notion.com/p/{}\">\nPaper\n</page>", id.replace('-', ""));
        let parent = json!({"text":format!("<page url=\"https://app.notion.com/p/parent\">\n<ancestor-path></ancestor-path>\n<content>\n{child}\n</content>\n</page>")});
        assert!(has_child_page(&parent, id));
        assert!(!has_child_page(&json!({"text":format!("<content>\n[Paper](https://www.notion.so/{id})\n</content>")}), id));
        assert!(!has_child_page(&json!({"text":format!("<content>\n```xml\n{child}\n```\n</content>")}), id));
        assert!(!has_child_page(&json!({"text":format!("{child}\n<content>empty</content>")}), id));
        assert!(!has_child_page(&parent, "418b89d0-80ff-8185-925f-e63b08fcd400"));
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":parent}}), "")]);
        let mut client = Client { bridge: None, agent: agent().unwrap(), endpoint: base, token: "example".into(), session: None, version: "2025-11-25".into(), id: 0, tools: BTreeMap::from([("notion-fetch".into(), json!({"properties":{}}))]) };
        assert!(client.has_parent(&json!({"text":"<ancestor-path></ancestor-path>"}), id, "parent").unwrap());
        assert_eq!(request_body(&server.join().unwrap())["params"]["arguments"]["id"], "parent");
    }
    #[test]
    fn create_uses_selected_parent_and_persists_confirmed_mapping() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"protocolVersion":"2025-11-25"}}), ""),
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            reply(json!({"id":2,"result":{"tools":[{"name":"notion-create-pages","inputSchema":{"properties":{}}}]}}), ""),
            reply(json!({"id":3,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff8185925fe63b08fcd400"}]}}}), ""),
        ]);
        let mut client = Client::at(&base, "token").unwrap();
        let page = client.create(&index, "scope", Some("parent-id"), "期刊 2020 题目", "Full metadata").unwrap();
        assert_eq!(index.page("scope").unwrap().unwrap().id, page.id);
        let sent = server.join().unwrap();
        let arguments = request_body(&sent)["params"]["arguments"].clone();
        assert_eq!(arguments["parent"]["page_id"], "parent-id");
        assert!(arguments.get("creation_mode").is_none());
        assert!(arguments["pages"][0]["content"].as_str().unwrap().contains("Full metadata"));
        assert!(!arguments["pages"][0]["content"].as_str().unwrap().contains("来自 WispPaper"));
    }
    fn root_client(base: String) -> Client {
        Client { bridge: None, agent: agent().unwrap(), endpoint: base, token: "example".into(), session: None, version: "2025-11-25".into(), id: 0, tools: BTreeMap::from([("notion-fetch".into(), json!({"properties":{}})), ("notion-create-pages".into(), json!({"properties":{}}))]) }
    }
    #[test]
    fn custom_titles_are_required_per_save_before_remote_access() {
        let settings = notion::Settings { title_mode: "custom".into(), title_template: String::new(), ..Default::default() };
        let mut first = Save { settings, settings_path: PathBuf::from("unused-settings"), library: LibraryInfo::default(), paper: Some(Paper { title:"Paper A".into(), ..Default::default() }), title: None, entries: vec![], pictures: vec![] };
        assert!(first.settings.validate().is_ok());
        assert!(first.requires_title());
        assert_eq!(save(first.clone()).err().unwrap(), "请填写本篇文献的笔记标题。");
        first.title = Some("文献 A 的阅读笔记".into());
        assert_eq!(first.note_title().unwrap(), "文献 A 的阅读笔记");
        let mut second = Save { paper: Some(Paper { title:"Paper B".into(), ..Default::default() }), title: None, ..first.clone() };
        assert!(second.note_title().is_err());
        second.title = Some("文献 B 的方法笔记".into());
        assert_eq!(second.note_title().unwrap(), "文献 B 的方法笔记");
        assert_ne!(second.note_title().unwrap(), first.note_title().unwrap());
        second.paper = None;
        assert!(!second.requires_title());
        assert_eq!(second.note_title().unwrap(), "文献库问答");
        first.settings.title_mode = "parts".into();
        assert!(!first.requires_title());
        assert_eq!(first.note_title().unwrap(), "Paper A");
    }
    #[test]
    fn confirmed_custom_titles_update_properties_without_replacing_content() {
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"page_id":"page"}}}), "")]);
        let mut client = root_client(base);
        client.tools.insert("notion-update-page".into(), json!({"properties":{}}));
        let page = Page { id: "page".into(), url: "https://example.org/page".into() };
        let fetched = json!({"properties":{"title":"旧标题"},"text":"<content>手工内容与原问答</content>"});
        client.set_title(&page, &fetched, "本篇文献的新标题").unwrap();
        let args = request_body(&server.join().unwrap())["params"]["arguments"].clone();
        assert_eq!(args["command"], "update_properties");
        assert_eq!(args["page_id"], "page");
        assert_eq!(args["properties"], json!({"title":"本篇文献的新标题"}));
        assert!(args.get("content").is_none() && args.get("new_str").is_none());
        client.set_title(&page, &json!({"properties":{"title":"本篇文献的新标题"}}), "本篇文献的新标题").unwrap();
    }
    #[test]
    fn an_empty_root_creates_the_library_directly_under_the_selected_parent() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { root_title: String::new(), parent_url: "409b89d080ff80798b55f9b618caecc1".into(), ..Default::default() };
        let library = LibraryInfo { id: "L1".into(), name: "示例文献库".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"structuredContent":{"title":"Parent","text":"<content></content>"}}}), ""),
            reply(json!({"id":2,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff80048fe0edb12f9fb033"}]}}}), ""),
        ]);
        let (root, page) = root_client(base).library_page(&index, "destination", "account", &settings, &library).unwrap();
        assert_eq!(root, notion::page_id(&settings.parent_url).unwrap());
        let args = request_body(&server.join().unwrap())["params"]["arguments"].clone();
        assert_eq!(args["parent"]["page_id"], root);
        assert_eq!(args["pages"][0]["properties"]["title"], "示例文献库");
        assert!(args.get("creation_mode").is_none());
        assert!(index.page("destination").unwrap().is_none());
        let path = format!("account/root:{root}/library:{}", notion::fingerprint("L1"));
        assert_eq!(index.page(&path).unwrap().unwrap().id, page.id);
    }
    #[test]
    fn an_empty_root_without_a_parent_creates_a_private_library_and_reuses_it() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { root_title: "  ".into(), ..Default::default() };
        let library = LibraryInfo { id: "L1".into(), name: "示例文献库".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff80048fe0edb12f9fb033"}]}}}), "")]);
        let (root, page) = root_client(base).library_page(&index, "destination", "account", &settings, &library).unwrap();
        assert_eq!(root, notion::PRIVATE_ROOT);
        let args = request_body(&server.join().unwrap())["params"]["arguments"].clone();
        assert_eq!(args["creation_mode"], "draft");
        assert!(args.get("parent").is_none());
        assert_eq!(args["pages"][0]["properties"]["title"], "示例文献库");
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"title":"示例文献库"}}}), "")]);
        let (again, existing) = root_client(base).library_page(&index, "destination", "account", &settings, &library).unwrap();
        assert_eq!((again, existing.id), (root, page.id));
        assert_eq!(request_body(&server.join().unwrap())["params"]["name"], "notion-fetch");
    }
    #[test]
    fn an_omitted_root_still_rejects_a_deleted_parent() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { root_title: String::new(), parent_url: "409b89d080ff80798b55f9b618caecc1".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"in_trash":true}}}), "")]);
        let error = root_client(base).library_page(&index, "destination", "account", &settings, &LibraryInfo::default()).unwrap_err();
        assert!(error.contains("父页面已被删除"));
        assert_eq!(request_body(&server.join().unwrap())["params"]["name"], "notion-fetch");
    }
    #[test]
    fn later_questions_append_flat_even_if_old_conversation_headings_exist() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "追加问题".into(), images: vec![], prompt: String::new() }, ai::Said::Answer { text: "追加回答".into() }], ..Default::default() };
        let mut entry = notion::entries("CA", &transcript, None).remove(0);
        entry.group.as_mut().unwrap().title = "A".into();
        index.keep_group(&format!("scope/group:{}", notion::fingerprint("CA")), "## 对话：A").unwrap();
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"page_id":"page"}}}), "")]);
        let mut client = root_client(base);
        client.tools.insert("notion-update-page".into(), json!({"properties":{}}));
        let page = Page { id: "page".into(), url: "https://example.org/page".into() };
        client.append_entry(&index, "scope", &page, &entry, &entry.markdown(&BTreeMap::new()).unwrap()).unwrap();
        let args = request_body(&server.join().unwrap())["params"]["arguments"].clone();
        assert_eq!(args["command"], "insert_content");
        assert_eq!(args["position"]["type"], "end");
        let added = args["content"].as_str().unwrap();
        assert!(added.contains("追加回答") && !added.contains("## 对话："));
        assert!(index.entry_saved(&format!("scope/{}", entry.key)).unwrap());
    }
    #[test]
    fn a_rejected_new_group_does_not_leave_a_phantom_heading_or_pending_source() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "Q".into(), images: vec![], prompt: String::new() }, ai::Said::Answer { text: "A".into() }], ..Default::default() };
        let entry = notion::entries("CA", &transcript, None).remove(0);
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"isError":true,"content":[{"type":"text","text":"{\"code\":\"validation_error\",\"message\":\"rejected\"}"}]}}), "")]);
        let mut client = root_client(base);
        client.tools.insert("notion-update-page".into(), json!({"properties":{}}));
        let page = Page { id: "page".into(), url: "https://example.org/page".into() };
        assert!(client.append_entry(&index, "scope", &page, &entry, "Markdown").is_err());
        assert!(index.group(&format!("scope/group:{}", notion::fingerprint("CA"))).unwrap().is_none());
        assert!(!index.source_pending(&format!("scope/{}", entry.source_key)).unwrap());
        server.join().unwrap();
    }
    #[test]
    fn configured_parent_gets_a_named_root_before_library_pages() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { parent_url: "https://app.notion.com/p/409b89d080ff80798b55f9b618caecc1".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"structuredContent":{"title":"文献笔记","text":"<content></content>"}}}), ""),
            reply(json!({"id":2,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff80048fe0edb12f9fb033"}]}}}), ""),
        ]);
        let root = root_client(base).root(&index, "root", &settings).unwrap();
        assert_eq!(root.id, "418b89d0-80ff-8004-8fe0-edb12f9fb033");
        let sent = server.join().unwrap();
        let args = request_body(&sent)["params"]["arguments"].clone();
        assert_eq!(args["parent"]["page_id"], "409b89d0-80ff-8079-8b55-f9b618caecc1");
        assert_eq!(args["pages"][0]["properties"]["title"], "WispPaper笔记");
        assert_eq!(args["pages"][0]["content"], "");
        assert_eq!(index.page("root").unwrap().unwrap().id, root.id);
    }
    #[test]
    fn deleted_cached_root_is_recreated_under_the_configured_parent() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        index.keep("root", &Page { id: "old-root".into(), url: "https://example.org/old".into() }).unwrap();
        let settings = notion::Settings { parent_url: "409b89d080ff80798b55f9b618caecc1".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"structuredContent":{"title":"Parent","text":"<content></content>"}}}), ""),
            reply(json!({"id":2,"result":{"structuredContent":{"text":"<page url=\"https://example.org/old\" deleted><blank-page /></page>"}}}), ""),
            reply(json!({"id":3,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff80048fe0edb12f9fb033"}]}}}), ""),
        ]);
        root_client(base).root(&index, "root", &settings).unwrap();
        assert_eq!(index.page("root").unwrap().unwrap().id, "418b89d0-80ff-8004-8fe0-edb12f9fb033");
        assert_eq!(request_body(&server.join().unwrap())["params"]["arguments"]["parent"]["page_id"], "409b89d0-80ff-8079-8b55-f9b618caecc1");
        assert!(!deleted_page(&json!({"text":"<page url=\"root\"><content>the word deleted appears here</content></page>"})));
    }
    #[test]
    fn existing_named_root_is_reused_only_inside_the_selected_parent() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { parent_url: "https://app.notion.com/p/409b89d080ff80798b55f9b618caecc1".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![
            reply(json!({"id":1,"result":{"structuredContent":{"text":"<content>\n<page url=\"https://app.notion.com/p/418b89d080ff80048fe0edb12f9fb033\">\nWispPaper笔记\n</page>\n</content>"}}}), ""),
            reply(json!({"id":2,"result":{"structuredContent":{"title":"WispPaper笔记"}}}), ""),
        ]);
        let mut client = root_client(base);
        client.tools.remove("notion-create-pages");
        let root = client.root(&index, "root", &settings).unwrap();
        assert_eq!(root.id, "418b89d0-80ff-8004-8fe0-edb12f9fb033");
        assert_eq!(request_body(&server.join().unwrap())["params"]["name"], "notion-fetch");
    }
    #[test]
    fn root_url_itself_is_reused_even_with_a_decorated_display_title() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = notion::Settings { parent_url: "https://app.notion.com/p/418b89d080ff80048fe0edb12f9fb033".into(), ..Default::default() };
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"title":"✍🏻 WispPaper笔记","text":"<properties>\n{\"title\":\"WispPaper笔记\"}\n</properties>\n<content></content>"}}}), "")]);
        let root = root_client(base).root(&index, "root", &settings).unwrap();
        assert_eq!(root.id, "418b89d0-80ff-8004-8fe0-edb12f9fb033");
        assert_eq!(request_body(&server.join().unwrap())["params"]["name"], "notion-fetch");
    }
    #[test]
    fn default_root_is_a_private_draft_without_attribution_text() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let (base, server) = crate::tests::serve_in_turn(vec![reply(json!({"id":1,"result":{"structuredContent":{"pages":[{"id":"418b89d080ff80048fe0edb12f9fb033"}]}}}), "")]);
        root_client(base).root(&index, "root", &notion::Settings::default()).unwrap();
        let args = request_body(&server.join().unwrap())["params"]["arguments"].clone();
        assert_eq!(args["creation_mode"], "draft");
        assert!(args.get("parent").is_none());
        assert_eq!(args["pages"][0]["properties"]["title"], "WispPaper笔记");
        assert_eq!(args["pages"][0]["content"], "");
    }
}
