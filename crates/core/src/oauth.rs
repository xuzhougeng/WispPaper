//! 浏览器登录: signing in to a connector the way MCP's authorization spec
//! has it, OAuth 2.1 with PKCE. A server that wants it answers 401 and says
//! where its protected resource metadata is (RFC 9728); that names the
//! authorization server, whose metadata (RFC 8414, or OpenID's) gives the
//! endpoints; the app registers itself there as a client (RFC 7591), sends
//! the person's browser to the authorization endpoint with a code challenge
//! and the server as `resource` (RFC 8707), and takes the code that comes
//! back to a loopback address (RFC 8252) for tokens, which it refreshes
//! when they run out. A server from before the metadata (2025-03-26) is its
//! own authorization server at its origin.
//!
//! This is the side of it that needs no socket: the addresses to ask, the
//! messages, how answers are read, and the file the sign-ins are kept in
//! (beside `settings.json`, in plain text like the model keys). The
//! requests, the loopback and the browser's page are `wisp_net::oauth`'s.

use crate::ai::base64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the browser comes back to on the loopback.
pub const CALLBACK: &str = "/callback";

/// An access token this close to running out is refreshed before it is sent.
pub const MARGIN_SECS: i64 = 60;

/// What a server's `WWW-Authenticate` says about signing in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Challenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// Read the parameters of a `WWW-Authenticate` header (`Bearer
/// resource_metadata="…", scope="a b"`), whichever challenge they are in.
pub fn challenge(header: &str) -> Challenge {
    let mut found = Challenge::default();
    let chars: Vec<char> = header.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() || chars[i] == ',' { i += 1; continue }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || "-_.".contains(chars[i])) { i += 1 }
        let name: String = chars[start..i].iter().collect::<String>().to_lowercase();
        if i == start { i += 1; continue }
        let mut j = i;
        while j < chars.len() && chars[j] == ' ' { j += 1 }
        // A scheme's name (`Bearer`) has no `=` after it.
        if j >= chars.len() || chars[j] != '=' { continue }
        j += 1;
        while j < chars.len() && chars[j] == ' ' { j += 1 }
        let mut value = String::new();
        if j < chars.len() && chars[j] == '"' {
            j += 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' && j + 1 < chars.len() { j += 1 }
                value.push(chars[j]);
                j += 1;
            }
            j += 1;
        } else {
            while j < chars.len() && chars[j] != ',' && !chars[j].is_whitespace() { value.push(chars[j]); j += 1 }
        }
        i = j;
        let value = Some(value).filter(|value| !value.is_empty());
        match name.as_str() {
            "resource_metadata" => found.resource_metadata = value,
            "scope" => found.scope = value,
            "error" => found.error = value,
            _ => {}
        }
    }
    found
}

/// An address cut into its origin (`https://host:port`, scheme and host in
/// small letters) and its path without a trailing slash, query or fragment.
fn split(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if host_end == 0 { return None }
    let origin = format!("{}://{}", url[..scheme_end].to_lowercase(), rest[..host_end].to_lowercase());
    let path = rest[host_end..].split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    Some((origin, path.to_string()))
}

/// The origin of an address, `https://host[:port]`.
pub fn origin(url: &str) -> Option<String> {
    split(url).map(|(origin, _)| origin)
}

/// The host an address names, for saying where the person signed in.
pub fn host(url: &str) -> String {
    origin(url).and_then(|origin| origin.split("://").nth(1).map(str::to_string)).unwrap_or_default()
}

/// Whether an address may be asked for anything to do with a sign-in, or
/// opened in the browser: HTTPS, or HTTP to this computer.
pub fn secure(url: &str) -> bool {
    let Some((origin, _)) = split(url) else { return false };
    if origin.starts_with("https://") { return true }
    let Some(authority) = origin.strip_prefix("http://") else { return false };
    let host = if authority.starts_with('[') { authority.split(']').next().map(|h| format!("{h}]")).unwrap_or_default() } else { authority.split(':').next().unwrap_or("").to_string() };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// Where a server's protected resource metadata may be when its 401 does not
/// say: the address with the server's path first (RFC 9728 §3.1), then the root.
pub fn resource_metadata_urls(server: &str) -> Vec<String> {
    let Some((origin, path)) = split(server) else { return vec![] };
    let root = format!("{origin}/.well-known/oauth-protected-resource");
    if path.is_empty() { vec![root] } else { vec![format!("{root}{path}"), root] }
}

/// Where an authorization server's metadata may be: OAuth's and OpenID's
/// well-known addresses, the issuer's path inserted and then appended.
pub fn server_metadata_urls(issuer: &str) -> Vec<String> {
    let Some((origin, path)) = split(issuer) else { return vec![] };
    if path.is_empty() {
        return vec![format!("{origin}/.well-known/oauth-authorization-server"), format!("{origin}/.well-known/openid-configuration")];
    }
    vec![format!("{origin}/.well-known/oauth-authorization-server{path}"), format!("{origin}/.well-known/openid-configuration{path}"), format!("{origin}{path}/.well-known/openid-configuration")]
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// What a protected resource metadata document says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    /// The server as tokens are bound to it.
    pub resource: String,
    /// The authorization servers, by issuer.
    pub servers: Vec<String>,
    pub scopes: Vec<String>,
}

/// Read a protected resource metadata document for `server`. A document
/// that claims another resource than the server or a part of it is
/// refused: tokens for that would be asked for in the server's name.
pub fn read_resource(doc: &Value, server: &str) -> Result<Resource, String> {
    let resource = doc["resource"].as_str().map(str::trim).filter(|r| !r.is_empty()).unwrap_or(server.trim());
    let (Some((own_origin, own_path)), Some((origin, path))) = (split(server), split(resource)) else { return Err(format!("服务的元数据里的资源地址读不懂：{resource}")) };
    if origin != own_origin || !(own_path == path || own_path.starts_with(&format!("{path}/")) || path.is_empty()) {
        return Err(format!("服务的元数据说它属于另一个地址（{resource}），为了安全没有登录。"));
    }
    let servers: Vec<String> = strings(&doc["authorization_servers"]).into_iter().filter(|issuer| secure(issuer)).collect();
    if servers.is_empty() { return Err("服务的元数据没有指明授权服务器。".into()) }
    Ok(Resource { resource: resource.to_string(), servers, scopes: strings(&doc["scopes_supported"]) })
}

/// An authorization server's endpoints.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Endpoints {
    pub issuer: String,
    pub authorization: String,
    pub token: String,
    /// Where a client registers itself; empty when the server takes no registrations.
    pub registration: String,
    /// Where tokens are given back on signing out; empty when it has none.
    pub revocation: String,
    /// How the token endpoint may be told who asks (`token_endpoint_auth_methods_supported`).
    pub auth_methods: Vec<String>,
}

/// Read an authorization server's metadata.
pub fn read_server(doc: &Value, issuer: &str) -> Result<Endpoints, String> {
    let text = |key: &str| doc[key].as_str().map(str::trim).unwrap_or("").to_string();
    let endpoints = Endpoints {
        issuer: Some(text("issuer")).filter(|i| !i.is_empty()).unwrap_or_else(|| issuer.trim().to_string()),
        authorization: text("authorization_endpoint"),
        token: text("token_endpoint"),
        registration: text("registration_endpoint"),
        revocation: text("revocation_endpoint"),
        auth_methods: strings(&doc["token_endpoint_auth_methods_supported"]),
    };
    if endpoints.authorization.is_empty() || endpoints.token.is_empty() { return Err(format!("授权服务器 {} 的元数据缺少登录或令牌的地址。", host(issuer))) }
    for url in [&endpoints.authorization, &endpoints.token, &endpoints.registration, &endpoints.revocation] {
        if !url.is_empty() && !secure(url) { return Err(format!("授权服务器的地址不安全（不是 HTTPS）：{url}")) }
    }
    let pkce = strings(&doc["code_challenge_methods_supported"]);
    if !pkce.is_empty() && !pkce.iter().any(|method| method == "S256") { return Err(format!("授权服务器 {} 不支持 PKCE（S256），为了安全没有登录。", host(issuer))) }
    Ok(endpoints)
}

/// The endpoints of a server from before discovery: it is its own
/// authorization server, at the usual paths under its origin.
pub fn legacy_endpoints(server: &str) -> Endpoints {
    let origin = origin(server).unwrap_or_default();
    Endpoints { issuer: origin.clone(), authorization: format!("{origin}/authorize"), token: format!("{origin}/token"), registration: format!("{origin}/register"), ..Default::default() }
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}{CALLBACK}")
}

/// The port a redirect address of `redirect_uri` listens on.
pub fn redirect_port(uri: &str) -> Option<u16> {
    uri.strip_prefix("http://localhost:")?.split('/').next()?.parse().ok()
}

/// The body that registers the app as a client: a public one, with no secret.
pub fn registration(redirect_uri: &str, scope: &str) -> Value {
    let mut body = json!({
        "client_name": "Wisp Paper",
        "client_uri": "https://xuzhougeng.github.io/WispPaper/",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if !scope.is_empty() { body["scope"] = json!(scope) }
    body
}

/// The app as a client of one authorization server.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Client {
    pub id: String,
    pub secret: String,
    /// How the token endpoint is told who asks: `none`, `client_secret_post` or `client_secret_basic`.
    pub auth: String,
    pub redirect_uri: String,
}

/// Read what a registration answered. A server that gives a secret anyway
/// is told it the way it says, else the way its metadata prefers.
pub fn read_client(doc: &Value, redirect_uri: &str, endpoints: &Endpoints) -> Result<Client, String> {
    let id = doc["client_id"].as_str().map(str::trim).unwrap_or("").to_string();
    if id.is_empty() { return Err("授权服务器的注册结果里没有 client_id。".into()) }
    let secret = doc["client_secret"].as_str().unwrap_or("").to_string();
    let said = doc["token_endpoint_auth_method"].as_str().unwrap_or("");
    let auth = if secret.is_empty() {
        "none"
    } else if matches!(said, "client_secret_basic" | "client_secret_post") {
        said
    } else if endpoints.auth_methods.iter().any(|m| m == "client_secret_post") && !endpoints.auth_methods.iter().any(|m| m == "client_secret_basic") {
        "client_secret_post"
    } else {
        "client_secret_basic"
    };
    Ok(Client { id, secret, auth: auth.into(), redirect_uri: redirect_uri.into() })
}

/// Bytes as base64url without padding: PKCE's alphabet.
pub fn base64url(bytes: &[u8]) -> String {
    base64(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

/// A code verifier (RFC 7636): 32 random bytes as 43 letters.
pub fn verifier(random: &[u8; 32]) -> String {
    base64url(random)
}

/// The S256 challenge of a verifier.
pub fn code_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Percent-encode a value for a query or a form: only unreserved characters stay.
pub fn component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

/// Undo `component`, and a form's `+` for a space.
pub fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("x"), 16) => {
                out.push(byte);
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form(params: &[(&str, &str)]) -> String {
    params.iter().map(|(name, value)| format!("{name}={}", component(value))).collect::<Vec<_>>().join("&")
}

/// The pairs of a query or a form body.
pub fn pairs(query: &str) -> Vec<(String, String)> {
    query.split('&').filter(|pair| !pair.is_empty()).map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(name), decode(value))
    }).collect()
}

/// A sign-in with one server: everything its requests share, and the tokens once there are some.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    pub endpoints: Endpoints,
    pub client: Client,
    /// The server as the tokens are bound to it (RFC 8707); empty sends no `resource`.
    pub resource: String,
    pub scope: String,
    pub tokens: Option<Tokens>,
}

impl Grant {
    /// Where the person signed in, for the window: the authorization server's host.
    pub fn account(&self) -> String {
        host(&self.endpoints.issuer)
    }
}

/// The address the browser is sent to.
pub fn authorize_url(grant: &Grant, challenge: &str, state: &str) -> String {
    let mut params = vec![("response_type", "code"), ("client_id", grant.client.id.as_str()), ("redirect_uri", grant.client.redirect_uri.as_str()), ("code_challenge", challenge), ("code_challenge_method", "S256"), ("state", state)];
    if !grant.scope.is_empty() { params.push(("scope", &grant.scope)) }
    if !grant.resource.is_empty() { params.push(("resource", &grant.resource)) }
    let endpoint = &grant.endpoints.authorization;
    format!("{endpoint}{}{}", if endpoint.contains('?') { '&' } else { '?' }, form(&params))
}

/// A request to the token or revocation endpoint: a form, and the
/// `Authorization` header of a client that tells its secret the basic way.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenRequest {
    pub url: String,
    pub body: String,
    pub authorization: Option<String>,
}

fn token_request<'a>(grant: &'a Grant, url: &str, mut params: Vec<(&'a str, &'a str)>) -> TokenRequest {
    let client = &grant.client;
    let mut authorization = None;
    match client.auth.as_str() {
        "client_secret_basic" if !client.secret.is_empty() => authorization = Some(format!("Basic {}", base64(format!("{}:{}", component(&client.id), component(&client.secret)).as_bytes()))),
        "client_secret_post" if !client.secret.is_empty() => params.extend([("client_id", client.id.as_str()), ("client_secret", client.secret.as_str())]),
        _ => params.push(("client_id", &client.id)),
    }
    TokenRequest { url: url.to_string(), body: form(&params), authorization }
}

/// Exchange the code the browser brought back.
pub fn code_request(grant: &Grant, code: &str, verifier: &str) -> TokenRequest {
    let mut params = vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", grant.client.redirect_uri.as_str()), ("code_verifier", verifier)];
    if !grant.resource.is_empty() { params.push(("resource", &grant.resource)) }
    token_request(grant, &grant.endpoints.token, params)
}

/// Refresh the tokens; `None` without a refresh token.
pub fn refresh_request(grant: &Grant) -> Option<TokenRequest> {
    let refresh = grant.tokens.as_ref().map(|tokens| tokens.refresh.as_str()).filter(|r| !r.is_empty())?;
    let mut params = vec![("grant_type", "refresh_token"), ("refresh_token", refresh)];
    if !grant.resource.is_empty() { params.push(("resource", &grant.resource)) }
    Some(token_request(grant, &grant.endpoints.token, params))
}

/// Give the tokens back (RFC 7009), the refresh token first, which ends the
/// sign-in at the server. Nothing when the server has no revocation endpoint.
pub fn revoke_requests(grant: &Grant) -> Vec<TokenRequest> {
    let (Some(tokens), false) = (&grant.tokens, grant.endpoints.revocation.is_empty()) else { return vec![] };
    [(tokens.refresh.as_str(), "refresh_token"), (tokens.access.as_str(), "access_token")]
        .into_iter()
        .filter(|(token, _)| !token.is_empty())
        .map(|(token, hint)| token_request(grant, &grant.endpoints.revocation, vec![("token", token), ("token_type_hint", hint)]))
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Seconds since 1970 when the access token runs out; 0 when the server did not say.
    pub expires_at: i64,
}

impl Tokens {
    /// Whether the access token can still be sent at `now`.
    pub fn fresh(&self, now: i64) -> bool {
        self.expires_at == 0 || now + MARGIN_SECS < self.expires_at
    }
}

/// Why the token endpoint gave no tokens.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// It refused the grant (`invalid_grant` and its kind): signing in again is the only way on.
    Rejected(String),
    /// Something else went wrong; the grant may still be good.
    Failed(String),
}

/// Read the token endpoint's answer (JSON, or a form from a server that
/// answers so). A refresh that brings no new refresh token keeps `before`'s.
pub fn read_tokens(status: u16, body: &str, now: i64, before: Option<&Tokens>) -> Result<Tokens, Refusal> {
    let doc: Value = serde_json::from_str(body.trim()).unwrap_or_else(|_| pairs(body.trim()).into_iter().map(|(k, v)| (k, Value::String(v))).collect::<serde_json::Map<_, _>>().into());
    let text = |key: &str| doc[key].as_str().map(str::trim).unwrap_or("").to_string();
    if let Some(error) = doc["error"].as_str() {
        let described = text("error_description");
        let why = format!("授权服务器拒绝了：{error}{}", if described.is_empty() { String::new() } else { format!("（{described}）") });
        return Err(if matches!(status, 400 | 401) { Refusal::Rejected(why) } else { Refusal::Failed(why) });
    }
    if !(200..300).contains(&status) {
        let said: String = body.chars().take(200).collect();
        return Err(if matches!(status, 400 | 401) { Refusal::Rejected(format!("授权服务器拒绝了（HTTP {status}）：{said}")) } else { Refusal::Failed(format!("授权服务器返回 HTTP {status}：{said}")) });
    }
    let access = text("access_token");
    if access.is_empty() { return Err(Refusal::Failed("授权服务器的回答里没有 access_token。".into())) }
    let expires_in = doc["expires_in"].as_i64().or_else(|| doc["expires_in"].as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0);
    let refresh = Some(text("refresh_token")).filter(|r| !r.is_empty()).or_else(|| before.map(|before| before.refresh.clone())).unwrap_or_default();
    Ok(Tokens { access, refresh, expires_at: if expires_in > 0 { now + expires_in } else { 0 } })
}

/// What came to the loopback.
#[derive(Clone, Debug, PartialEq)]
pub enum Callback {
    /// The code to exchange.
    Code(String),
    /// The person or the server said no; why, in its words.
    Refused(String),
    /// An answer to another sign-in than this one.
    Mismatch,
    /// Not the callback at all (`/favicon.ico`).
    NotOurs,
}

/// Read the target of a request to the loopback (`/callback?code=…&state=…`).
pub fn callback(target: &str, state: &str) -> Callback {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK { return Callback::NotOurs }
    let pairs = pairs(query.split('#').next().unwrap_or(""));
    let get = |key: &str| pairs.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
    if get("state").as_deref() != Some(state) { return if get("code").is_some() || get("error").is_some() { Callback::Mismatch } else { Callback::NotOurs } }
    if let Some(error) = get("error") { return Callback::Refused(get("error_description").filter(|d| !d.is_empty()).unwrap_or(error)) }
    get("code").filter(|code| !code.is_empty()).map_or(Callback::NotOurs, Callback::Code)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page the browser shows when it comes back: signed in, or why not.
pub fn page(signed_in: bool, why: &str) -> String {
    let (title, line, english) = if signed_in { ("已登录", "可以关闭这个页面，回到 Wisp Paper。", "Signed in. You can close this tab and go back to Wisp Paper.") } else { ("没有登录", "回到 Wisp Paper 再试一次。", "Not signed in. Go back to Wisp Paper and try again.") };
    let why = if why.is_empty() { String::new() } else { format!("<p class=\"why\">{}</p>", escape(why)) };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Wisp Paper · {title}</title><style>\
         :root{{color-scheme:light dark}}body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#F5F4F1;color:#1F2329;font:15px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC','Microsoft YaHei',sans-serif}}\
         main{{max-width:420px;padding:32px;background:#fff;border:1px solid #E7E7E3;border-radius:12px}}h1{{font-size:20px;margin:0 0 8px}}p{{margin:4px 0;color:#5C636E}}.why{{color:#D4483B;word-break:break-word}}\
         @media (prefers-color-scheme:dark){{body{{background:#1A1B1D;color:#E6E7EA}}main{{background:#222326;border-color:#333539}}p{{color:#ABAFB7}}.why{{color:#EB7468}}}}</style></head>\
         <body><main><h1>{title}</h1><p>{line}</p><p>{english}</p>{why}</main></body></html>"
    )
}

/// The file the sign-ins are kept in: beside the settings file.
pub fn file(settings_file: &Path) -> PathBuf {
    settings_file.parent().unwrap_or(Path::new(".")).join("oauth.json")
}

/// A connector's sign-in is kept under its address as written.
pub fn key(target: &str) -> String {
    target.trim().to_string()
}

/// The sign-ins kept; none when the file is missing or cannot be read.
pub fn load(path: &Path) -> BTreeMap<String, Grant> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Write the sign-ins, readable by the person alone where the system has such a thing.
pub fn save(path: &Path, grants: &BTreeMap<String, Grant>) -> Result<(), String> {
    let failed = |err: std::io::Error| format!("无法保存登录信息 {}：{err}", path.display());
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir).map_err(failed)? }
    // Of this process alone: another one writing at the same time does not take it away.
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_string_pretty(grants).unwrap_or_default()).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, path).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_challenge_says_where_to_look() {
        let found = challenge(r#"Bearer realm="mcp", error="invalid_token", error_description="The \"token\" expired", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp", scope="read write""#);
        assert_eq!(found, Challenge { resource_metadata: Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp".into()), scope: Some("read write".into()), error: Some("invalid_token".into()) });
        // Another scheme first, a bare token value, no parameters at all.
        assert_eq!(challenge("Basic realm=x, Bearer error=insufficient_scope").error.as_deref(), Some("insufficient_scope"));
        assert_eq!(challenge("Bearer"), Challenge::default());
    }

    #[test]
    fn metadata_is_looked_for_where_the_rfcs_put_it() {
        assert_eq!(resource_metadata_urls("https://MCP.example.com/v1/mcp/?x=1"), ["https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp", "https://mcp.example.com/.well-known/oauth-protected-resource"]);
        assert_eq!(resource_metadata_urls("https://mcp.example.com"), ["https://mcp.example.com/.well-known/oauth-protected-resource"]);
        assert_eq!(server_metadata_urls("https://auth.example.com/tenant1"), ["https://auth.example.com/.well-known/oauth-authorization-server/tenant1", "https://auth.example.com/.well-known/openid-configuration/tenant1", "https://auth.example.com/tenant1/.well-known/openid-configuration"]);
        assert_eq!(server_metadata_urls("https://auth.example.com/").len(), 2);
        assert_eq!(legacy_endpoints("http://localhost:8790/mcp").token, "http://localhost:8790/token");
        assert!(secure("https://a.example") && secure("http://127.0.0.1:9/x") && secure("http://[::1]:9") && secure("http://localhost"));
        assert!(!secure("http://example.com") && !secure("javascript:alert(1)") && !secure("file:///etc/passwd") && !secure("http://localhost.evil.com"));
    }

    #[test]
    fn a_resource_names_its_server_and_no_other() {
        let doc = json!({"resource": "https://mcp.example.com", "authorization_servers": ["https://auth.example.com", "http://insecure.example"], "scopes_supported": ["mcp"]});
        let read = read_resource(&doc, "https://mcp.example.com/mcp").unwrap();
        assert_eq!((read.resource.as_str(), read.servers, read.scopes), ("https://mcp.example.com", vec!["https://auth.example.com".to_string()], vec!["mcp".to_string()]));
        // Without a resource the server stands for itself; another origin or a sibling path is refused.
        assert_eq!(read_resource(&json!({"authorization_servers": ["https://a.example"]}), "https://mcp.example.com/mcp").unwrap().resource, "https://mcp.example.com/mcp");
        assert!(read_resource(&json!({"resource": "https://evil.example/mcp", "authorization_servers": ["https://a.example"]}), "https://mcp.example.com/mcp").is_err());
        assert!(read_resource(&json!({"resource": "https://mcp.example.com/other", "authorization_servers": ["https://a.example"]}), "https://mcp.example.com/mcp").is_err());
        assert!(read_resource(&json!({"resource": "https://mcp.example.com/mcp"}), "https://mcp.example.com/mcp").is_err());
    }

    #[test]
    fn server_metadata_is_read_and_checked() {
        let doc = json!({"issuer": "https://auth.example.com", "authorization_endpoint": "https://auth.example.com/authorize", "token_endpoint": "https://auth.example.com/token", "registration_endpoint": "https://auth.example.com/register", "code_challenge_methods_supported": ["S256"], "token_endpoint_auth_methods_supported": ["none", "client_secret_post"]});
        let endpoints = read_server(&doc, "https://auth.example.com").unwrap();
        assert_eq!((endpoints.registration.as_str(), endpoints.revocation.as_str()), ("https://auth.example.com/register", ""));
        let mut plain = doc.clone();
        plain["code_challenge_methods_supported"] = json!(["plain"]);
        assert!(read_server(&plain, "https://auth.example.com").unwrap_err().contains("PKCE"));
        let mut opened = doc.clone();
        opened["authorization_endpoint"] = json!("javascript:alert(1)");
        assert!(read_server(&opened, "https://auth.example.com").is_err());
        assert!(read_server(&json!({"issuer": "x"}), "https://auth.example.com").is_err());
    }

    #[test]
    fn a_client_is_registered_public_and_read_back() {
        let body = registration(&redirect_uri(53682), "mcp");
        assert_eq!((body["redirect_uris"][0].as_str(), body["token_endpoint_auth_method"].as_str(), body["scope"].as_str()), (Some("http://localhost:53682/callback"), Some("none"), Some("mcp")));
        assert_eq!(redirect_port("http://localhost:53682/callback"), Some(53682));
        let endpoints = Endpoints { auth_methods: vec!["client_secret_post".into()], ..Default::default() };
        assert_eq!(read_client(&json!({"client_id": "abc"}), "r", &endpoints).unwrap().auth, "none");
        assert_eq!(read_client(&json!({"client_id": "abc", "client_secret": "s"}), "r", &endpoints).unwrap().auth, "client_secret_post");
        assert_eq!(read_client(&json!({"client_id": "abc", "client_secret": "s"}), "r", &Endpoints::default()).unwrap().auth, "client_secret_basic");
        assert!(read_client(&json!({}), "r", &endpoints).is_err());
    }

    #[test]
    fn pkce_follows_rfc_7636() {
        // RFC 7636, appendix B.
        let bytes = [116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121];
        let verifier = verifier(&bytes);
        assert_eq!(verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(code_challenge(&verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    fn grant() -> Grant {
        Grant {
            endpoints: Endpoints { issuer: "https://auth.example.com".into(), authorization: "https://auth.example.com/authorize?tenant=a".into(), token: "https://auth.example.com/token".into(), revocation: "https://auth.example.com/revoke".into(), ..Default::default() },
            client: Client { id: "client 1".into(), auth: "none".into(), redirect_uri: redirect_uri(8000), ..Default::default() },
            resource: "https://mcp.example.com/mcp".into(),
            scope: "read write".into(),
            tokens: Some(Tokens { access: "at".into(), refresh: "rt".into(), expires_at: 100 }),
        }
    }

    #[test]
    fn the_browser_and_the_token_endpoint_are_asked_with_everything_they_need() {
        let grant = grant();
        assert_eq!(authorize_url(&grant, "CH", "ST"), "https://auth.example.com/authorize?tenant=a&response_type=code&client_id=client%201&redirect_uri=http%3A%2F%2Flocalhost%3A8000%2Fcallback&code_challenge=CH&code_challenge_method=S256&state=ST&scope=read%20write&resource=https%3A%2F%2Fmcp.example.com%2Fmcp");
        let code = code_request(&grant, "c/1", "V");
        assert_eq!((code.url.as_str(), code.authorization), ("https://auth.example.com/token", None));
        assert_eq!(code.body, "grant_type=authorization_code&code=c%2F1&redirect_uri=http%3A%2F%2Flocalhost%3A8000%2Fcallback&code_verifier=V&resource=https%3A%2F%2Fmcp.example.com%2Fmcp&client_id=client%201");
        assert_eq!(refresh_request(&grant).unwrap().body, "grant_type=refresh_token&refresh_token=rt&resource=https%3A%2F%2Fmcp.example.com%2Fmcp&client_id=client%201");
        // A secret goes the way the client was told; no refresh token, no refresh.
        let mut basic = grant.clone();
        basic.client = Client { id: "id".into(), secret: "s:1".into(), auth: "client_secret_basic".into(), ..basic.client };
        assert_eq!(refresh_request(&basic).unwrap().authorization.as_deref(), Some(format!("Basic {}", base64(b"id:s%3A1")).as_str()));
        basic.client.auth = "client_secret_post".into();
        assert!(refresh_request(&basic).unwrap().body.ends_with("&client_id=id&client_secret=s%3A1"));
        basic.tokens = Some(Tokens { access: "at".into(), ..Default::default() });
        assert!(refresh_request(&basic).is_none());
        assert_eq!(revoke_requests(&grant).iter().map(|r| r.body.as_str()).collect::<Vec<_>>(), ["token=rt&token_type_hint=refresh_token&client_id=client%201", "token=at&token_type_hint=access_token&client_id=client%201"]);
    }

    #[test]
    fn tokens_are_read_kept_and_refused() {
        let before = Tokens { access: "old".into(), refresh: "keep".into(), expires_at: 0 };
        assert_eq!(read_tokens(200, r#"{"access_token":"a","token_type":"Bearer","expires_in":3600,"refresh_token":"r"}"#, 1000, None), Ok(Tokens { access: "a".into(), refresh: "r".into(), expires_at: 4600 }));
        assert_eq!(read_tokens(200, r#"{"access_token":"b","expires_in":"60"}"#, 1000, Some(&before)), Ok(Tokens { access: "b".into(), refresh: "keep".into(), expires_at: 1060 }));
        assert_eq!(read_tokens(200, "access_token=c&scope=repo&token_type=bearer", 1000, None).unwrap().access, "c");
        assert!(matches!(read_tokens(400, r#"{"error":"invalid_grant","error_description":"expired"}"#, 0, None), Err(Refusal::Rejected(why)) if why.contains("invalid_grant") && why.contains("expired")));
        assert!(matches!(read_tokens(503, "busy", 0, None), Err(Refusal::Failed(_))));
        assert!(matches!(read_tokens(200, "{}", 0, None), Err(Refusal::Failed(_))));
        assert!(Tokens { expires_at: 0, ..Default::default() }.fresh(i64::MAX - 100) && Tokens { expires_at: 1100, ..Default::default() }.fresh(1000));
        assert!(!Tokens { expires_at: 1030, ..Default::default() }.fresh(1000));
    }

    #[test]
    fn the_callback_is_told_from_what_else_comes() {
        assert_eq!(callback("/callback?code=a%2Bb&state=S", "S"), Callback::Code("a+b".into()));
        assert_eq!(callback("/callback?error=access_denied&error_description=The+user+said+no&state=S", "S"), Callback::Refused("The user said no".into()));
        assert_eq!(callback("/callback?error=access_denied&state=S", "S"), Callback::Refused("access_denied".into()));
        assert_eq!(callback("/callback?code=a&state=T", "S"), Callback::Mismatch);
        assert_eq!((callback("/favicon.ico", "S"), callback("/callback", "S")), (Callback::NotOurs, Callback::NotOurs));
        assert_eq!(decode("%E4%BD%A0%zz+x"), "你%zz x");
        assert!(page(false, "<script>").contains("&lt;script&gt;") && page(true, "").contains("已登录"));
    }

    #[test]
    fn sign_ins_are_kept_in_a_file_of_their_own() {
        let dir = std::env::temp_dir().join(format!("wisp-oauth-{}", std::process::id()));
        let path = file(&dir.join("settings.json"));
        assert_eq!(path, dir.join("oauth.json"));
        assert!(load(&path).is_empty());
        let mut grants = BTreeMap::new();
        grants.insert(key(" https://mcp.example.com/mcp "), grant());
        save(&path, &grants).unwrap();
        assert_eq!(load(&path).get("https://mcp.example.com/mcp"), Some(&grant()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(grant().account(), "auth.example.com");
        let _ = std::fs::remove_dir_all(dir);
    }
}
