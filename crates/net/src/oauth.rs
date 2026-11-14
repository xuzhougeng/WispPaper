//! 浏览器登录's requests: what `wisp_core::oauth` says to ask, asked. A
//! sign-in is begun (`begin`: the server asked without a token, its
//! metadata and its authorization server's found, the app registered as a
//! client, a loopback port opened), the browser is sent to its `address`
//! by whoever shows a window, and `SignIn::finish` waits for the browser to
//! come back, exchanges the code and keeps the tokens. `bearer` is the
//! token a connector's requests carry, refreshed when it has run out;
//! `renew` refreshes it after the server refused it.

use crate::{agent, network_error};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wisp_core::connector;
use wisp_core::oauth::{self, Callback, Client, Endpoints, Grant, Refusal, TokenRequest};

/// How long the browser has for the sign-in.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// What `SignIn::finish` says when it was stopped.
pub const CANCELLED: &str = "已取消登录。";

/// Another file for the sign-ins than the one beside `settings.json` (tests).
static FILE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// One reader or writer of the file at a time, and one refresh: two at once
/// would spend a refresh token twice, and a server that rotates them takes it once.
static LOCK: Mutex<()> = Mutex::new(());

pub fn set_file(path: PathBuf) {
    *FILE.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
}

fn file() -> PathBuf {
    FILE.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().unwrap_or_else(|| oauth::file(&wisp_core::settings::settings_file()))
}

fn guard() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

fn random() -> Result<[u8; 32], String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| format!("无法生成随机数：{err}"))?;
    Ok(bytes)
}

/// GET a metadata document; `None` when it is not there, so that the next place is tried.
fn document(url: &str) -> Result<Option<Value>, String> {
    if !oauth::secure(url) { return Ok(None) }
    let mut response = agent(Some(Duration::from_secs(30)), "")?.get(url).header("accept", "application/json").header("mcp-protocol-version", connector::VERSION).call().map_err(network_error)?;
    if !(200..300).contains(&response.status().as_u16()) { return Ok(None) }
    Ok(response.body_mut().read_json::<Value>().ok().filter(Value::is_object))
}

/// POST a form to the token or revocation endpoint: the status and the body.
fn post_form(request: &TokenRequest) -> Result<(u16, String), String> {
    let mut call = agent(Some(Duration::from_secs(60)), "")?.post(request.url.as_str()).header("content-type", "application/x-www-form-urlencoded").header("accept", "application/json");
    if let Some(authorization) = &request.authorization { call = call.header("authorization", authorization) }
    let mut response = call.send(request.body.as_str()).map_err(network_error)?;
    let status = response.status().as_u16();
    Ok((status, response.body_mut().read_to_string().unwrap_or_default()))
}

/// Ask the server without a token, as a connection would, and read what its refusal says.
fn probe(target: &str) -> Result<oauth::Challenge, String> {
    let response = agent(Some(Duration::from_secs(60)), "")?
        .post(target)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .send(connector::initialize(0).to_string().as_str())
        .map_err(network_error)?;
    let said: Vec<&str> = response.headers().get_all("www-authenticate").iter().filter_map(|value| value.to_str().ok()).collect();
    Ok(oauth::challenge(&said.join(", ")))
}

/// The endpoints of the first of `issuers` whose metadata can be found.
fn server_endpoints(issuers: &[String]) -> Result<Endpoints, String> {
    for issuer in issuers {
        for url in oauth::server_metadata_urls(issuer) {
            if let Some(doc) = document(&url)? { return oauth::read_server(&doc, issuer) }
        }
    }
    Err(format!("找不到授权服务器 {} 的元数据，无法登录。", issuers.first().map(|issuer| oauth::host(issuer)).unwrap_or_default()))
}

/// A server without protected resource metadata: its origin is its
/// authorization server, with metadata or at the usual paths.
fn legacy_endpoints(target: &str) -> Result<Endpoints, String> {
    let origin = oauth::origin(target).unwrap_or_default();
    for url in oauth::server_metadata_urls(&origin) {
        if let Some(doc) = document(&url)? { return oauth::read_server(&doc, &origin) }
    }
    Ok(oauth::legacy_endpoints(target))
}

/// Register the app as a client of the authorization server.
fn register(endpoints: &Endpoints, redirect_uri: &str, scope: &str) -> Result<Client, String> {
    let host = oauth::host(&endpoints.issuer);
    let token_instead = "如果这个服务提供令牌（API key），可以填进连接器的“令牌”里连接。";
    if endpoints.registration.is_empty() { return Err(format!("{host} 不接受应用自行注册（没有动态客户端注册），无法在浏览器中登录。{token_instead}")) }
    let mut response = agent(Some(Duration::from_secs(60)), "")?
        .post(endpoints.registration.as_str())
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send(oauth::registration(redirect_uri, scope).to_string().as_str())
        .map_err(network_error)?;
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if status == 404 { return Err(format!("{host} 没有提供浏览器登录（找不到注册客户端的地址）。{token_instead}")) }
    if !(200..300).contains(&status) { return Err(format!("{host} 拒绝了注册（HTTP {status}）：{}", body.chars().take(200).collect::<String>())) }
    let doc: Value = serde_json::from_str(&body).map_err(|_| format!("{host} 的注册结果读不懂。"))?;
    oauth::read_client(&doc, redirect_uri, endpoints)
}

/// Listen on the loopback at `port` (0: any port free), on IPv4 and, where
/// it can, on IPv6 too: the browser may look `localhost` up either way.
fn listen(port: u16) -> Option<Vec<TcpListener>> {
    let first = TcpListener::bind(("127.0.0.1", port)).ok()?;
    let port = first.local_addr().ok()?.port();
    let mut listeners = vec![first];
    if let Ok(second) = TcpListener::bind(("::1", port)) { listeners.push(second) }
    for listener in &listeners { listener.set_nonblocking(true).ok()? }
    Some(listeners)
}

/// A sign-in under way: the browser is to be sent to `address`, and it comes back to the loopback.
pub struct SignIn {
    key: String,
    grant: Grant,
    verifier: String,
    state: String,
    address: String,
    listeners: Vec<TcpListener>,
}

/// Begin signing in to the connector at `target`. Blocks on the network for discovery and registration.
pub fn begin(target: &str) -> Result<SignIn, String> {
    let target = target.trim();
    if !oauth::secure(target) { return Err("只有 HTTPS 地址的连接器能在浏览器中登录。".into()) }
    let challenge = probe(target)?;
    let mut found = None;
    for url in challenge.resource_metadata.iter().cloned().chain(oauth::resource_metadata_urls(target)) {
        if let Some(doc) = document(&url)? {
            found = Some(oauth::read_resource(&doc, target)?);
            break;
        }
    }
    // A server from before RFC 9728 gets no `resource`: it would not know what to do with one.
    let (endpoints, resource, scopes) = match found {
        Some(found) => (server_endpoints(&found.servers)?, found.resource, found.scopes),
        None => (legacy_endpoints(target)?, String::new(), vec![]),
    };
    let scope = challenge.scope.unwrap_or_else(|| scopes.join(" "));
    let key = oauth::key(target);
    let kept = {
        let _guard = guard();
        oauth::load(&file()).remove(&key)
    };
    // The client registered last time is used again while its port is free.
    let again = kept.filter(|grant| grant.endpoints.issuer == endpoints.issuer && !grant.client.id.is_empty()).and_then(|grant| Some((listen(oauth::redirect_port(&grant.client.redirect_uri)?)?, grant.client)));
    let (listeners, client) = match again {
        Some(again) => again,
        None => {
            let listeners = listen(0).ok_or("无法在本机打开接收登录结果的端口。")?;
            let port = listeners[0].local_addr().map_err(|err| format!("无法在本机打开接收登录结果的端口：{err}"))?.port();
            let client = register(&endpoints, &oauth::redirect_uri(port), &scope)?;
            (listeners, client)
        }
    };
    let verifier = oauth::verifier(&random()?);
    let state = oauth::base64url(&random()?[..16]);
    let grant = Grant { endpoints, client, resource, scope, tokens: None };
    let address = oauth::authorize_url(&grant, &oauth::code_challenge(&verifier), &state);
    if !oauth::secure(&address) { return Err(format!("授权服务器的登录地址不安全：{address}")) }
    Ok(SignIn { key, grant, verifier, state, address, listeners })
}

impl SignIn {
    /// Where to send the browser.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Wait for the browser (at most `WAIT`, or until `stop`), exchange the
    /// code and keep the sign-in. The page the browser shows says how it went.
    /// Returns where the person signed in.
    pub fn finish(self, stop: &AtomicBool) -> Result<String, String> {
        let (mut stream, code) = self.wait(stop)?;
        let outcome = code.and_then(|code| {
            let (status, body) = post_form(&oauth::code_request(&self.grant, &code, &self.verifier))?;
            let tokens = oauth::read_tokens(status, &body, now(), None).map_err(|(Refusal::Rejected(why) | Refusal::Failed(why))| why)?;
            let mut grant = self.grant.clone();
            grant.tokens = Some(tokens);
            let account = grant.account();
            let _guard = guard();
            let path = file();
            let mut grants = oauth::load(&path);
            grants.insert(self.key.clone(), grant);
            oauth::save(&path, &grants)?;
            Ok(account)
        });
        let page = match &outcome {
            Ok(_) => oauth::page(true, ""),
            Err(why) => oauth::page(false, why),
        };
        respond(&mut stream, "200 OK", &page);
        outcome
    }

    /// The callback's connection, kept open for the page, and its code or why there is none.
    fn wait(&self, stop: &AtomicBool) -> Result<(TcpStream, Result<String, String>), String> {
        let deadline = Instant::now() + WAIT;
        loop {
            if stop.load(Ordering::Relaxed) { return Err(CANCELLED.into()) }
            if Instant::now() > deadline { return Err(format!("{} 分钟内没有在浏览器里完成登录。", WAIT.as_secs() / 60)) }
            let mut heard = false;
            for listener in &self.listeners {
                let Ok((stream, _)) = listener.accept() else { continue };
                heard = true;
                if let Some(outcome) = self.hear(stream) { return Ok(outcome) }
            }
            if !heard { std::thread::sleep(Duration::from_millis(50)) }
        }
    }

    /// Read one request to the loopback. The callback is handed back unanswered; anything else is answered here.
    fn hear(&self, mut stream: TcpStream) -> Option<(TcpStream, Result<String, String>)> {
        // An accepted socket keeps its listener's non-blocking mode on some systems.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut received = vec![];
        let mut buffer = [0u8; 2048];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") && received.len() < 16 * 1024 {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => received.extend_from_slice(&buffer[..n]),
            }
        }
        let text = String::from_utf8_lossy(&received);
        let mut words = text.lines().next().unwrap_or("").split_whitespace();
        let heard = match (words.next(), words.next()) {
            (Some("GET"), Some(target)) => oauth::callback(target, &self.state),
            _ => Callback::NotOurs,
        };
        match heard {
            Callback::Code(code) => Some((stream, Ok(code))),
            Callback::Refused(why) => Some((stream, Err(format!("登录没有完成：{why}")))),
            Callback::Mismatch => {
                respond(&mut stream, "400 Bad Request", &oauth::page(false, "这个登录结果不属于正在进行的登录。"));
                None
            }
            Callback::NotOurs => {
                respond(&mut stream, "404 Not Found", "");
                None
            }
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, page: &str) {
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{page}", page.len());
    let _ = stream.flush();
}

/// Refresh the tokens of `key`, under the lock: the new access token, or
/// `None` when the server refused and the sign-in is over (its client stays
/// registered for the next one). A failure that may pass is an error.
fn refresh(path: &Path, grants: &mut BTreeMap<String, Grant>, key: &str) -> Result<Option<String>, String> {
    let Some(grant) = grants.get_mut(key) else { return Ok(None) };
    let outcome = match oauth::refresh_request(grant) {
        None => Err(Refusal::Rejected(String::new())),
        Some(request) => {
            let (status, body) = post_form(&request)?;
            oauth::read_tokens(status, &body, now(), grant.tokens.as_ref())
        }
    };
    match outcome {
        Ok(tokens) => {
            let access = tokens.access.clone();
            grant.tokens = Some(tokens);
            oauth::save(path, grants)?;
            Ok(Some(access))
        }
        Err(Refusal::Rejected(_)) => {
            grant.tokens = None;
            oauth::save(path, grants)?;
            Ok(None)
        }
        Err(Refusal::Failed(why)) => Err(why),
    }
}

/// The access token for the connector at `target`, refreshed first when it
/// has run out; `None` when there is no sign-in to use.
pub fn bearer(target: &str) -> Result<Option<String>, String> {
    let _guard = guard();
    let path = file();
    let mut grants = oauth::load(&path);
    let key = oauth::key(target);
    let Some(tokens) = grants.get(&key).and_then(|grant| grant.tokens.clone()) else { return Ok(None) };
    if tokens.fresh(now()) { return Ok(Some(tokens.access)) }
    refresh(&path, &mut grants, &key)
}

/// The server refused `rejected`: the token to try instead, refreshed
/// unless another request did that meanwhile; `None` when the sign-in is over.
pub fn renew(target: &str, rejected: &str) -> Result<Option<String>, String> {
    let _guard = guard();
    let path = file();
    let mut grants = oauth::load(&path);
    let key = oauth::key(target);
    let Some(tokens) = grants.get(&key).and_then(|grant| grant.tokens.clone()) else { return Ok(None) };
    if tokens.access != rejected { return Ok(Some(tokens.access)) }
    refresh(&path, &mut grants, &key)
}

/// Where the connector at `target` is signed in, when it is.
pub fn account(target: &str) -> Option<String> {
    let _guard = guard();
    oauth::load(&file()).get(&oauth::key(target)).filter(|grant| grant.tokens.is_some()).map(Grant::account)
}

/// Sign out of the connector at `target`: its tokens are forgotten (with
/// `whole`, its client registration too) and handed back for `revoke`.
pub fn sign_out(target: &str, whole: bool) -> Result<Option<Grant>, String> {
    let _guard = guard();
    let path = file();
    let mut grants = oauth::load(&path);
    let key = oauth::key(target);
    let Some(grant) = grants.get_mut(&key) else { return Ok(None) };
    let before = grant.clone();
    grant.tokens = None;
    if whole { grants.remove(&key); }
    oauth::save(&path, &grants)?;
    Ok(Some(before).filter(|grant| grant.tokens.is_some()))
}

/// Give a sign-in's tokens back to the server, as far as it takes them; nothing is said of a failure.
pub fn revoke(grant: &Grant) {
    for request in oauth::revoke_requests(grant) {
        let _ = post_form(&request);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// The sign-ins of every test in one file of their own, never the person's.
    pub(crate) fn private_file() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| set_file(std::env::temp_dir().join(format!("wisp-net-oauth-{}", std::process::id())).join("oauth.json")));
    }

    /// A server that answers by the request: `answer(method, path, headers, body)` gives the whole response.
    pub(crate) fn route(answer: impl Fn(&str, &str, &str, &str) -> String + Send + Sync + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let answer = answer.clone();
                std::thread::spawn(move || {
                    let mut received = vec![];
                    let mut buffer = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut buffer) {
                        received.extend_from_slice(&buffer[..n]);
                        let text = String::from_utf8_lossy(&received).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let head = text[..end].to_lowercase();
                            let length: usize = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
                            let chunked = head.contains("transfer-encoding: chunked");
                            if (chunked && text.ends_with("0\r\n\r\n")) || (!chunked && received.len() >= end + 4 + length) { break }
                        }
                        if n == 0 { break }
                    }
                    let text = String::from_utf8_lossy(&received).to_string();
                    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                    let mut words = head.split_whitespace();
                    let (method, path) = (words.next().unwrap_or("").to_string(), words.next().unwrap_or("").to_string());
                    // A chunked body: its one chunk's text is enough for these tests.
                    let body = if head.to_lowercase().contains("transfer-encoding: chunked") { body.split("\r\n").nth(1).unwrap_or("").to_string() } else { body.to_string() };
                    let _ = stream.write_all(answer(&method, &path, &head.to_lowercase(), &body).as_bytes());
                });
            }
        });
        base
    }

    pub(crate) fn json(status: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    /// Play the browser: follow the sign-in address to the server's own
    /// consent, which sends it straight back to the loopback.
    fn browse(address: &str) -> String {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(5).build().into();
        agent.get(address).call().unwrap().body_mut().read_to_string().unwrap()
    }

    /// An MCP server that is its own authorization server, as most are:
    /// tokens `at1`, `at2` … in turn, each good for `lifetime` seconds.
    fn server(lifetime: i64) -> (String, Arc<AtomicUsize>) {
        let issued = Arc::new(AtomicUsize::new(0));
        let counter = issued.clone();
        let base = Arc::new(std::sync::OnceLock::<String>::new());
        let known = base.clone();
        let address = route(move |method, path, headers, body| {
            let base = known.get().cloned().unwrap_or_default();
            let current = format!("authorization: bearer at{}", counter.load(Ordering::SeqCst));
            match (method, path.split('?').next().unwrap_or("")) {
                ("POST", "/mcp") if !headers.contains(&current) => format!("HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"notes\"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"),
                ("POST", "/mcp") if body.contains("\"initialize\"") => json("200 OK", r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-03-26"}}"#),
                ("POST", "/mcp") if body.contains("notifications/") => "HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
                ("POST", "/mcp") => json("200 OK", &format!(r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"whoami","description":"{}"}}]}}}}"#, if headers.contains("mcp-protocol-version: 2025-03-26") { "negotiated" } else { "not negotiated" })),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => json("200 OK", &format!(r#"{{"resource":"{base}/mcp","authorization_servers":["{base}"]}}"#)),
                ("GET", "/.well-known/oauth-authorization-server") => json("200 OK", &format!(r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","registration_endpoint":"{base}/register","code_challenge_methods_supported":["S256"]}}"#)),
                ("POST", "/register") => json("201 Created", &format!(r#"{{"client_id":"wisp-1","redirect_uris":{}}}"#, serde_json::from_str::<Value>(body).unwrap()["redirect_uris"])),
                ("GET", "/authorize") => {
                    let pairs = oauth::pairs(path.split_once('?').unwrap().1);
                    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
                    assert_eq!((get("client_id").as_str(), get("scope").as_str(), get("resource"), get("code_challenge_method").as_str()), ("wisp-1", "notes", format!("{base}/mcp"), "S256"));
                    format!("HTTP/1.1 302 Found\r\nLocation: {}?code=c-{}&state={}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", get("redirect_uri"), oauth::component(&get("code_challenge")), get("state"))
                }
                ("POST", "/token") => {
                    let pairs = oauth::pairs(body);
                    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
                    let good = match get("grant_type").as_str() {
                        "authorization_code" => get("code") == format!("c-{}", oauth::code_challenge(&get("code_verifier"))),
                        _ => get("refresh_token") == format!("rt{}", counter.load(Ordering::SeqCst)),
                    };
                    if !good { return json("400 Bad Request", r#"{"error":"invalid_grant"}"#) }
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    json("200 OK", &format!(r#"{{"access_token":"at{n}","token_type":"Bearer","expires_in":{lifetime},"refresh_token":"rt{n}"}}"#))
                }
                _ => "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
            }
        });
        base.set(address.clone()).unwrap();
        (address, issued)
    }

    #[test]
    fn a_sign_in_goes_through_the_browser_and_is_kept() {
        private_file();
        let (base, issued) = server(3600);
        let target = format!("{base}/mcp");
        let sign_in = begin(&target).unwrap();
        assert!(sign_in.address().starts_with(&format!("{base}/authorize?")), "{}", sign_in.address());
        let address = sign_in.address().to_string();
        let browser = std::thread::spawn(move || browse(&address));
        let host = base.trim_start_matches("http://").to_string();
        assert_eq!(sign_in.finish(&AtomicBool::new(false)), Ok(host.clone()));
        assert!(browser.join().unwrap().contains("已登录"));
        assert_eq!((bearer(&target), account(&target), issued.load(Ordering::SeqCst)), (Ok(Some("at1".into())), Some(host), 1));
        // A refused token is refreshed once; one refreshed meanwhile is used as it is.
        assert_eq!(renew(&target, "at1"), Ok(Some("at2".into())));
        assert_eq!(renew(&target, "at1"), Ok(Some("at2".into())));
        // Signing out forgets the tokens and keeps the client for next time.
        assert!(sign_out(&target, false).unwrap().is_some());
        assert_eq!((bearer(&target), account(&target)), (Ok(None), None));
        assert_eq!(oauth::load(&file())[&oauth::key(&target)].client.id, "wisp-1");
        assert_eq!(sign_out(&target, true), Ok(None));
        assert!(!oauth::load(&file()).contains_key(&oauth::key(&target)));
    }

    #[test]
    fn a_token_run_out_is_refreshed_before_it_is_sent_and_a_dead_one_ends_the_sign_in() {
        private_file();
        let (base, _issued) = server(30);
        let target = format!("{base}/mcp");
        let sign_in = begin(&target).unwrap();
        let address = sign_in.address().to_string();
        std::thread::spawn(move || browse(&address));
        sign_in.finish(&AtomicBool::new(false)).unwrap();
        // Thirty seconds is within the margin: the token is refreshed before use.
        assert_eq!(bearer(&target), Ok(Some("at2".into())));
        // A refresh token the server does not know any more ends the sign-in.
        {
            let _guard = guard();
            let path = file();
            let mut grants = oauth::load(&path);
            grants.get_mut(&oauth::key(&target)).unwrap().tokens.as_mut().unwrap().refresh = "gone".into();
            oauth::save(&path, &grants).unwrap();
        }
        assert_eq!(renew(&target, "at2"), Ok(None));
        assert_eq!(account(&target), None);
    }

    #[test]
    fn a_sign_in_can_be_stopped_and_refused() {
        private_file();
        let (base, _issued) = server(3600);
        let target = format!("{base}/mcp");
        let stop = AtomicBool::new(true);
        assert_eq!(begin(&target).unwrap().finish(&stop), Err(CANCELLED.into()));
        // The person says no: the page says so, and so does the result.
        let sign_in = begin(&target).unwrap();
        let back = format!("{}?error=access_denied&error_description=Not+now&state={}", sign_in.grant.client.redirect_uri, sign_in.state);
        let wrong = format!("{}?code=x&state=other", sign_in.grant.client.redirect_uri);
        let browser = std::thread::spawn(move || (browse(&wrong), browse(&back)));
        assert_eq!(sign_in.finish(&AtomicBool::new(false)), Err("登录没有完成：Not now".into()));
        let (wrong, back) = browser.join().unwrap();
        assert!(wrong.contains("不属于") && back.contains("Not now"), "{wrong}\n{back}");
        // An address that is not HTTPS, and a server that offers no sign-in, say why.
        assert!(begin("http://example.com/mcp").is_err());
        let bare = route(|_, _, _, _| "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into());
        let error = begin(&format!("{bare}/mcp")).err().unwrap();
        assert!(error.contains("没有提供浏览器登录"), "{error}");
    }

    /// For `mcp`'s tests: the address of a server that asks for a sign-in, and a way to sign in to it.
    pub(crate) fn signed_in_server() -> String {
        private_file();
        let (base, _issued) = server(3600);
        let target = format!("{base}/mcp");
        let sign_in = begin(&target).unwrap();
        let address = sign_in.address().to_string();
        std::thread::spawn(move || browse(&address));
        sign_in.finish(&AtomicBool::new(false)).unwrap();
        target
    }

    pub(crate) fn unsigned_server() -> String {
        private_file();
        format!("{}/mcp", server(3600).0)
    }
}
