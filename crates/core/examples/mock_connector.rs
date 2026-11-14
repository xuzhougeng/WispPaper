//! A stand-in for a remote MCP server that wants a sign-in in the browser,
//! for trying 设置 › 自定义 › 连接器 without an account anywhere:
//!
//!     cargo run -p wisp_core --example mock_connector -- 8790 [seconds]
//!
//! Add a connector with the address `http://localhost:8790/mcp` and no
//! token. The server is its own authorization server, the way most are: a
//! request without a good token is answered 401 with the address of its
//! protected resource metadata, which names it as the authorization server;
//! its metadata offers registration, and `/authorize` shows a page with
//! 允许 and 拒绝 that sends the browser back to the app. Codes are checked
//! against the PKCE challenge, refresh tokens are used once (rotated), and
//! each access token is good for the given number of seconds (an hour by
//! default; a short one shows the refresh). Its tools are `whoami`, which
//! only reads, and `add_note`, which the assistant asks about first. Every
//! request is printed.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use wisp_core::oauth::{code_challenge, component, pairs};

#[derive(Default)]
struct Server {
    base: String,
    lifetime: u64,
    next: u64,
    clients: Vec<String>,
    /// Code → (challenge, client, redirect address).
    codes: HashMap<String, (String, String, String)>,
    access: HashMap<String, Instant>,
    refresh: Vec<String>,
    notes: Vec<String>,
}

/// The request's head and body, whichever way the body's length is announced.
fn read(stream: &mut TcpStream) -> (String, String) {
    let mut received = vec![];
    let mut buffer = [0u8; 8192];
    loop {
        let n = stream.read(&mut buffer).unwrap_or(0);
        received.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&received).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let head = text[..end].to_string();
            let lower = head.to_lowercase();
            let length: usize = lower.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
            if lower.contains("transfer-encoding: chunked") {
                if text.ends_with("0\r\n\r\n") {
                    // One chunk or several: their texts joined.
                    let mut body = String::new();
                    let mut rest = &text[end + 4..];
                    while let Some((size, after)) = rest.split_once("\r\n") {
                        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                        if size == 0 { break }
                        body.push_str(&after[..size.min(after.len())]);
                        rest = after.get(size + 2..).unwrap_or("");
                    }
                    return (head, body);
                }
            } else if received.len() >= end + 4 + length {
                return (head, text[end + 4..].to_string());
            }
        }
        if n == 0 { return (text, String::new()) }
    }
}

fn respond(status: &str, kind: &str, extra: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n{extra}Connection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn json(status: &str, body: serde_json::Value) -> String {
    respond(status, "application/json", "", &body.to_string())
}

fn redirect(to: &str) -> String {
    respond("302 Found", "text/plain", &format!("Location: {to}\r\n"), "")
}

fn get(pairs: &[(String, String)], key: &str) -> String {
    pairs.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone()).unwrap_or_default()
}

impl Server {
    fn token(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }

    fn tokens(&mut self) -> String {
        let (access, refresh) = (self.token("at"), self.token("rt"));
        self.access.insert(access.clone(), Instant::now() + Duration::from_secs(self.lifetime));
        self.refresh.push(refresh.clone());
        json("200 OK", serde_json::json!({"access_token": access, "token_type": "Bearer", "expires_in": self.lifetime, "refresh_token": refresh}))
    }

    fn answer(&mut self, head: &str, body: &str) -> String {
        let mut words = head.split_whitespace();
        let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = pairs(query);
        let base = self.base.clone();
        match (method, path) {
            ("POST", "/mcp") => {
                let bearer = head.lines().find_map(|line| line.to_lowercase().starts_with("authorization: bearer ").then(|| line[22..].trim().to_string()));
                let good = bearer.as_ref().and_then(|token| self.access.get(token)).is_some_and(|until| *until > Instant::now());
                if !good {
                    let error = if bearer.is_some() { "error=\"invalid_token\", " } else { "" };
                    return respond("401 Unauthorized", "text/plain", &format!("WWW-Authenticate: Bearer {error}resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"notes\"\r\n"), "");
                }
                let message: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let id = message["id"].clone();
                let result = match message["method"].as_str().unwrap_or("") {
                    "initialize" => serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "mock-notes", "version": "1"}}),
                    "tools/list" => serde_json::json!({"tools": [
                        {"name": "whoami", "description": "Who the token belongs to", "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": true}},
                        {"name": "add_note", "description": "Keep a note", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
                    ]}),
                    "tools/call" => {
                        let text = match message["params"]["name"].as_str() {
                            Some("whoami") => format!("You are the person who signed in at {base}, with {} notes.", self.notes.len()),
                            Some("add_note") => {
                                self.notes.push(message["params"]["arguments"]["text"].as_str().unwrap_or("").to_string());
                                format!("Kept. {} notes now.", self.notes.len())
                            }
                            _ => "No such tool.".into(),
                        };
                        serde_json::json!({"content": [{"type": "text", "text": text}]})
                    }
                    _ if id.is_null() => return respond("202 Accepted", "text/plain", "", ""),
                    other => return json("200 OK", serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
                };
                json("200 OK", serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
            }
            ("GET", "/.well-known/oauth-protected-resource/mcp" | "/.well-known/oauth-protected-resource") => json("200 OK", serde_json::json!({"resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["notes"]})),
            ("GET", "/.well-known/oauth-authorization-server") => json("200 OK", serde_json::json!({
                "issuer": base, "authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"), "revocation_endpoint": format!("{base}/revoke"),
                "response_types_supported": ["code"], "code_challenge_methods_supported": ["S256"], "token_endpoint_auth_methods_supported": ["none"],
            })),
            ("POST", "/register") => {
                let asked: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let id = self.token("client");
                self.clients.push(id.clone());
                json("201 Created", serde_json::json!({"client_id": id, "client_name": asked["client_name"], "redirect_uris": asked["redirect_uris"], "token_endpoint_auth_method": "none"}))
            }
            ("GET", "/authorize") => {
                let client = get(&query, "client_id");
                if !self.clients.contains(&client) { return respond("400 Bad Request", "text/plain", "", "unknown client") }
                let link = |allow: bool| format!("/consent?allow={}&{}", u8::from(allow), query.iter().map(|(k, v)| format!("{k}={}", component(v))).collect::<Vec<_>>().join("&"));
                let page = format!(
                    "<!doctype html><meta charset=utf-8><title>Mock Notes</title><body style=\"font:15px system-ui;max-width:420px;margin:15vh auto\"><h2>Mock Notes</h2><p>「{}」 asks to read and write your notes (scope: {}).</p><p><a href=\"{}\">允许 · Allow</a> &nbsp; <a href=\"{}\">拒绝 · Deny</a></p>",
                    client, get(&query, "scope"), link(true), link(false)
                );
                respond("200 OK", "text/html; charset=utf-8", "", &page)
            }
            ("GET", "/consent") => {
                let back = get(&query, "redirect_uri");
                let state = component(&get(&query, "state"));
                if get(&query, "allow") != "1" { return redirect(&format!("{back}?error=access_denied&error_description=The+person+said+no&state={state}")) }
                let code = self.token("code");
                self.codes.insert(code.clone(), (get(&query, "code_challenge"), get(&query, "client_id"), back.clone()));
                redirect(&format!("{back}?code={code}&state={state}"))
            }
            ("POST", "/token") => {
                let form = pairs(body);
                let refused = |why: &str| json("400 Bad Request", serde_json::json!({"error": "invalid_grant", "error_description": why}));
                match get(&form, "grant_type").as_str() {
                    "authorization_code" => {
                        let Some((challenge, client, back)) = self.codes.remove(&get(&form, "code")) else { return refused("unknown code") };
                        if challenge != code_challenge(&get(&form, "code_verifier")) { return refused("PKCE check failed") }
                        if client != get(&form, "client_id") || back != get(&form, "redirect_uri") { return refused("client or redirect differs") }
                        self.tokens()
                    }
                    "refresh_token" => {
                        let given = get(&form, "refresh_token");
                        let Some(at) = self.refresh.iter().position(|known| *known == given) else { return refused("refresh token used or unknown") };
                        self.refresh.remove(at);
                        self.tokens()
                    }
                    _ => refused("unsupported grant"),
                }
            }
            ("POST", "/revoke") => {
                let token = get(&pairs(body), "token");
                self.access.remove(&token);
                self.refresh.retain(|known| *known != token);
                respond("200 OK", "text/plain", "", "")
            }
            _ => respond("404 Not Found", "text/plain", "", ""),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(8790);
    let lifetime: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3600);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("port taken");
    let mut server = Server { base: format!("http://localhost:{port}"), lifetime, ..Default::default() };
    println!("mock MCP server with sign-in at {}/mcp (tokens good for {lifetime} s)", server.base);
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let (head, body) = read(&mut stream);
        let line = head.lines().next().unwrap_or("").to_string();
        let response = server.answer(&head, &body);
        let shown: String = body.chars().take(120).collect();
        println!("{line} → {}  {shown}", response.lines().next().unwrap_or("").trim_start_matches("HTTP/1.1 "));
        let _ = stream.write_all(response.as_bytes());
    }
}
