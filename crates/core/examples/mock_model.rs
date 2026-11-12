//! A stand-in for a model service, for trying the assistant's tool loop
//! without an API key:
//!
//!     cargo run -p wisp_core --example mock_model -- 8787 [milliseconds]
//!
//! Point a profile at it (协议 Anthropic, 地址 http://127.0.0.1:8787, any
//! key and model). It speaks the Claude Messages streaming format and always
//! plays the same parts. Asked for search queries by the guide (a request
//! without tools), it proposes two for PubMed. Asked to screen a paper, it
//! decides by one word; asked to translate, it hands the text back marked
//! as translated. In a chat, asked a question, it calls `list_papers` and
//! `library_status`; given their results, it answers and cites the first
//! paper the search returned, saying how many pictures came with the
//! conversation. A question containing 「试试撤销」 makes it call
//! `undo_import` instead, a tool the window asks the person about first.
//!
//! Requests are answered side by side, each after the given number of
//! milliseconds (none by default), so a run of several screening requests
//! can be watched. Every request is printed with its model and thinking
//! setting, which shows what each task was sent with.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// The request body, whichever way its length is announced.
fn body(stream: &mut TcpStream) -> String {
    let mut received = vec![];
    let mut buffer = [0u8; 8192];
    loop {
        let n = stream.read(&mut buffer).unwrap_or(0);
        received.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&received).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let head = text[..end].to_lowercase();
            let length: usize = head.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
            let done = if head.contains("transfer-encoding: chunked") { text.ends_with("0\r\n\r\n") } else { received.len() >= end + 4 + length };
            if done { return text[end + 4..].to_string() }
        }
        if n == 0 { return text }
    }
}

/// A text as a JSON string.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn event(value: &str) -> String {
    format!("data: {value}\n\n")
}

/// The label of the first paper in the tool results, as the app wrote it: `\"label\":\"P1\"`.
fn first_label(body: &str) -> String {
    let marker = "\\\"label\\\":\\\"";
    body.find(marker).map(|at| body[at + marker.len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect()).unwrap_or_else(|| "P1".into())
}

/// The string, object or number after `"name":` in the request, as it was sent.
fn sent(request: &str, name: &str) -> String {
    let Some(at) = request.find(&format!("\"{name}\":")) else { return "-".into() };
    let rest = &request[at + name.len() + 3..];
    let end = if rest.starts_with('{') { rest.find('}').map_or(rest.len(), |end| end + 1) } else { rest.find([',', '}']).unwrap_or(rest.len()) };
    rest[..end].to_string()
}

fn main() {
    let port = std::env::args().nth(1).unwrap_or_else(|| "8787".into());
    let delay: u64 = std::env::args().nth(2).and_then(|ms| ms.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    println!("mock model on http://127.0.0.1:{port}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        std::thread::spawn(move || answer(stream, delay));
    }
}

fn answer(mut stream: TcpStream, delay: u64) {
    {
        let request = body(&mut stream);
        let kind = if request.contains("文献筛选助手") { "screen" } else if request.contains("写作检索词助手") { "search words" } else if request.contains("翻译助手") { "translate" } else if request.contains("\"tools\"") { "chat" } else { "queries" };
        println!("{kind}: model {} thinking {} output_config {}", sent(&request, "model"), sent(&request, "thinking"), sent(&request, "output_config"));
        std::thread::sleep(std::time::Duration::from_millis(delay));
        // The last message decides the step: tool results get the answer, a question gets the calls.
        let answered = request.rfind("\"tool_result\"").is_some_and(|at| request.rfind("\"role\":\"user\"").is_some_and(|user| user < at));
        let mut events = String::new();
        if request.contains("文献筛选助手") {
            // A screening request: the verdict turns on one word, and the excerpt is the start of the abstract.
            let quote: String = request.split("摘要：").nth(1).map(|rest| rest.chars().take_while(|c| *c != '\\' && *c != '"').take(90).collect()).unwrap_or_default();
            let (verdict, reason, confidence) = if request.contains("rabidopsis") { ("include", "（假模型）文中提到拟南芥，按主题收录。", 0.9) } else if quote.is_empty() { ("uncertain", "（假模型）没有摘要可读。", 0.3) } else { ("irrelevant", "（假模型）文中没有提到拟南芥。", 0.7) };
            let reply = format!(r#"{{"verdict": "{verdict}", "reason": "{reason}", "excerpt": "{}", "confidence": {confidence}}}"#, quote.trim());
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(&format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#, quoted(&reply))));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#));
        } else if request.contains("写作检索词助手") {
            // 写作 asking for groups of search words: three, whatever was written.
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(&format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#, quoted("内容：root soil compaction cell\n论述：remains unclear little is known poorly understood\n说法：mechanical impedance hard soil").replace('\n', "\\n"))));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#));
        } else if request.contains("翻译助手") {
            // A translation request: the start of the text comes back, marked as translated.
            let source: String = request.rsplit("\"content\":\"").next().map(|rest| rest.chars().take_while(|c| *c != '\\' && *c != '"').take(60).collect()).unwrap_or_default();
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(&format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#, quoted(&format!("（假模型的译文）{}", source.trim())))));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#));
        } else if !request.contains("\"tools\"") {
            // A request without tools is the guide asking for search queries: two for PubMed, as JSON in the text.
            let reply = r#"[{"source": "pubmed", "query": "(\"single cell\"[Title/Abstract] OR \"single-cell\"[Title/Abstract]) AND root[Title/Abstract] AND Arabidopsis[Title/Abstract]", "why": "宽：单细胞加根加拟南芥，可能带进只是顺带提到根的文章"}, {"source": "pubmed", "query": "\"single cell\"[Title] AND root[Title] AND (atlas OR \"stem cell niche\")", "why": "聚焦：题目里同时有单细胞和根，并限定图谱或干细胞微环境"}]"#;
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(&format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{}}}}}"#, quoted(reply))));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#));
        } else if answered {
            let label = first_label(&request);
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            let pictures = request.matches("\"type\":\"image\"").count();
            let seen = if pictures > 0 { format!("收到 {pictures} 张图片。") } else { String::new() };
            for piece in ["（假模型）我查了文献库的概况和匹配的文献。", &seen, &format!("最相关的一篇是 [{label}]。"), "这段话只用来演示工具调用的显示和引用的跳转。"] {
                events.push_str(&event(&format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{piece}"}}}}"#)));
            }
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#));
        } else if request.rfind("试试撤销").is_some_and(|at| request.rfind("\"role\":\"user\"").is_some_and(|user| user < at)) {
            // A tool that takes papers out of the library: the window asks the person before it runs.
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我来撤销那次导入。"}}"#));
            events.push_str(&event(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_3","name":"undo_import","input":{}}}"#));
            events.push_str(&event(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"id\": \"B-demo\"}"}}"#));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#));
        } else {
            events.push_str(&event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#));
            events.push_str(&event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我先看看文献库里有什么。"}}"#));
            events.push_str(&event(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"library_status","input":{}}}"#));
            events.push_str(&event(r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_2","name":"list_papers","input":{}}}"#));
            events.push_str(&event(r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"limit\": 3, \"sort\": \"year-desc\"}"}}"#));
            events.push_str(&event(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#));
        }
        let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{events}").as_bytes());
    }
}
