//! 连接器: Model Context Protocol servers of the person's own, whose tools
//! the assistant may call beside the library's. This is the side of it that
//! needs no socket and no process: what a connector is, the JSON-RPC
//! messages, and how a reply is read. The transports are in `wisp_net::mcp`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The protocol revision asked for; a server may answer with an older one.
pub const VERSION: &str = "2025-06-18";

/// One MCP server (设置 › 自定义 › 连接器).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connector {
    pub name: String,
    /// An address (`https://…`, Streamable HTTP) or a command line that
    /// speaks MCP on its standard streams (`npx -y some-mcp-server`).
    pub target: String,
    /// Sent as `Authorization: Bearer …` to an address; empty sends none.
    pub token: String,
    /// Switched off: kept, and not offered to the assistant.
    pub off: bool,
}

impl Connector {
    /// Whether the target is an address and not a command.
    pub fn remote(&self) -> bool {
        let target = self.target.trim();
        target.starts_with("https://") || target.starts_with("http://")
    }

    /// The connector's part of its tools' names: what of its name a tool name may hold.
    pub fn slug(&self) -> String {
        let slug: String = self.name.chars().filter(char::is_ascii_alphanumeric).take(16).collect::<String>().to_lowercase();
        if slug.is_empty() { "connector".into() } else { slug }
    }
}

/// A tool from outside the library's catalog, as a model is told of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// What the model calls it: the connector's slug, two underscores, the server's name for it.
    pub name: String,
    /// The server's own name for it.
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
    /// The server says it changes nothing; any other tool is asked about before it runs.
    pub read_only: bool,
}

pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

pub fn initialize(id: u64) -> Value {
    request(id, "initialize", json!({"protocolVersion": VERSION, "capabilities": {}, "clientInfo": {"name": "wisp-paper", "title": "Wisp Paper", "version": env!("CARGO_PKG_VERSION")}}))
}

pub fn initialized() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

/// What one message from a server means to whoever waits for the reply to `id`.
pub enum Heard {
    /// The reply: its result, or the error as a sentence.
    Reply(Result<Value, String>),
    /// The server asks something of its own; this is the answer to send back.
    Answer(Value),
    /// A notification, another reply, or nothing that can be read.
    Nothing,
}

/// Read one message: a line of a server's standard output, the `data:` of an event, or a JSON body.
pub fn hear(text: &str, id: u64) -> Heard {
    let Ok(message) = serde_json::from_str::<Value>(text.trim()) else { return Heard::Nothing };
    if let Some(method) = message["method"].as_str() {
        // A request has an id and wants an answer; this client offers nothing but a ping.
        if message.get("id").is_none_or(Value::is_null) { return Heard::Nothing }
        let answer = if method == "ping" { json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}) } else { json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": format!("method not found: {method}")}}) };
        return Heard::Answer(answer);
    }
    if message["id"].as_u64() != Some(id) { return Heard::Nothing }
    Heard::Reply(match message.get("error").filter(|error| !error.is_null()) {
        Some(error) => Err(error["message"].as_str().map_or_else(|| error.to_string(), str::to_string)),
        None => Ok(message["result"].clone()),
    })
}

/// The tools of a `tools/list` result, named for the model.
pub fn read_tools(connector: &Connector, result: &Value) -> Vec<Tool> {
    let slug = connector.slug();
    let tools = result["tools"].as_array().into_iter().flatten();
    tools.filter_map(|tool| {
        let own = tool["name"].as_str()?;
        // The model services take letters, digits, `_` and `-`, at most 64 of them.
        let name: String = format!("{slug}__{own}").chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect();
        let schema = if tool["inputSchema"].is_object() { tool["inputSchema"].clone() } else { json!({"type": "object", "properties": {}}) };
        Some(Tool { name, tool: own.to_string(), description: format!("[{}] {}", connector.name.trim(), tool["description"].as_str().unwrap_or("")), input_schema: schema, read_only: tool["annotations"]["readOnlyHint"].as_bool() == Some(true) })
    }).collect()
}

/// A `tools/call` result as the text a model reads; `Err` when the tool says it failed.
pub fn read_result(result: &Value) -> Result<String, String> {
    let parts: Vec<String> = result["content"].as_array().into_iter().flatten().map(|part| match part["type"].as_str() {
        Some("text") => part["text"].as_str().unwrap_or("").to_string(),
        Some("resource") => part["resource"]["text"].as_str().unwrap_or("[resource]").to_string(),
        Some(kind) => format!("[{kind}]"),
        None => String::new(),
    }).collect();
    let mut text = parts.join("\n");
    if text.trim().is_empty() && !result["structuredContent"].is_null() { text = result["structuredContent"].to_string() }
    if text.len() > crate::tools::MAX_OUTPUT_BYTES {
        let cut = text.floor_char_boundary(crate::tools::MAX_OUTPUT_BYTES);
        text = format!("{}\n…[结果过长，只显示了 {} 字节中的前 {cut} 字节；请缩小范围后重试]", &text[..cut], text.len());
    }
    if result["isError"].as_bool() == Some(true) { Err(if text.is_empty() { "工具报告了失败，没有说明原因。".into() } else { text }) } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connector_is_an_address_or_a_command() {
        let notion = Connector { name: "My Notion 笔记".into(), target: " https://mcp.example/mcp".into(), ..Default::default() };
        assert!(notion.remote() && !Connector { target: "npx -y server".into(), ..Default::default() }.remote());
        assert_eq!((notion.slug().as_str(), Connector { name: "笔记".into(), ..Default::default() }.slug().as_str()), ("mynotion", "connector"));
    }

    #[test]
    fn replies_are_told_from_what_else_a_server_says() {
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#, 3), Heard::Reply(Ok(result)) if result["ok"] == true));
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"no"}}"#, 3), Heard::Reply(Err(why)) if why == "no"));
        // Another call's reply, a notification and noise are passed over; a request is answered.
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 3), Heard::Nothing));
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#, 3), Heard::Nothing));
        assert!(matches!(hear("starting…", 3), Heard::Nothing));
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#, 3), Heard::Answer(answer) if answer["id"] == "a" && answer["result"].is_object()));
        assert!(matches!(hear(r#"{"jsonrpc":"2.0","id":9,"method":"roots/list"}"#, 3), Heard::Answer(answer) if answer["error"]["code"] == -32601));
    }

    #[test]
    fn tools_and_results_are_read() {
        let connector = Connector { name: "Notion".into(), ..Default::default() };
        let tools = read_tools(&connector, &json!({"tools": [
            {"name": "search", "description": "Find pages", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}}, "annotations": {"readOnlyHint": true}},
            {"name": "pages.create", "description": "Make a page"},
            {"description": "nameless"},
        ]}));
        assert_eq!(tools.iter().map(|t| (t.name.as_str(), t.tool.as_str(), t.read_only)).collect::<Vec<_>>(), [("notion__search", "search", true), ("notion__pages_create", "pages.create", false)]);
        assert_eq!((tools[0].description.as_str(), tools[1].input_schema["type"].as_str()), ("[Notion] Find pages", Some("object")));
        assert_eq!(read_result(&json!({"content": [{"type": "text", "text": "a"}, {"type": "image", "data": "…"}, {"type": "text", "text": "b"}]})), Ok("a\n[image]\nb".into()));
        assert_eq!(read_result(&json!({"content": [{"type": "text", "text": "not found"}], "isError": true})), Err("not found".into()));
        assert_eq!(read_result(&json!({"content": [], "structuredContent": {"n": 1}})), Ok(r#"{"n":1}"#.into()));
    }
}
