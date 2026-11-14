//! The connectors' transports: an MCP server at an address (Streamable
//! HTTP) or a command whose standard streams speak the protocol. The
//! messages and how they are read are `wisp_core::connector`'s. An address
//! is asked with the connector's own token or, without one, the token of
//! the person's sign-in in the browser (`crate::oauth`), refreshed once when
//! the server refuses it; a server that still refuses wants a sign-in.

use crate::{agent, network_error, oauth};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wisp_core::connector::{self, Connector, Heard, Tool};

/// How long a server has to answer. A first connection may wait for the
/// person to sign in in a browser (`mcp-remote`), so it is generous.
const PATIENCE: Duration = Duration::from_secs(180);

/// Why a connector could not be used.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// The server wants the person to sign in in the browser, and no kept sign-in answers.
    SignIn,
    Failed(String),
}

impl From<String> for Failure {
    fn from(why: String) -> Failure {
        Failure::Failed(why)
    }
}

impl Failure {
    /// The failure as a sentence for the model or the person.
    pub fn said(self, connector: &Connector) -> String {
        match self {
            Failure::SignIn => format!("连接器「{}」需要在浏览器中登录（或登录已失效）：请到 设置 › 自定义 › 连接器 里登录后再试。", connector.name.trim()),
            Failure::Failed(why) => why,
        }
    }
}

// Keep the PKCE URL in memory only while this exact bridge is authorizing.
static NOTION_LOGIN: Mutex<Option<(u32, String)>> = Mutex::new(None);

pub fn pending_notion_login(connector: &Connector) -> Option<String> {
    if !crate::notion::notion_bridge(&connector.target) { return None }
    NOTION_LOGIN.lock().ok()?.as_ref().map(|(_, url)| url.clone())
}

fn clear_login(pid: u32) {
    if let Ok(mut login) = NOTION_LOGIN.lock() && login.as_ref().is_some_and(|(owner, _)| *owner == pid) { *login = None; }
}

enum Link {
    /// `token` is the connector's own; `bearer` the sign-in's, when it has none.
    /// `version` is the protocol revision the server answered with.
    Http { url: String, token: String, bearer: Option<String>, session: Option<String>, version: String },
    Stdio { child: Child, stdin: ChildStdin, lines: Receiver<String>, errors: Arc<Mutex<String>> },
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Link::Stdio { child, .. } = self {
            clear_login(child.id());
            #[cfg(windows)]
            if child.try_wait().is_ok_and(|status| status.is_none()) {
                // cmd -> npx -> node: killing only cmd leaves the OAuth callback
                // listener alive and subsequent connections wait on that orphan.
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill.exe").args(["/T", "/F", "/PID", &child.id().to_string()])
                    .creation_flags(0x0800_0000).stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Client {
    link: Link,
    next: u64,
}

impl Client {
    fn connect(connector: &Connector) -> Result<Client, Failure> {
        let target = connector.target.trim();
        if target.is_empty() { return Err(Failure::Failed("还没有填写地址或命令。".into())) }
        let link = if connector.remote() {
            let token = connector.token.trim().to_string();
            let bearer = if token.is_empty() { oauth::bearer(target)? } else { None };
            Link::Http { url: target.to_string(), token, bearer, session: None, version: String::new() }
        } else {
            spawn(target)?
        };
        let mut client = Client { link, next: 1 };
        let answered = client.exchange(&connector::initialize(0), Some(0))?;
        // Every later request says the revision agreed on.
        if let Link::Http { version, .. } = &mut client.link { *version = answered["protocolVersion"].as_str().unwrap_or(connector::VERSION).to_string() }
        client.exchange(&connector::initialized(), None)?;
        Ok(client)
    }

    fn ask(&mut self, method: &str, params: Value) -> Result<Value, Failure> {
        self.next += 1;
        self.exchange(&connector::request(self.next, method, params), Some(self.next))
    }

    /// Send a message and, when it has an id, wait for its reply.
    fn exchange(&mut self, message: &Value, id: Option<u64>) -> Result<Value, Failure> {
        match &mut self.link {
            Link::Http { url, token, bearer, session, version } => {
                let mut renewed = false;
                let mut response = loop {
                    let mut call = agent(Some(PATIENCE), "")?.post(url.as_str()).header("content-type", "application/json").header("accept", "application/json, text/event-stream");
                    if !version.is_empty() { call = call.header("mcp-protocol-version", version.as_str()) }
                    let sent = if token.is_empty() { bearer.as_deref() } else { Some(token.as_str()) };
                    if let Some(sent) = sent { call = call.header("authorization", &format!("Bearer {sent}")) }
                    if let Some(session) = session.as_deref() { call = call.header("mcp-session-id", session) }
                    let response = call.send(message.to_string().as_str()).map_err(network_error)?;
                    if response.status().as_u16() != 401 { break response }
                    if !token.is_empty() { return Err(Failure::Failed("连接器不接受这个令牌（HTTP 401）。请检查令牌；要在浏览器中登录，就把令牌留空。".into())) }
                    // The sign-in's token ran out early or was taken back: refreshed once, then it is a new sign-in.
                    if !renewed && let Some(rejected) = bearer.take() {
                        renewed = true;
                        *bearer = oauth::renew(url, &rejected)?;
                        if bearer.is_some() { continue }
                    }
                    return Err(Failure::SignIn);
                };
                let status = response.status().as_u16();
                if status == 403 { return Err(Failure::Failed("连接器拒绝了访问（HTTP 403）：这个账号或令牌没有使用它的权限。".into())) }
                if !(200..300).contains(&status) { return Err(Failure::Failed(format!("连接器返回 HTTP {status}：{}", response.body_mut().read_to_string().unwrap_or_default().chars().take(200).collect::<String>()))) }
                if let Some(given) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) { *session = Some(given.to_string()) }
                let Some(id) = id else { return Ok(Value::Null) };
                // The reply is the body, or one event of a stream that may carry others before it.
                for line in BufReader::new(response.body_mut().as_reader()).lines() {
                    let line = line.map_err(|err| format!("连接中断：{err}"))?;
                    if let Heard::Reply(reply) = connector::hear(line.strip_prefix("data:").unwrap_or(&line), id) { return Ok(reply?) }
                }
                Err(Failure::Failed("连接器没有应答。".into()))
            }
            Link::Stdio { stdin, lines, errors, .. } => {
                let gone = |errors: &Arc<Mutex<String>>| {
                    let said = errors.lock().map(|said| said.trim().to_string()).unwrap_or_default();
                    if said.is_empty() { "连接器的进程没有应答。".to_string() } else { format!("连接器的进程没有应答：{said}") }
                };
                let mut send = |message: &Value| writeln!(stdin, "{message}").and_then(|()| stdin.flush());
                if send(message).is_err() { return Err(Failure::Failed(gone(errors))) }
                let Some(id) = id else { return Ok(Value::Null) };
                loop {
                    let line = match lines.recv_timeout(PATIENCE) {
                        Ok(line) => line,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err(Failure::Failed("连接器等待应答超时；如浏览器已打开，请完成授权后重新连接。".into())),
                        Err(_) => return Err(Failure::Failed(gone(errors))),
                    };
                    match connector::hear(&line, id) {
                        Heard::Reply(reply) => return Ok(reply?),
                        Heard::Answer(answer) => if send(&answer).is_err() { return Err(Failure::Failed(gone(errors))) },
                        Heard::Nothing => {}
                    }
                }
            }
        }
    }

    fn tools(&mut self, connector: &Connector) -> Result<Vec<Tool>, Failure> {
        let (mut tools, mut cursor) = (vec![], Value::Null);
        // A long list comes in pages; twenty of them is more tools than a model can be told of.
        for _ in 0..20 {
            let result = self.ask("tools/list", if cursor.is_null() { json!({}) } else { json!({"cursor": cursor}) })?;
            tools.extend(connector::read_tools(connector, &result));
            cursor = result["nextCursor"].clone();
            if cursor.is_null() { break }
        }
        Ok(tools)
    }
}

/// Start a command the way a terminal would, so that it finds what the
/// person's shell finds (`npx`, `uvx`): an app started from the Dock has a bare PATH.
fn spawn(line: &str) -> Result<Link, String> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        // No console window flashes up behind the app.
        // Some launchers pass a partial PATHEXT. Keep custom extensions but
        // restore the standard ones needed by npm's npx.cmd and node.exe.
        let mut extensions = std::env::var("PATHEXT").unwrap_or_default();
        for ext in [".COM", ".EXE", ".BAT", ".CMD"] {
            if !extensions.split(';').any(|e| e.eq_ignore_ascii_case(ext)) {
                if !extensions.is_empty() { extensions.push(';') }
                extensions.push_str(ext);
            }
        }
        command.env("PATHEXT", extensions);
        // /S strips the outer pair. Pass the command as shell syntax:
        // Command::arg would escape it as one executable name, spaces included.
        command.args(["/D", "/S", "/C"]).raw_arg(format!("\"{line}\"")).creation_flags(0x0800_0000);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
        command.args(["-lc", line]);
        command
    };
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|err| format!("无法启动连接器的命令：{err}"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else { return Err("无法启动连接器的命令。".into()) };
    let (sender, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() { break }
        }
    });
    // What it last said on its error stream is the reason when it stops answering.
    let errors = Arc::new(Mutex::new(String::new()));
    let kept = errors.clone();
    let official = crate::notion::notion_bridge(line);
    let pid = child.id();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if official && let Ok(url) = url::Url::parse(line.trim())
                && url.origin().ascii_serialization() == "https://mcp.notion.com"
                && url.path() == "/authorize" && url.query_pairs().any(|(k, _)| k == "code_challenge")
            {
                if let Ok(mut login) = NOTION_LOGIN.lock() { *login = Some((pid, url.into())); }
                continue;
            }
            if !line.trim().is_empty() && let Ok(mut said) = kept.lock() { *said = line.chars().take(300).collect() }
        }
    });
    Ok(Link::Stdio { child, stdin, lines, errors })
}

type Shared = Arc<Mutex<Client>>;

/// The open connections, each under its connector as written: a connector changed is another connection.
static CLIENTS: Mutex<Option<HashMap<String, Shared>>> = Mutex::new(None);

fn key(connector: &Connector) -> String {
    format!("{}\n{}", connector.target.trim(), connector.token.trim())
}

fn kept(connector: &Connector) -> Option<Shared> {
    CLIENTS.lock().ok()?.as_ref()?.get(&key(connector)).cloned()
}

fn keep(connector: &Connector, client: Client) -> Shared {
    let shared = Arc::new(Mutex::new(client));
    if let Ok(mut clients) = CLIENTS.lock() { clients.get_or_insert_default().insert(key(connector), shared.clone()); }
    shared
}

/// Close a connector's connection (signed out: the next call connects anew).
pub fn disconnect(connector: &Connector) {
    if let Ok(mut clients) = CLIENTS.lock() && let Some(clients) = clients.as_mut() { clients.remove(&key(connector)); }
}

/// Borrow an already authorized session. Manual writes must never start a second
/// bridge or replay a request whose outcome is unknown.
pub(crate) struct Session(Shared);

impl Session {
    pub(crate) fn existing(connector: &Connector) -> Result<Self, String> {
        kept(connector).map(Self).ok_or_else(|| "请在「设置 → 自定义」连接 Notion，完成浏览器授权，等到显示已连接后再试。".into())
    }

    pub(crate) fn ask(&self, method: &str, params: Value) -> Result<Value, String> {
        self.0.lock().map_err(|_| "Notion 连接不可用，请重新连接。".to_string())?.ask(method, params).map_err(|e| match e { Failure::SignIn => "请重新登录连接器。".into(), Failure::Failed(why) => why })
    }
}

/// Close the connections of connectors that are not among `wanted` any more.
pub fn keep_only(wanted: &[Connector]) {
    let keys: Vec<String> = wanted.iter().map(key).collect();
    if let Ok(mut clients) = CLIENTS.lock() && let Some(clients) = clients.as_mut() { clients.retain(|key, _| keys.contains(key)) }
}

/// Connect to a connector anew and list its tools.
pub fn tools(connector: &Connector) -> Result<Vec<Tool>, Failure> {
    let mut client = Client::connect(connector)?;
    let tools = client.tools(connector)?;
    if let Link::Stdio { child, .. } = &client.link { clear_login(child.id()); }
    keep(connector, client);
    Ok(tools)
}

/// Call a connector's tool by the server's name for it. A connection that
/// broke meanwhile (a session that ran out, a process that stopped) is made
/// once more; a failure the tool itself reports is not tried again.
pub fn call(connector: &Connector, tool: &str, args: &Value) -> Result<String, String> {
    let params = json!({"name": tool, "arguments": args});
    let ask = |shared: &Shared| shared.lock().map_err(|_| Failure::Failed("连接器上一次调用出了错。".into()))?.ask("tools/call", params.clone());
    let first = match kept(connector) {
        Some(shared) => ask(&shared),
        None => Err(Failure::SignIn),
    };
    let result = match first {
        Ok(result) => result,
        Err(_) => Client::connect(connector).and_then(|client| ask(&keep(connector, client))).map_err(|failure| failure.said(connector))?,
    };
    connector::read_result(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_asked_over_http_with_its_token_and_session() {
        let (base, served) = crate::tests::serve_in_turn(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s1\r\nConnection: close\r\nContent-Length: 36\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}",
            "HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            // An event stream: a notification first, then the reply.
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"search\",\"description\":\"Find\",\"annotations\":{\"readOnlyHint\":true}}]}}\n\n",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 82\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"two pages\"}]}}  ",
        ]);
        let connector = Connector { name: "Notes".into(), target: format!("{base}/mcp"), token: "secret".into(), off: false };
        let tools = tools(&connector).unwrap();
        assert_eq!((tools.len(), tools[0].name.as_str(), tools[0].read_only), (1, "notes__search", true));
        assert_eq!(call(&connector, "search", &json!({"query": "auxin"})), Ok("two pages".into()));
        let last = served.join().unwrap().to_lowercase();
        assert!(last.contains("authorization: bearer secret") && last.contains("mcp-session-id: s1") && last.contains(r#""name":"search""#), "{last}");
        keep_only(&[]);
        assert!(kept(&connector).is_none());
    }

    #[test]
    fn a_refused_address_wants_a_sign_in_or_another_token() {
        crate::oauth::tests::private_file();
        let refused = "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
        let (base, _served) = crate::tests::serve(refused);
        assert_eq!(tools(&Connector { target: base, ..Default::default() }).unwrap_err(), Failure::SignIn);
        // A token of the connector's own is not swapped for a sign-in: it is wrong.
        let (base, _served) = crate::tests::serve(refused);
        let error = tools(&Connector { target: base, token: "wrong".into(), ..Default::default() }).unwrap_err();
        assert!(matches!(&error, Failure::Failed(why) if why.contains("令牌")), "{error:?}");
    }

    #[test]
    fn an_address_is_asked_with_the_browser_sign_in() {
        let connector = Connector { name: "Notes".into(), target: crate::oauth::tests::signed_in_server(), ..Default::default() };
        let listed = tools(&connector).unwrap();
        // The server answered with an older revision, and is asked in it from then on.
        assert_eq!(listed[0].description, "[Notes] negotiated");
        let other = Connector { name: "Other".into(), target: crate::oauth::tests::unsigned_server(), ..Default::default() };
        assert_eq!(tools(&other).unwrap_err(), Failure::SignIn);
        let error = call(&other, "whoami", &json!({})).unwrap_err();
        assert!(error.contains("「Other」") && error.contains("设置 › 自定义"), "{error}");
        disconnect(&connector);
        assert!(kept(&connector).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_preserves_arguments_and_quoted_paths() {
        let Link::Stdio { ref lines, .. } = spawn(r#"echo connected "two words""#).unwrap() else { unreachable!() };
        assert_eq!(lines.recv_timeout(Duration::from_secs(5)).unwrap(), r#"connected "two words""#);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_is_asked_over_its_standard_streams() {
        // A server in a few lines of shell: it answers by the id it reads, after asking a ping of its own.
        let script = r#"while read -r line; do id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p'); case "$line" in *tools/list*) echo '{"jsonrpc":"2.0","id":"p","method":"ping"}'; echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}";; *tools/call*) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}";; *'"id"'*) [ -n "$id" ] && echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{}}";; esac; done"#;
        let connector = Connector { name: "本机".into(), target: script.into(), ..Default::default() };
        let tools = tools(&connector).unwrap();
        assert_eq!((tools[0].name.as_str(), tools[0].read_only), ("connector__echo", false));
        assert_eq!(call(&connector, "echo", &json!({})), Ok("hello".into()));
        keep_only(&[]);
        // A command that is not there says so in the shell's words.
        let error = super::tools(&Connector { target: "wisp-no-such-command-96".into(), ..Default::default() }).unwrap_err();
        assert!(matches!(&error, Failure::Failed(why) if why.contains("没有应答")), "{error:?}");
    }
}
