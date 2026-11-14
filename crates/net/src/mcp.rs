//! The connectors' transports: an MCP server at an address (Streamable
//! HTTP) or a command whose standard streams speak the protocol. The
//! messages and how they are read are `wisp_core::connector`'s.

use crate::{agent, network_error};
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

enum Link {
    Http { url: String, token: String, session: Option<String> },
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
    fn connect(connector: &Connector) -> Result<Client, String> {
        let target = connector.target.trim();
        if target.is_empty() { return Err("还没有填写地址或命令。".into()) }
        let link = if connector.remote() { Link::Http { url: target.to_string(), token: connector.token.trim().to_string(), session: None } } else { spawn(target)? };
        let mut client = Client { link, next: 1 };
        client.exchange(&connector::initialize(0), Some(0))?;
        client.exchange(&connector::initialized(), None)?;
        Ok(client)
    }

    fn ask(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        self.exchange(&connector::request(self.next, method, params), Some(self.next))
    }

    /// Send a message and, when it has an id, wait for its reply.
    fn exchange(&mut self, message: &Value, id: Option<u64>) -> Result<Value, String> {
        match &mut self.link {
            Link::Http { url, token, session } => {
                let mut call = agent(Some(PATIENCE), "")?.post(url.as_str()).header("content-type", "application/json").header("accept", "application/json, text/event-stream").header("mcp-protocol-version", connector::VERSION);
                if !token.is_empty() { call = call.header("authorization", &format!("Bearer {token}")) }
                if let Some(session) = session.as_deref() { call = call.header("mcp-session-id", session) }
                let mut response = call.send(message.to_string().as_str()).map_err(network_error)?;
                let status = response.status().as_u16();
                if status == 401 || status == 403 { return Err(format!("连接器拒绝了访问（HTTP {status}）。需要令牌的服务请填写令牌；只支持浏览器登录（OAuth）的服务，可以把地址换成命令：npx -y mcp-remote {url}")) }
                if !(200..300).contains(&status) { return Err(format!("连接器返回 HTTP {status}：{}", response.body_mut().read_to_string().unwrap_or_default().chars().take(200).collect::<String>())) }
                if let Some(given) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) { *session = Some(given.to_string()) }
                let Some(id) = id else { return Ok(Value::Null) };
                // The reply is the body, or one event of a stream that may carry others before it.
                for line in BufReader::new(response.body_mut().as_reader()).lines() {
                    let line = line.map_err(|err| format!("连接中断：{err}"))?;
                    if let Heard::Reply(reply) = connector::hear(line.strip_prefix("data:").unwrap_or(&line), id) { return reply }
                }
                Err("连接器没有应答。".into())
            }
            Link::Stdio { stdin, lines, errors, .. } => {
                let gone = |errors: &Arc<Mutex<String>>| {
                    let said = errors.lock().map(|said| said.trim().to_string()).unwrap_or_default();
                    if said.is_empty() { "连接器的进程没有应答。".to_string() } else { format!("连接器的进程没有应答：{said}") }
                };
                let mut send = |message: &Value| writeln!(stdin, "{message}").and_then(|()| stdin.flush());
                if send(message).is_err() { return Err(gone(errors)) }
                let Some(id) = id else { return Ok(Value::Null) };
                loop {
                    let Ok(line) = lines.recv_timeout(PATIENCE) else { return Err(gone(errors)) };
                    match connector::hear(&line, id) {
                        Heard::Reply(reply) => return reply,
                        Heard::Answer(answer) => if send(&answer).is_err() { return Err(gone(errors)) },
                        Heard::Nothing => {}
                    }
                }
            }
        }
    }

    fn tools(&mut self, connector: &Connector) -> Result<Vec<Tool>, String> {
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

/// Close the connections of connectors that are not among `wanted` any more.
pub fn keep_only(wanted: &[Connector]) {
    let keys: Vec<String> = wanted.iter().map(key).collect();
    if let Ok(mut clients) = CLIENTS.lock() && let Some(clients) = clients.as_mut() { clients.retain(|key, _| keys.contains(key)) }
}

/// Connect to a connector anew and list its tools.
pub fn tools(connector: &Connector) -> Result<Vec<Tool>, String> {
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
    let ask = |shared: &Shared| shared.lock().map_err(|_| "连接器上一次调用出了错。".to_string())?.ask("tools/call", params.clone());
    let first = match kept(connector) {
        Some(shared) => ask(&shared),
        None => Err(String::new()),
    };
    let result = match first {
        Ok(result) => result,
        Err(_) => ask(&keep(connector, Client::connect(connector)?))?,
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
    fn a_refused_address_says_what_to_do() {
        let (base, _served) = crate::tests::serve("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
        let error = tools(&Connector { target: base, ..Default::default() }).unwrap_err();
        assert!(error.contains("mcp-remote"), "{error}");
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
        assert!(error.contains("没有应答"), "{error}");
    }
}
