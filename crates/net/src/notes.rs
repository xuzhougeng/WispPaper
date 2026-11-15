//! Notion publishing through the shared MCP client and OAuth session.
//! No other Notion client, settings file or database is involved.

use serde_json::{Value, json};
use wisp_core::{connector::{Connector, NOTION_MCP}, libraries::LibraryInfo, notes::{self, Document}, settings::{Settings, settings_file}, store::Store};

fn data(result: Value) -> Result<Value, String> {
    wisp_core::connector::read_result(&result)?;
    if result["structuredContent"].is_object() { return Ok(result["structuredContent"].clone()) }
    result["content"].as_array().into_iter().flatten().filter_map(|part| part["text"].as_str()).find_map(|text| serde_json::from_str(text).ok()).ok_or_else(|| "Notion 没有返回可识别的 JSON 结果，请在 Notion 核对；不会自动重试。".into())
}

fn call(connector: &Connector, tool: &str, args: Value) -> Result<Value, String> {
    data(crate::mcp::call_raw(connector, tool, &args)?)
}

pub fn publish(store: &mut Store, info: &LibraryInfo, args: &Value) -> Result<Value, String> {
    let doc = notes::from_args(store, info, args).map_err(|e| format!("{e:#}"))?;
    let parent = notes::page_id(args["parent"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
    save(store, &doc, &parent)
}

pub fn save(store: &mut Store, doc: &Document, parent: &str) -> Result<Value, String> {
    let _guard = store.lock_notes().map_err(|e| e.to_string())?;
    doc.validate().map_err(|e| e.to_string())?;
    let parent = notes::page_id(parent).map_err(|e| e.to_string())?;
    let settings = Settings::load(&settings_file()).map_err(|e| format!("{e:#}"))?;
    let connector = settings.connectors.iter().find(|c| c.target.trim().trim_end_matches('/') == NOTION_MCP && !c.off).ok_or("请先在「设置 › 自定义 › MCP」启用 Notion 并登录。")?;
    let listed = crate::mcp::tools(connector).map_err(|e| e.said(connector))?;
    for required in ["notion-fetch", "notion-create-pages", "notion-update-page"] {
        if !listed.iter().any(|t| t.tool == required) { return Err(format!("当前 Notion MCP 不提供 {required}。")) }
    }
    let images: Vec<_> = doc.used_attachments().collect();
    if !images.is_empty() && !listed.iter().any(|t| t.tool == "notion-create-file-upload") { return Err("当前 Notion MCP 不支持图片上传，请先导出到本地。".into()) }
    if images.iter().any(|a| a.bytes.len() > 20 * 1024 * 1024) { return Err("Notion 单张图片不能超过 20 MiB，请分开处理。".into()) }
    // Verify access before reserving or writing. The server's page body stays opaque.
    call(connector, "notion-fetch", json!({"id": parent}))?;
    let existing = store.note_saves(&doc.subject).map_err(|e| e.to_string())?;
    if let Some(saved) = existing.iter().find(|s| s.parent == parent && s.status == "saved") {
        call(connector, "notion-fetch", json!({"id": saved.page}))?;
    }
    let saved = store.reserve_note(doc, &parent).map_err(|e| e.to_string())?;
    if saved.status == "saved" { return Ok(json!({"url": saved.url(), "page": saved.page, "duplicate": true})) }
    let result = write(connector, doc, &parent, &saved.page);
    match result {
        Ok(page) => {
            store.finish_note(&saved.id, Some(&page)).map_err(|e| format!("Notion 已写入 https://www.notion.so/{page}，本地记录更新失败：{e}。请核对待确认记录，不要重试。"))?;
            Ok(json!({"url": format!("https://www.notion.so/{page}"), "page": page, "duplicate": false, "appended": !saved.page.is_empty()}))
        }
        Err(error) => Err(format!("{error}\n保存记录 {} 待核对。请打开 {} 确认结果；不要重复保存。", saved.id, saved.url())),
    }
}

fn write(connector: &Connector, doc: &Document, parent: &str, page: &str) -> Result<String, String> {
    let mut markdown = doc.markdown.clone();
    for attachment in doc.used_attachments() {
        let prepared = call(connector, "notion-create-file-upload", json!({"filename": attachment.name, "content_type": attachment.media_type}))?;
        let source = upload(&prepared, attachment)?;
        markdown = markdown.replace(&format!("({})", attachment.name), &format!("({source})"));
    }
    let result = if page.is_empty() {
        call(connector, "notion-create-pages", json!({"parent": {"page_id": parent}, "pages": [{"properties": {"title": doc.title}, "content": markdown}], "allow_async": false}))?
    } else {
        call(connector, "notion-update-page", json!({"page_id": page, "command": "insert_content", "position": {"type": "end"}, "content": format!("\n\n---\n\n{markdown}"), "allow_async": false}))?
    };
    // A queued task or warning is not a confirmed write. Do not claim success or replay.
    if result.get("async_task").is_some() || result["type"] == "async_task" || result.get("warnings").is_some_and(|w| !w.is_null() && w.as_array().is_none_or(|a| !a.is_empty())) {
        return Err(format!("Notion 返回了待处理任务或警告，请核对页面：{result}"));
    }
    let id = if page.is_empty() {
        result["pages"][0]["id"].as_str().or_else(|| result["pages"][0]["url"].as_str())
    } else {
        result["page_id"].as_str().or_else(|| result["id"].as_str()).or_else(|| result["url"].as_str())
    }.ok_or_else(|| format!("Notion 写入返回的页面信息无法识别，请核对页面：{result}"))?;
    let id = notes::page_id(id).map_err(|e| e.to_string())?;
    if !page.is_empty() && id != page { return Err("Notion 返回的页面与目标页面不一致，请核对结果。".into()) }
    Ok(id)
}

fn upload(prepared: &Value, attachment: &notes::Attachment) -> Result<String, String> {
    let url = prepared["upload_url"].as_str().ok_or("Notion 没有提供图片上传地址。")?;
    let uri: ureq::http::Uri = url.parse().map_err(|_| "图片上传地址无效。")?;
    if uri.scheme_str() != Some("https") || !matches!(uri.host(), Some("mcp.notion.com" | "api.notion.com")) || uri.port_u16().is_some_and(|p| p != 443) {
        return Err("Notion 返回了不受支持的图片上传地址，已停止上传。".into());
    }
    let boundary = format!("wisp-{}", wisp_core::model::new_id("upload"));
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", attachment.name, attachment.media_type).into_bytes();
    body.extend_from_slice(&attachment.bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(std::time::Duration::from_secs(120))).proxy(crate::proxy_for("")?).build().into();
    let mut request = agent.post(url).header("content-type", format!("multipart/form-data; boundary={boundary}"));
    if let Some(headers) = prepared["upload_headers"].as_object() {
        for (key, value) in headers {
            let value = value.as_str().ok_or("Notion 上传请求头无效。")?;
            request = request.header(key, value);
        }
    }
    let mut response = request.send(body).map_err(crate::network_error)?;
    if !response.status().is_success() { return Err(format!("Notion 图片上传失败（HTTP {}）；没有重试。", response.status())) }
    let result: Value = response.body_mut().read_json().map_err(crate::network_error)?;
    result["markdown_source"].as_str().filter(|s| !s.contains(['\r', '\n', '"', ')'])).map(str::to_string).ok_or_else(|| "Notion 图片上传结果没有 markdown_source。".into())
}
