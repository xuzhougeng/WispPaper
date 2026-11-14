//! The connectors' transports: an MCP server at an address (Streamable
//! HTTP, or for a server from before it, HTTP with an event stream) or a
//! command whose standard streams speak the protocol. The messages and how
//! they are read are `wisp_core::connector`'s. An address
//! is asked with the connector's own token or, without one, the token of
//! the person's sign-in in the browser (`crate::oauth`), refreshed once when
//! the server refuses it; a server that still refuses wants a sign-in.

use crate::{agent, network_error, oauth};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ureq::Body;
use ureq::http::Response;
use wisp_core::connector::{self, Connector, Events, Heard, Tool};

/// How long a server has to answer. A first connection may wait for the
/// person to sign in in a browser (`mcp-remote`), so it is generous.
const PATIENCE: Duration = Duration::from_secs(180);

/// How long a server's event stream has to say where messages go: at once, as a rule.
const ENDPOINT_WAIT: Duration = Duration::from_secs(30);

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

/// What an address is asked with: the connector's own token or, without
/// one, the sign-in's. `url` is the connector's address, which the sign-in is kept under.
struct Auth {
    url: String,
    token: String,
    bearer: Option<String>,
}

impl Auth {
    fn of(target: &str, token: &str) -> Result<Auth, String> {
        let token = token.trim().to_string();
        let bearer = if token.is_empty() { oauth::bearer(target)? } else { None };
        Ok(Auth { url: target.to_string(), token, bearer })
    }

    /// Send the request `ask` makes with an authorization header, if there is
    /// one to send; when the server refuses the sign-in's token it is
    /// refreshed once, and a server that still refuses wants a new sign-in.
    fn send(&mut self, ask: impl Fn(Option<&str>) -> Result<Response<Body>, String>) -> Result<Response<Body>, Failure> {
        let mut renewed = false;
        loop {
            let sent = if self.token.is_empty() { self.bearer.as_deref() } else { Some(self.token.as_str()) };
            let response = ask(sent.map(|sent| format!("Bearer {sent}")).as_deref())?;
            if response.status().as_u16() != 401 { return Ok(response) }
            if !self.token.is_empty() { return Err(Failure::Failed("连接器不接受这个令牌（HTTP 401）。请检查令牌；要在浏览器中登录，就把令牌留空。".into())) }
            // The sign-in's token ran out early or was taken back: refreshed once, then it is a new sign-in.
            if !renewed && let Some(rejected) = self.bearer.take() {
                renewed = true;
                self.bearer = oauth::renew(&self.url, &rejected)?;
                if self.bearer.is_some() { continue }
            }
            return Err(Failure::SignIn);
        }
    }
}

/// What a response that is no success says, as a failure.
fn refusal(response: &mut Response<Body>) -> Failure {
    let status = response.status().as_u16();
    if status == 403 { return Failure::Failed("连接器拒绝了访问（HTTP 403）：这个账号或令牌没有使用它的权限。".into()) }
    Failure::Failed(format!("连接器返回 HTTP {status}：{}", response.body_mut().read_to_string().unwrap_or_default().chars().take(200).collect::<String>()))
}

fn succeeded(response: &mut Response<Body>) -> Result<(), Failure> {
    if response.status().is_success() { Ok(()) } else { Err(refusal(response)) }
}

/// Streamable HTTP: every message is posted to the address, and its reply
/// is the response's body or an event of the stream the response is.
struct Http {
    auth: Auth,
    session: Option<String>,
    /// The protocol revision the server answered with.
    version: String,
}

impl Http {
    fn post(&mut self, message: &Value) -> Result<Response<Body>, Failure> {
        let (url, body) = (self.auth.url.clone(), message.to_string());
        let (session, version) = (&self.session, &self.version);
        self.auth.send(|authorization| {
            let mut call = agent(Some(PATIENCE), "")?.post(url.as_str()).header("content-type", "application/json").header("accept", "application/json, text/event-stream");
            if !version.is_empty() { call = call.header("mcp-protocol-version", version.as_str()) }
            if let Some(authorization) = authorization { call = call.header("authorization", authorization) }
            if let Some(session) = session.as_deref() { call = call.header("mcp-session-id", session) }
            call.send(body.as_str()).map_err(network_error)
        })
    }

    /// The reply to `id` in the response to a post.
    fn read(&mut self, mut response: Response<Body>, id: Option<u64>) -> Result<Value, Failure> {
        succeeded(&mut response)?;
        if let Some(given) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) { self.session = Some(given.to_string()) }
        let Some(id) = id else { return Ok(Value::Null) };
        let stream = response.headers().get("content-type").and_then(|value| value.to_str().ok()).is_some_and(|kind| kind.contains("event-stream"));
        // The reply is the body, or one event of a stream that may carry others before it.
        let mut said = String::new();
        for line in BufReader::new(response.body_mut().as_reader()).lines() {
            let line = line.map_err(|err| format!("连接中断：{err}"))?;
            if let Heard::Reply(reply) = connector::hear(line.strip_prefix("data:").unwrap_or(&line), id) { return Ok(reply?) }
            if !stream && said.is_empty() { said = line.trim().chars().take(200).collect() }
        }
        // A body that is no reply is usually the service's own complaint (a key it does not know).
        Err(Failure::Failed(if said.is_empty() { "连接器没有应答。".into() } else { format!("连接器没有应答，它说：{said}") }))
    }

    fn exchange(&mut self, message: &Value, id: Option<u64>) -> Result<Value, Failure> {
        let response = self.post(message)?;
        self.read(response, id)
    }
}

/// HTTP with an event stream, the transport before Streamable HTTP
/// (2024-11-05): the replies come on a stream opened at the address, and
/// messages are posted to the address its first event names.
struct Sse {
    auth: Auth,
    endpoint: String,
    lines: Receiver<String>,
    events: Events,
}

impl Sse {
    /// Open the stream at the address that refused a post; `refused` is
    /// what is said when the address has no stream either.
    fn open(mut auth: Auth, refused: Failure) -> Result<Sse, Failure> {
        let url = auth.url.clone();
        let mut response = auth.send(|authorization| {
            // The stream lasts as long as the connection: no overall timeout.
            let mut call = agent(None, "")?.get(url.as_str()).header("accept", "text/event-stream");
            if let Some(authorization) = authorization { call = call.header("authorization", authorization) }
            call.call().map_err(network_error)
        })?;
        if response.status().as_u16() == 403 { return Err(refusal(&mut response)) }
        let streams = response.headers().get("content-type").and_then(|value| value.to_str().ok()).is_some_and(|kind| kind.contains("event-stream"));
        if !response.status().is_success() || !streams { return Err(refused) }
        let (sender, lines) = channel();
        let reader = response.into_body().into_reader();
        // Ends when the server closes the stream, or at its next line once the connection is let go.
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if sender.send(line).is_err() { break }
            }
        });
        let mut sse = Sse { auth, endpoint: String::new(), lines, events: Events::default() };
        let deadline = Instant::now() + ENDPOINT_WAIT;
        loop {
            let Ok(event) = sse.next(deadline) else { return Err(Failure::Failed("连接器的事件流没有说消息发到哪里（endpoint），这可能不是 MCP 服务的地址。".into())) };
            if event.name != "endpoint" { continue }
            let Some(endpoint) = connector::endpoint(&sse.auth.url, &event.data) else { return Err(Failure::Failed(format!("连接器要把消息发到另一个网站（{}），为了令牌的安全没有照做。", event.data.trim()))) };
            sse.endpoint = endpoint;
            return Ok(sse);
        }
    }

    /// The next event of the stream, by `deadline`.
    fn next(&mut self, deadline: Instant) -> Result<connector::Event, Failure> {
        loop {
            match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => if let Some(event) = self.events.line(&line) { return Ok(event) },
                Err(RecvTimeoutError::Timeout) => return Err(Failure::Failed("连接器没有应答。".into())),
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Failed("连接器的事件流断开了。".into())),
            }
        }
    }

    fn post(&mut self, message: &Value) -> Result<(), Failure> {
        let (endpoint, body) = (self.endpoint.clone(), message.to_string());
        let mut response = self.auth.send(|authorization| {
            let mut call = agent(Some(PATIENCE), "")?.post(endpoint.as_str()).header("content-type", "application/json");
            if let Some(authorization) = authorization { call = call.header("authorization", authorization) }
            call.send(body.as_str()).map_err(network_error)
        })?;
        succeeded(&mut response)
    }

    /// Post a message and, when it has an id, wait for its reply on the
    /// stream, answering by post what the server asks meanwhile (a ping).
    fn exchange(&mut self, message: &Value, id: Option<u64>) -> Result<Value, Failure> {
        self.post(message)?;
        let Some(id) = id else { return Ok(Value::Null) };
        let deadline = Instant::now() + PATIENCE;
        loop {
            let event = self.next(deadline)?;
            if event.name != "message" { continue }
            match connector::hear(&event.data, id) {
                Heard::Reply(reply) => return Ok(reply?),
                Heard::Answer(answer) => self.post(&answer)?,
                Heard::Nothing => {}
            }
        }
    }
}

enum Link {
    Http(Http),
    Sse(Sse),
    Stdio { child: Child, stdin: ChildStdin, lines: Receiver<String>, errors: Arc<Mutex<String>> },
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Link::Stdio { child, .. } = self {
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
        let mut client = if connector.remote() { Client { link: remote(target, &connector.token)?, next: 1 } } else {
            let mut client = Client { link: spawn(target)?, next: 1 };
            client.exchange(&connector::initialize(0), Some(0))?;
            client
        };
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
            Link::Http(http) => http.exchange(message, id),
            Link::Sse(sse) => sse.exchange(message, id),
            Link::Stdio { stdin, lines, errors, .. } => {
                let gone = |errors: &Arc<Mutex<String>>| {
                    let said = errors.lock().map(|said| said.trim().to_string()).unwrap_or_default();
                    if said.is_empty() { "连接器的进程没有应答。".to_string() } else { format!("连接器的进程没有应答：{said}") }
                };
                let mut send = |message: &Value| writeln!(stdin, "{message}").and_then(|()| stdin.flush());
                if send(message).is_err() { return Err(Failure::Failed(gone(errors))) }
                let Some(id) = id else { return Ok(Value::Null) };
                loop {
                    let Ok(line) = lines.recv_timeout(PATIENCE) else { return Err(Failure::Failed(gone(errors))) };
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

/// Initialize the server at an address, over Streamable HTTP or, when the
/// address takes no post (400, 404, 405), over the event stream of a server
/// from before it, as the protocol's revision of 2025-03-26 has clients do.
fn remote(target: &str, token: &str) -> Result<Link, Failure> {
    let hello = connector::initialize(0);
    let mut http = Http { auth: Auth::of(target, token)?, session: None, version: String::new() };
    let mut response = http.post(&hello)?;
    if matches!(response.status().as_u16(), 400 | 404 | 405) {
        let refused = refusal(&mut response);
        let mut sse = Sse::open(http.auth, refused)?;
        sse.exchange(&hello, Some(0))?;
        return Ok(Link::Sse(sse));
    }
    let answered = http.read(response, Some(0))?;
    // Every later request says the revision agreed on.
    http.version = answered["protocolVersion"].as_str().unwrap_or(connector::VERSION).to_string();
    Ok(Link::Http(http))
}

/// Start a command the way a terminal would, so that it finds what the
/// person's shell finds (`npx`, `uvx`): an app started from the Dock has a bare PATH.
fn spawn(line: &str) -> Result<Link, String> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        // No console window flashes up behind the app.
        command.args(["/C", line]).creation_flags(0x0800_0000);
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
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
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

/// Close the connections of connectors that are not among `wanted` any more.
pub fn keep_only(wanted: &[Connector]) {
    let keys: Vec<String> = wanted.iter().map(key).collect();
    if let Ok(mut clients) = CLIENTS.lock() && let Some(clients) = clients.as_mut() { clients.retain(|key, _| keys.contains(key)) }
}

/// Connect to a connector anew and list its tools.
pub fn tools(connector: &Connector) -> Result<Vec<Tool>, Failure> {
    let mut client = Client::connect(connector)?;
    let tools = client.tools(connector)?;
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

    /// A server of the transport before Streamable HTTP, as the SDKs of
    /// 2024 made them: the address takes no post, its event stream names where
    /// to post, and every reply comes on the stream. Before it lists its tools
    /// it asks a ping of its own, and lists them once the ping is answered.
    /// Returns the address and the requests it heard (the first line and the head).
    fn old_server() -> (String, Arc<Mutex<Vec<String>>>) {
        use std::net::{TcpListener, TcpStream};
        // One event, as one chunk of the stream.
        fn say(stream: &mut Option<TcpStream>, event: &str) {
            let chunk = format!("{event}\n\n");
            let _ = write!(stream.as_mut().unwrap(), "{:x}\r\n{chunk}\r\n", chunk.len());
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let heard: Arc<Mutex<Vec<String>>> = Arc::default();
        let kept = heard.clone();
        std::thread::spawn(move || {
            let (mut stream, mut listing) = (None, Value::Null);
            for connection in listener.incoming() {
                let Ok(mut connection) = connection else { continue };
                let (method, path, head, body) = crate::oauth::tests::read_request(&mut connection);
                kept.lock().unwrap().push(format!("{method} {path}\n{head}"));
                match (method.as_str(), path.split('?').next().unwrap_or("")) {
                    ("GET", "/sse") => {
                        let _ = connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n\r\n");
                        stream = Some(connection);
                        say(&mut stream, ": connected");
                        say(&mut stream, "event: endpoint\ndata: /messages?session_id=s7");
                    }
                    ("POST", "/messages") => {
                        let _ = connection.write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 8\r\n\r\nAccepted");
                        let message: Value = serde_json::from_str(&body).unwrap();
                        let reply = |id: &Value, result: Value| format!("event: message\ndata: {}", json!({"jsonrpc": "2.0", "id": id, "result": result}));
                        match message["method"].as_str() {
                            Some("initialize") => say(&mut stream, &reply(&message["id"], json!({"protocolVersion": "2024-11-05"}))),
                            Some("tools/list") => {
                                listing = message["id"].clone();
                                say(&mut stream, &format!("event: message\ndata: {}", json!({"jsonrpc": "2.0", "id": "p", "method": "ping"})));
                            }
                            Some("tools/call") => say(&mut stream, &reply(&message["id"], json!({"content": [{"type": "text", "text": message["params"]["arguments"]["text"]}]}))),
                            None if message["id"] == "p" => say(&mut stream, &reply(&listing, json!({"tools": [{"name": "echo", "annotations": {"readOnlyHint": true}}]}))),
                            _ => {}
                        }
                    }
                    _ => { let _ = connection.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"); }
                }
            }
        });
        (base, heard)
    }

    #[test]
    fn an_old_server_is_asked_over_its_event_stream() {
        let (base, heard) = old_server();
        let connector = Connector { name: "Old".into(), target: format!("{base}/sse"), token: "secret".into(), off: false };
        let tools = tools(&connector).unwrap();
        assert_eq!(tools.iter().map(|tool| (tool.name.as_str(), tool.read_only)).collect::<Vec<_>>(), [("old__echo", true)]);
        assert_eq!(call(&connector, "echo", &json!({"text": "hello"})), Ok("hello".into()));
        keep_only(&[]);
        let heard = heard.lock().unwrap().clone();
        // The post at the address is refused, the stream opened there, and every message
        // after it posted to the endpoint: initialize, initialized, tools/list, the ping's answer, tools/call.
        let firsts: Vec<&str> = heard.iter().map(|request| request.lines().next().unwrap_or("")).collect();
        assert_eq!(firsts, [&["POST /sse", "GET /sse"][..], &["POST /messages?session_id=s7"; 5]].concat());
        assert!(heard[1].contains("accept: text/event-stream"), "{}", heard[1]);
        assert!(heard.iter().all(|request| request.contains("authorization: bearer secret")), "{heard:?}");
    }

    #[test]
    fn an_old_server_wants_the_sign_in_too_and_an_address_of_neither_says_so() {
        crate::oauth::tests::private_file();
        let answer = |status: &str| format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
        // The stream refuses without a token: a sign-in is wanted, as at the post.
        let base = crate::oauth::tests::route(move |method, _, _, _| answer(if method == "GET" { "401 Unauthorized" } else { "405 Method Not Allowed" }));
        assert_eq!(tools(&Connector { target: format!("{base}/sse"), ..Default::default() }).unwrap_err(), Failure::SignIn);
        // No stream either: what the post was answered is the reason.
        let base = crate::oauth::tests::route(move |_, _, _, _| answer("404 Not Found"));
        let error = tools(&Connector { target: format!("{base}/nothing"), ..Default::default() }).unwrap_err();
        assert!(matches!(&error, Failure::Failed(why) if why.contains("HTTP 404")), "{error:?}");
        // A body that is no reply is said.
        let base = crate::oauth::tests::route(|_, _, _, _| crate::oauth::tests::json("200 OK", r#"{"status":"0","info":"INVALID_USER_KEY"}"#));
        let error = tools(&Connector { target: format!("{base}/sse"), ..Default::default() }).unwrap_err();
        assert!(matches!(&error, Failure::Failed(why) if why.contains("INVALID_USER_KEY")), "{error:?}");
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
