//! The network: metadata lookups, literature searches, the streamed chat
//! request and the update check. Shared by the app and `wisp`. Everything
//! here blocks, so the app runs it on the background executor.

pub mod mcp;
pub mod oauth;

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use wisp_core::ai::{self, ChatEvent, HttpRequest, Protocol, TurnReader};
use wisp_core::imagegen;
use wisp_core::lookup::{self, Identifier};
use wisp_core::model::Paper;
use wisp_core::update::{self, Release};

const USER_AGENT: &str = concat!("WispPaper/", env!("CARGO_PKG_VERSION"));

/// The proxy every request of this process goes through, from the settings.
static PROXY: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Set the global proxy (`http://…`, `socks5://…`); empty leaves the choice
/// to the environment (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY`).
pub fn set_proxy(address: &str) {
    *PROXY.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = address.trim().to_string();
}

/// Which proxy a request takes. `own` is the setting of whoever asks (a
/// model profile): an address of its own, `direct` for none at all, or empty
/// for the global one, which when empty too is the environment's.
fn proxy_for(own: &str) -> Result<Option<ureq::Proxy>, String> {
    let own = own.trim();
    if own.eq_ignore_ascii_case("direct") { return Ok(None) }
    let global = PROXY.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let address = if own.is_empty() { global.as_str() } else { own };
    if address.is_empty() { return Ok(ureq::Proxy::try_from_env()) }
    ureq::Proxy::new(address).map(Some).map_err(|err| format!("代理地址 {address} 无法使用：{err}"))
}

fn agent(overall: Option<Duration>, proxy: &str) -> Result<ureq::Agent, String> {
    patient_agent(overall, proxy, Duration::from_secs(120))
}

/// An agent that waits `answer` for a response to begin.
fn patient_agent(overall: Option<Duration>, proxy: &str, answer: Duration) -> Result<ureq::Agent, String> {
    Ok(ureq::Agent::config_builder()
        // Error responses carry the reason in their body; read it rather than a bare status.
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(answer))
        .timeout_global(overall)
        .user_agent(USER_AGENT)
        .proxy(proxy_for(proxy)?)
        .build()
        .into())
}

fn network_error(err: ureq::Error) -> String {
    format!("网络请求失败：{err}")
}

/// GET a JSON document; `None` for a 404.
fn get_json(url: &str) -> Result<Option<Value>, String> {
    let mut response = agent(Some(Duration::from_secs(40)), "")?.get(url).call().map_err(network_error)?;
    let status = response.status().as_u16();
    if status == 404 { return Ok(None) }
    if !(200..300).contains(&status) { return Err(format!("服务返回 HTTP {status}：{url}")) }
    response.body_mut().read_json::<Value>().map(Some).map_err(|e| format!("无法解析返回内容：{e}"))
}

/// GET a document as text: what `wisp_core::sources` asks the literature
/// databases for (JSON from most, XML from PubMed and arXiv).
pub fn get_text(url: &str) -> Result<String, String> {
    let host = url.split('/').nth(2).unwrap_or(url);
    // A service that says "too fast" is asked again, a little later each time.
    for wait in [Some(1500), Some(4000), None] {
        take_turn(host);
        let mut response = agent(Some(Duration::from_secs(60)), "")?.get(url).call().map_err(network_error)?;
        let status = response.status().as_u16();
        match (status, wait) {
            (200..300, _) => return response.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| format!("无法读取 {host} 的返回内容：{e}")),
            (429, Some(millis)) => std::thread::sleep(Duration::from_millis(millis)),
            (429, None) => break,
            _ => return Err(format!("{host} 返回 HTTP {status}。")),
        }
    }
    Err(format!("{host} 限制了请求频率，请稍后再试。"))
}

/// The most a fetched file may be: a paper with its figures is a few megabytes, a book a few dozen.
const MAX_DOWNLOAD: u64 = 120 << 20;

/// GET a file as bytes: what `find_pdf` asks the open-access addresses
/// for. Whether it is the PDF hoped for is the caller's to check; a page
/// that asks to log in comes back with 200 as well.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let host = url.split('/').nth(2).unwrap_or(url);
    let mut response = agent(Some(Duration::from_secs(180)), "")?.get(url).header("Accept", "application/pdf,*/*;q=0.8").call().map_err(network_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) { return Err(format!("{host} 返回 HTTP {status}。")) }
    response.body_mut().with_config().limit(MAX_DOWNLOAD).read_to_vec().map_err(|e| format!("无法读取 {host} 的返回内容：{e}"))
}

/// NCBI allows three requests a second without a key, counted across
/// everything this process sends: searches running side by side take turns,
/// one every 400 ms. Other hosts are not held back.
fn take_turn(host: &str) {
    static NEXT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if !host.ends_with("ncbi.nlm.nih.gov") { return }
    let now = std::time::Instant::now();
    let at = {
        let mut next = NEXT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let at = next.filter(|at| *at > now).unwrap_or(now);
        *next = Some(at + Duration::from_millis(400));
        at
    };
    std::thread::sleep(at.saturating_duration_since(now));
}

/// The paper behind a DOI or PMID: Crossref first for a DOI, completed by
/// Europe PMC (abstract, PMID); Europe PMC alone for a PMID.
pub fn lookup_paper(id: &Identifier) -> Result<Paper, String> {
    let mut notes = vec![];
    let mut paper: Option<Paper> = None;
    if let Identifier::Doi(doi) = id {
        match get_json(&lookup::crossref_url(doi)) {
            Ok(Some(body)) => paper = lookup::crossref_paper(&body),
            Ok(None) => {}
            Err(err) => notes.push(format!("Crossref：{err}")),
        }
    }
    match get_json(&lookup::europepmc_url(id)) {
        Ok(Some(body)) => {
            if let Some(extra) = lookup::europepmc_paper(&body) {
                paper = Some(match paper {
                    Some(found) => lookup::merge_lookup(found, extra),
                    None => extra,
                });
            }
        }
        Ok(None) => {}
        Err(err) => notes.push(format!("Europe PMC：{err}")),
    }
    paper.ok_or_else(|| if notes.is_empty() { "没有找到这个编号对应的文献，请检查后重试，或改用手动录入。".to_string() } else { notes.join("；") })
}

/// The newest published release, read off the redirect `url` answers with
/// (`update::latest_url()`); `None` when nothing has been published. Only
/// the redirect is read: no page or package is fetched.
pub fn latest_release(url: &str) -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(20))).user_agent(USER_AGENT).proxy(proxy_for("")?).build().into();
    let response = agent.get(url).call().map_err(network_error)?;
    let status = response.status().as_u16();
    match status {
        300..400 => Ok(response.headers().get("location").and_then(|v| v.to_str().ok()).and_then(update::release_from_location)),
        200..300 | 404 => Ok(None),
        _ => Err(format!("更新服务器返回 HTTP {status}")),
    }
}

/// How long to wait before each further try of a request the service could
/// not take: two more tries, as the model SDKs make.
const RETRY_WAITS: [u64; 2] = if cfg!(test) { [10, 10] } else { [1000, 3000] };

/// The longest a service's own `retry-after` is waited for, in milliseconds.
const RETRY_AFTER_CAP: u64 = 20_000;

/// Statuses that say "not now", not "not this": too many requests, a
/// service that is overloaded or failing, a request that timed out there.
fn retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

/// Send a chat request and forward the model's turn: its text as it
/// arrives, then the whole turn with any tool calls. A request the service
/// is too busy for, or that never reached it, is sent again a little later,
/// up to twice; nothing of an answer has arrived by then, so nothing is
/// repeated. Returns when the stream ends or the receiver is gone (the
/// conversation was reset or stopped).
pub fn stream_chat(request: HttpRequest, protocol: Protocol, events: async_channel::Sender<ChatEvent>) {
    let send = |event| events.send_blocking(event).is_ok();
    // The model service may have a proxy of its own, or be told to use none.
    let agent = match agent(None, &request.proxy) {
        Ok(agent) => agent,
        Err(err) => {
            send(ChatEvent::Error(err));
            return;
        }
    };
    // Compact JSON: the documents can be hundreds of kilobytes, and indentation only adds to them.
    let body = request.body.to_string();
    let mut tries = 0;
    let mut response = loop {
        let mut call = agent.post(&request.url);
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }
        let last = tries >= RETRY_WAITS.len();
        let (why, wait) = match call.send(body.as_str()) {
            Ok(response) => {
                let status = response.status().as_u16();
                if last || !retryable(status) { break response }
                // The service may say how long it wants to be left alone.
                let asked = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok()).map(|seconds| ((seconds.max(0.) * 1000.) as u64).min(RETRY_AFTER_CAP));
                (format!("HTTP {status}"), asked)
            }
            // A request that waited two minutes for nothing is not worth two more waits.
            Err(err) if last || matches!(err, ureq::Error::Timeout(_)) => {
                send(ChatEvent::Error(network_error(err)));
                return;
            }
            Err(err) => (network_error(err), None),
        };
        let wait = wait.unwrap_or(RETRY_WAITS[tries]);
        tries += 1;
        if !send(ChatEvent::Retrying(tries, why)) { return }
        std::thread::sleep(Duration::from_millis(wait));
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        send(ChatEvent::Error(ai::error_message(status, &body)));
        return;
    }
    let mut reader = TurnReader::new(protocol);
    for line in BufReader::new(response.body_mut().as_reader()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                send(ChatEvent::Error(format!("连接中断：{err}")));
                return;
            }
        };
        match reader.line(&line) {
            Ok(Some(text)) => {
                if !send(ChatEvent::Text(text)) { return }
            }
            Ok(None) => {}
            Err(error) => {
                send(ChatEvent::Error(error));
                return;
            }
        }
    }
    send(ChatEvent::Done(reader.finish()));
}

/// Send a request and wait for the whole answer as text: for a one-off
/// question whose reply is read by code, not shown as it arrives.
pub fn complete(request: HttpRequest, protocol: Protocol) -> Result<String, String> {
    let (sender, events) = async_channel::unbounded();
    stream_chat(request, protocol, sender);
    let mut text = String::new();
    while let Ok(event) = events.try_recv() {
        match event {
            ChatEvent::Text(more) => text.push_str(&more),
            ChatEvent::Retrying(..) => {}
            ChatEvent::Error(error) => return Err(error),
            // A reply that was cut off or declined is not one to parse.
            ChatEvent::Done(turn) => {
                if let Some(note) = ai::stop_note(&turn.stop) { return Err(note.to_string()) }
            }
        }
    }
    if text.trim().is_empty() { return Err("模型没有返回内容。".into()) }
    Ok(text)
}

/// How long a service may take over one picture: the large ones take minutes.
const PICTURE_WAIT: Duration = Duration::from_secs(300);

/// The most an answer with a picture in it, or the picture itself, may be.
const MAX_PICTURE: u64 = 96 << 20;

/// Ask a model service for a picture (`imagegen::request`) and wait for
/// it: the bytes of a PNG, JPEG or WebP. A service too busy to take the
/// request is asked again like a chat request; one that answers with an
/// address has the picture fetched from there, through the same proxy.
pub fn generate_image(request: HttpRequest, interface: imagegen::Interface) -> Result<Vec<u8>, String> {
    let reply = post_json(&request, PICTURE_WAIT)?;
    let picture = match imagegen::read(interface, &reply)? {
        imagegen::Made::Picture(bytes) => bytes,
        imagegen::Made::Address(url) => {
            let mut response = patient_agent(Some(Duration::from_secs(180)), &request.proxy, Duration::from_secs(120))?.get(&url).call().map_err(network_error)?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) { return Err(format!("图片画好了，但取不回来（HTTP {status}）。")) }
            response.body_mut().with_config().limit(MAX_PICTURE).read_to_vec().map_err(|e| format!("图片画好了，但取不回来：{e}"))?
        }
    };
    if wisp_core::figure::picture_kind(&picture).is_none() { return Err("模型服务返回的不是 PNG、JPEG 或 WebP 图片。".into()) }
    Ok(picture)
}

/// Ask a model that judges (`judge::request`) and wait for its answer, a
/// JSON document read by `judge::read_screening`. These models answer in a
/// fraction of a second; a busy service is asked again like any other.
pub fn judge(request: HttpRequest) -> Result<Value, String> {
    post_json(&request, Duration::from_secs(60))
}

/// POST a request whose answer is one JSON document, waiting `answer` for
/// it to begin. A service too busy to take it (or never reached) is asked
/// again, up to twice; an error response is said in the service's words.
fn post_json(request: &HttpRequest, answer: Duration) -> Result<Value, String> {
    let agent = patient_agent(None, &request.proxy, answer)?;
    let body = request.body.to_string();
    let mut tries = 0;
    let mut response = loop {
        let mut call = agent.post(&request.url);
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }
        let last = tries >= RETRY_WAITS.len();
        match call.send(body.as_str()) {
            Ok(response) if last || !retryable(response.status().as_u16()) => break response,
            Ok(_) => {}
            // Minutes were waited already: asking again would only double them.
            Err(err) if last || matches!(err, ureq::Error::Timeout(_)) => return Err(network_error(err)),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(RETRY_WAITS[tries]));
        tries += 1;
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(ai::error_message(status, &body));
    }
    response.body_mut().with_config().limit(MAX_PICTURE).read_json::<Value>().map_err(|e| format!("无法解析模型服务的返回内容：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use wisp_core::ai::{Context, Profile, chat_request, user_message};

    /// Serve one canned HTTP response on a local port; returns the base URL and the request it received.
    pub(crate) fn serve(response: &'static str) -> (String, std::thread::JoinHandle<String>) {
        serve_in_turn(vec![response])
    }

    /// Serve one canned response to each connection in turn; returns the base URL and the last request received.
    pub(crate) fn serve_in_turn(responses: Vec<&'static str>) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut last = String::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut received = vec![];
                let mut buffer = [0u8; 4096];
                // Read the headers and the body they announce.
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    received.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&received).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length: usize = text.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                        // ureq sends a JSON body chunked: it ends with a zero-length chunk.
                        let chunked = text.to_lowercase().contains("transfer-encoding: chunked");
                        if (chunked && text.ends_with("0\r\n\r\n")) || (!chunked && received.len() >= end + 4 + length) { break }
                    }
                    if n == 0 { break }
                }
                stream.write_all(response.as_bytes()).unwrap();
                last = String::from_utf8_lossy(&received).to_string();
            }
            last
        });
        (base, handle)
    }

    /// Send the conversation `wire` to the server at `base` and collect what comes back.
    fn step(base: String, protocol: Protocol, wire: &[Value]) -> (Vec<ChatEvent>, HttpRequest) {
        let profile = Profile { protocol, base_url: base, api_key: "test-key".into(), model: "m".into(), ..Default::default() };
        let ctx = Context { instructions: "I".into(), documents: "D".into(), ..Default::default() };
        let request = chat_request(&profile, &ctx, wire, &wisp_core::tools::library_catalog()).unwrap();
        let (sender, events) = async_channel::unbounded();
        stream_chat(request.clone(), protocol, sender);
        let mut seen = vec![];
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        (seen, request)
    }

    fn ask(base: String, protocol: Protocol) -> (Vec<ChatEvent>, HttpRequest) {
        step(base, protocol, &[user_message("你好")])
    }

    /// One question answered in two steps: the model calls a tool, gets the
    /// result, and answers. What matters is what the second request carries.
    #[test]
    fn tool_round_trip() {
        let (base, server) = serve(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"c2ln\"}}\n\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"search_fulltext\",\"input\":{}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\": \\\"chromium\\\"}\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        );
        let mut wire = vec![user_message("哪篇文献提到 Chromium？")];
        let (events, _) = step(base, Protocol::Anthropic, &wire);
        server.join().unwrap();
        let [ChatEvent::Done(turn)] = &events[..] else { panic!("{events:?}") };
        assert_eq!((turn.calls[0].name.as_str(), turn.calls[0].args.clone()), ("search_fulltext", Ok(serde_json::json!({"query": "chromium"}))));
        assert!(turn.runnable());

        wire.push(turn.message.clone());
        wire.extend(ai::tool_results(Protocol::Anthropic, &[(turn.calls[0].id.clone(), "{\"total\":1}".into(), false)]));
        let (base, server) = serve(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"[P1 p.2] 提到了。\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        );
        let (events, request) = step(base, Protocol::Anthropic, &wire);
        server.join().unwrap();
        assert_eq!(events[0], ChatEvent::Text("[P1 p.2] 提到了。".into()));
        assert!(matches!(&events[1], ChatEvent::Done(turn) if turn.calls.is_empty() && turn.stop == "end_turn"));
        // The model's turn went back whole, followed by the result of its call.
        let sent = &request.body["messages"];
        assert_eq!(sent[1]["content"][0], serde_json::json!({"type": "thinking", "thinking": "", "signature": "c2ln"}));
        assert_eq!(sent[1]["content"][1]["input"]["query"], "chromium");
        assert_eq!(sent[2]["content"][0], serde_json::json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"total\":1}", "is_error": false}));
    }

    #[test]
    fn anthropic_stream() {
        let (base, server) = serve(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"c2ln\"}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"根尖 \"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"[P1]\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let (events, _) = ask(base, Protocol::Anthropic);
        assert_eq!(events[..2], [ChatEvent::Text("根尖 ".into()), ChatEvent::Text("[P1]".into())]);
        let ChatEvent::Done(turn) = &events[2] else { panic!("{events:?}") };
        // The turn goes back to the model as it came, the signed thinking block included.
        assert_eq!((turn.stop.as_str(), turn.calls.len(), turn.message["content"][0]["signature"].as_str(), turn.message["content"][1]["text"].as_str()), ("end_turn", 0, Some("c2ln"), Some("根尖 [P1]")));
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /v1/messages "), "{received}");
        assert!(received.to_lowercase().contains("x-api-key: test-key"));
        assert!(received.contains("\"stream\":true") && received.contains("你好") && received.contains("\"search_fulltext\""), "{received}");
        // A local address is not Anthropic's API: no beta parameters.
        assert!(!received.contains("fallbacks"));
    }

    #[test]
    fn responses_stream() {
        let (base, server) = serve(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"根尖 [P1]\"}\n\n\
event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"根尖 [P1]\"}]}}\n\n\
event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        );
        let (events, request) = ask(format!("{base}/v1"), Protocol::Responses);
        assert_eq!(events[0], ChatEvent::Text("根尖 [P1]".into()));
        assert!(matches!(&events[1], ChatEvent::Done(turn) if turn.stop == "stop" && turn.wire()[0]["id"] == "msg_1"));
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /v1/responses "), "{received}");
        assert!(received.to_lowercase().contains("authorization: bearer test-key"));
        assert_eq!((request.body["store"].as_bool(), request.body["input"][0]["content"].as_str()), (Some(false), Some("你好")));
    }

    /// A profile's own proxy takes its request; `direct` and a bad address are what they say.
    #[test]
    fn profile_proxy() {
        let reply = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n";
        let ask_through = |base: &str, proxy: &str| {
            let profile = Profile { protocol: Protocol::OpenAI, base_url: base.into(), api_key: "k".into(), model: "m".into(), proxy: proxy.into(), ..Default::default() };
            let request = chat_request(&profile, &Context { instructions: "I".into(), documents: "D".into(), ..Default::default() }, &[user_message("你好")], &[]).unwrap();
            let (sender, events) = async_channel::unbounded();
            stream_chat(request, Protocol::OpenAI, sender);
            std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>()
        };
        // A proxy that grants the tunnel it is asked for and then answers inside it itself. The
        // service's name resolves nowhere: only the proxy can have answered.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut read_until = |end: &str| {
                let mut received = vec![];
                let mut buffer = [0u8; 4096];
                while !String::from_utf8_lossy(&received).ends_with(end) {
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 { break }
                    received.extend_from_slice(&buffer[..n]);
                }
                String::from_utf8_lossy(&received).to_string()
            };
            let asked = read_until("\r\n\r\n");
            let mut stream = stream;
            stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
            let mut received = vec![];
            let mut buffer = [0u8; 4096];
            // The whole request: its body is either counted or chunked, ending with a zero-length chunk.
            loop {
                let n = stream.read(&mut buffer).unwrap();
                received.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&received).to_lowercase();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if (text.contains("transfer-encoding: chunked") && text.ends_with("0\r\n\r\n")) || (!text.contains("transfer-encoding: chunked") && received.len() >= end + 4 + length) { break }
                }
                if n == 0 { break }
            }
            stream.write_all(reply.as_bytes()).unwrap();
            (asked, String::from_utf8_lossy(&received).to_string())
        });
        let events = ask_through("http://model.invalid/v1", &proxy);
        assert_eq!(events[0], ChatEvent::Text("Hi".into()), "{events:?}");
        let (asked, inner) = server.join().unwrap();
        assert!(asked.starts_with("CONNECT model.invalid:80 "), "{asked}");
        assert!(inner.starts_with("POST /v1/chat/completions "), "{inner}");

        let (base, server) = serve(reply);
        assert_eq!(ask_through(&format!("{base}/v1"), "direct")[0], ChatEvent::Text("Hi".into()));
        assert!(server.join().unwrap().starts_with("POST /v1/chat/completions "));

        let events = ask_through("http://model.invalid/v1", "not a proxy");
        assert!(matches!(&events[..], [ChatEvent::Error(e)] if e.contains("代理地址")), "{events:?}");
    }

    #[test]
    fn openai_stream_and_errors() {
        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
        let (events, _) = ask(format!("{base}/v1"), Protocol::OpenAI);
        assert_eq!(events[0], ChatEvent::Text("Hi".into()));
        assert!(matches!(&events[1], ChatEvent::Done(turn) if turn.stop == "stop" && turn.message["content"] == "Hi"));
        assert!(server.join().unwrap().starts_with("POST /v1/chat/completions "));

        let (base, server) = serve("HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 41\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"invalid x-api-key\"}}");
        let (events, _) = ask(base, Protocol::Anthropic);
        assert_eq!(events, [ChatEvent::Error("API Key 无效或没有权限（HTTP 401）：invalid x-api-key".into())]);
        server.join().unwrap();

        // Nothing listening: tried three times, then a network error, not a hang.
        let (events, _) = ask("http://127.0.0.1:9".into(), Protocol::OpenAI);
        assert!(matches!(&events[..], [ChatEvent::Retrying(1, _), ChatEvent::Retrying(2, _), ChatEvent::Error(e)] if e.starts_with("网络请求失败")), "{events:?}");
    }

    #[test]
    fn a_busy_service_is_asked_again() {
        const BUSY: &str = "HTTP/1.1 529 Overloaded\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}";
        const LIMITED: &str = "HTTP/1.1 429 Too Many Requests\r\nretry-after: 0.01\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        const ANSWER: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n";
        // Overloaded, then rate-limited, then answered: the answer arrives once, after two notices.
        let (base, server) = serve_in_turn(vec![BUSY, LIMITED, ANSWER]);
        let (events, _) = ask(format!("{base}/v1"), Protocol::OpenAI);
        assert_eq!(events[..3], [ChatEvent::Retrying(1, "HTTP 529".into()), ChatEvent::Retrying(2, "HTTP 429".into()), ChatEvent::Text("Hi".into())], "{events:?}");
        assert!(matches!(&events[3], ChatEvent::Done(turn) if turn.stop == "stop"));
        server.join().unwrap();
        // Still busy on the third try: the service's own words, and no fourth request.
        let (base, server) = serve_in_turn(vec![BUSY, BUSY, BUSY]);
        let (events, _) = ask(base, Protocol::Anthropic);
        assert!(matches!(&events[..], [ChatEvent::Retrying(1, _), ChatEvent::Retrying(2, _), ChatEvent::Error(e)] if e.contains("Overloaded")), "{events:?}");
        server.join().unwrap();
    }

    /// A picture asked for through each interface: what is sent where, and what comes back.
    #[test]
    fn pictures() {
        // "\x89PNG\r\n\x1a\n" in base64.
        const PNG: &str = "iVBORw0KGgo=";
        let profile = |base: String, model: &str, interface: &str| Profile { protocol: Protocol::OpenAI, base_url: base, api_key: "test-key".into(), model: model.into(), interface: interface.into(), ..Default::default() };
        let draw = |profile: &Profile| generate_image(imagegen::request(profile, "a root tip", imagegen::Shape::Wide).unwrap(), imagegen::interface(profile));

        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"created\":1,\"data\":[{\"b64_json\":\"iVBORw0KGgo=\"}]}");
        assert_eq!(draw(&profile(format!("{base}/v1"), "gpt-image-2", "")).as_deref(), Ok(&b"\x89PNG\r\n\x1a\n"[..]));
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /v1/images/generations "), "{received}");
        assert!(received.to_lowercase().contains("authorization: bearer test-key") && received.contains("\"size\":\"1536x1024\"") && received.contains("a root tip"), "{received}");

        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Here.\"},{\"inlineData\":{\"mimeType\":\"image/png\",\"data\":\"iVBORw0KGgo=\"}}]}}]}");
        assert_eq!(draw(&profile(format!("{base}/v1beta/openai"), "gemini-3-pro-image-preview", "gemini")).map(|bytes| bytes.len()), Ok(8));
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /v1beta/models/gemini-3-pro-image-preview:generateContent "), "{received}");
        assert!(received.to_lowercase().contains("x-goog-api-key: test-key") && received.contains("\"aspectRatio\":\"3:2\""), "{received}");

        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"images\":[{\"type\":\"image_url\",\"image_url\":{\"url\":\"data:image/png;base64,iVBORw0KGgo=\"}}]}}]}");
        assert_eq!(draw(&profile(format!("{base}/v1"), "gemini-3.1-flash-image-preview", "")).map(|bytes| bytes.len()), Ok(8));
        assert!(server.join().unwrap().starts_with("POST /v1/chat/completions "));
        assert_eq!(PNG, ai::base64(b"\x89PNG\r\n\x1a\n"));

        // Words in place of a picture, bytes that are no picture, and a refusal are each said as they are.
        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"I can only describe it.\"}}]}");
        assert_eq!(draw(&profile(format!("{base}/v1"), "some-model", "chat")), Err("模型没有返回图片，它说：I can only describe it.".into()));
        server.join().unwrap();
        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"data\":[{\"b64_json\":\"JVBERi0xLjc=\"}]}");
        assert!(draw(&profile(format!("{base}/v1"), "gpt-image-2", "")).unwrap_err().contains("不是 PNG"));
        server.join().unwrap();
        let (base, server) = serve("HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"Your request was rejected by the safety system.\"}}");
        assert_eq!(draw(&profile(format!("{base}/v1"), "gpt-image-2", "")), Err("请求被拒绝（HTTP 400）：Your request was rejected by the safety system.".into()));
        server.join().unwrap();
    }

    /// A paper judged by a model that judges: what is sent where, what is made of the answer, and how its errors read.
    #[test]
    fn judgements() {
        let paper = Paper { title: "A single-cell atlas of the root".into(), abstract_text: "We profile 100,000 cells.".into(), ..Default::default() };
        let jev = |base: String| Profile { protocol: Protocol::OpenAI, base_url: format!("{base}/v1"), api_key: "test-key".into(), model: "jev-latest".into(), kind: wisp_core::judge::KIND.into(), ..Default::default() };
        let ask = |base: String| judge(wisp_core::judge::screen_request(&jev(base), "root development", "", &paper, None).unwrap().0);

        let (base, server) = serve("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"model\":\"jev-1.13.0\",\"answers\":{\"verdict\":{\"type\":\"choice\",\"choice\":\"include\",\"confidence\":0.88,\"probabilities\":{\"include\":0.93,\"related\":0.05,\"irrelevant\":0.02}}},\"usage\":{\"input_tokens\":212,\"output_tokens\":1}}");
        let judged = wisp_core::judge::read_screening(&ask(base).unwrap()).unwrap();
        assert_eq!((judged.verdict.as_str(), judged.confidence), ("included", Some(0.88)));
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /v1/systemone "), "{received}");
        assert!(received.to_lowercase().contains("authorization: bearer test-key") && received.contains("\"type\":\"choice\"") && received.contains("A single-cell atlas of the root"), "{received}");

        // What is wrong with a request comes as `detail`: a sentence, or a list naming the field.
        let (base, server) = serve("HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"detail\":\"Invalid API key\"}");
        assert_eq!(ask(base), Err("API Key 无效或没有权限（HTTP 401）：Invalid API key".into()));
        server.join().unwrap();
        let (base, server) = serve("HTTP/1.1 422 Unprocessable Entity\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"detail\":[{\"loc\":[\"body\",\"model\"],\"msg\":\"Unknown model\",\"type\":\"value_error\"}]}");
        assert_eq!(ask(base), Err("请求被拒绝（HTTP 422）：Unknown model".into()));
        server.join().unwrap();
        // Overloaded twice, then answered.
        const BUSY: &str = "HTTP/1.1 529 Overloaded\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let (base, server) = serve_in_turn(vec![BUSY, BUSY, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"answers\":{\"verdict\":{\"choice\":\"irrelevant\",\"confidence\":0.95}}}"]);
        assert_eq!(ask(base).map(|reply| wisp_core::judge::read_screening(&reply).map(|j| j.verdict)), Ok(Ok("irrelevant".to_string())));
        server.join().unwrap();
    }

    /// A service that answers with an address: the picture is fetched from there.
    #[test]
    fn a_picture_at_an_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let address = format!("{base}/out/a.png");
        let server = std::thread::spawn(move || {
            let mut asked = vec![];
            for n in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut received = vec![];
                let mut buffer = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    received.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&received).to_lowercase();
                    // The whole request: its body is either counted or chunked, ending with a zero-length chunk.
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length: usize = text.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                        let chunked = text.contains("transfer-encoding: chunked");
                        if (chunked && text.ends_with("0\r\n\r\n")) || (!chunked && received.len() >= end + 4 + length) { break }
                    }
                    if read == 0 { break }
                }
                asked.push(String::from_utf8_lossy(&received).to_string());
                if n == 0 {
                    let body = format!("{{\"data\":[{{\"url\":\"{address}\"}}]}}");
                    stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
                } else {
                    stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: image/jpeg\r\ncontent-length: 4\r\nconnection: close\r\n\r\n\xff\xd8\xff\xe0").unwrap();
                }
            }
            asked
        });
        let profile = Profile { protocol: Protocol::OpenAI, base_url: format!("{base}/api/v3"), api_key: "k".into(), model: "doubao-seedream-4-0-250828".into(), ..Default::default() };
        let picture = generate_image(imagegen::request(&profile, "根尖", imagegen::Shape::Square).unwrap(), imagegen::interface(&profile));
        assert_eq!(picture.as_deref(), Ok(&b"\xff\xd8\xff\xe0"[..]));
        let asked = server.join().unwrap();
        assert!(asked[0].starts_with("POST /api/v3/images/generations ") && asked[0].contains("\"watermark\":false"), "{}", asked[0]);
        assert!(asked[1].starts_with("GET /out/a.png "), "{}", asked[1]);
    }

    #[test]
    fn latest_release_reads_the_redirect() {
        let (base, server) = serve("HTTP/1.1 302 Found\r\nlocation: https://github.com/xuzhougeng/WispPaper/releases/tag/v9.8.7\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let release = latest_release(&format!("{base}/releases/latest")).unwrap().unwrap();
        assert_eq!((release.version.as_str(), release.url.as_str()), ("9.8.7", "https://github.com/xuzhougeng/WispPaper/releases/tag/v9.8.7"));
        assert!(server.join().unwrap().starts_with("GET /releases/latest "));

        // No release published: GitHub redirects to the list. Elsewhere: never offered.
        for response in ["HTTP/1.1 302 Found\r\nlocation: https://github.com/xuzhougeng/WispPaper/releases\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", "HTTP/1.1 302 Found\r\nlocation: https://example.com/releases/tag/v9.9.9\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"] {
            let (base, server) = serve(response);
            assert_eq!(latest_release(&base), Ok(None));
            server.join().unwrap();
        }
        let (base, server) = serve("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        assert_eq!(latest_release(&base), Err("更新服务器返回 HTTP 503".into()));
        server.join().unwrap();
        assert!(latest_release("http://127.0.0.1:9").unwrap_err().starts_with("网络请求失败"));
    }

    /// Talks to Crossref and Europe PMC: `cargo test -- --ignored lookup`.
    #[test]
    #[ignore]
    fn lookup_real_identifiers() {
        let paper = lookup_paper(&Identifier::Doi("10.1038/s41586-020-2649-2".into())).unwrap();
        assert_eq!(paper.title, "Array programming with NumPy");
        assert_eq!((paper.year, paper.journal.as_str(), paper.pmid.as_str()), (Some(2020), "Nature", "32939066"));
        assert!(paper.authors.starts_with("Harris, Charles R.") && !paper.abstract_text.is_empty());
        let by_pmid = lookup_paper(&Identifier::Pmid("32939066".into())).unwrap();
        assert_eq!(by_pmid.doi, "10.1038/s41586-020-2649-2");
        assert!(lookup_paper(&Identifier::Doi("10.9999/definitely-not-a-real-doi-000".into())).is_err());
    }
}
