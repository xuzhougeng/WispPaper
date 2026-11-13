//! 生图: asking a model service for a picture. A provider's model is
//! either one that answers in words or one that draws (`provider::Model::kind`,
//! `KIND`), and a drawing one is asked in one of three ways (`Interface`):
//! OpenAI's Images API (`/images/generations`, which 火山方舟's Seedream and
//! most relays speak as well), Gemini's own `generateContent`, or chat
//! completions that answer with a picture (how relays and OpenRouter hand
//! out Gemini's image models). `request` builds the request, `read` finds
//! the picture in the answer: its bytes, or an address to fetch them from.
//! Sending is `wisp_net::generate_image`.

use crate::ai::{HttpRequest, Profile};
use serde_json::{Value, json};

/// `provider::Model::kind` of a model that draws; a model that answers in words has none.
pub const KIND: &str = "image";

/// How a service is asked for a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interface {
    /// OpenAI's Images API: `POST {base}/images/generations`.
    Images,
    /// Gemini's own: `POST …/models/{model}:generateContent`.
    Gemini,
    /// Chat completions whose answer carries a picture.
    Chat,
}

/// What `provider::Model::interface` may hold, and what each is called. Empty is `guess`.
pub const INTERFACES: [(&str, &str); 4] = [("", "自动"), ("images", "Images 接口"), ("gemini", "Gemini 原生接口"), ("chat", "对话接口")];

impl Interface {
    pub fn from_key(key: &str) -> Option<Interface> {
        match key.trim() {
            "images" => Some(Interface::Images),
            "gemini" => Some(Interface::Gemini),
            "chat" => Some(Interface::Chat),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Interface::Images => INTERFACES[1].1,
            Interface::Gemini => INTERFACES[2].1,
            Interface::Chat => INTERFACES[3].1,
        }
    }
}

/// The interface a model is asked through when none is chosen: Gemini's own
/// at Google's address, chat completions for a Gemini model anywhere else
/// (a relay), the Images API for everything else.
pub fn guess(base_url: &str, model: &str) -> Interface {
    let (base, model) = (base_url.to_lowercase(), model.trim().to_lowercase());
    let model = model.strip_prefix("models/").unwrap_or(&model);
    if base.contains("generativelanguage.googleapis.com") && !model.starts_with("imagen") { return Interface::Gemini }
    if model.contains("gemini") { return Interface::Chat }
    Interface::Images
}

/// The interface a profile's model is asked through: the one chosen, else the guess.
pub fn interface(profile: &Profile) -> Interface {
    Interface::from_key(&profile.interface).unwrap_or_else(|| guess(&profile.base_url, &profile.model))
}

/// Whether a model's name says it draws: what a model just added is taken for.
pub fn looks_like(model: &str) -> bool {
    let name = model.trim().to_lowercase();
    ["image", "seedream", "dall-e", "imagen", "flux", "stable-diffusion", "sdxl", "cogview", "kolors", "wanx", "midjourney", "nano-banana", "jimeng"].iter().any(|word| name.contains(word))
}

/// The shape of the picture asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    #[default]
    Square,
    /// 3:2.
    Wide,
    /// 2:3.
    Tall,
}

pub const SHAPES: [(Shape, &str); 3] = [(Shape::Square, "方形"), (Shape::Wide, "横向"), (Shape::Tall, "竖向")];

impl Shape {
    pub fn ratio(self) -> &'static str {
        match self {
            Shape::Square => "1:1",
            Shape::Wide => "3:2",
            Shape::Tall => "2:3",
        }
    }

    fn pick(self, sizes: [&'static str; 3]) -> &'static str {
        sizes[match self { Shape::Square => 0, Shape::Wide => 1, Shape::Tall => 2 }]
    }
}

/// The longest description a picture is asked for with, in bytes.
pub const PROMPT_LIMIT: usize = 16_000;

/// The request for one picture of what `prompt` describes.
pub fn request(profile: &Profile, prompt: &str, shape: Shape) -> Result<HttpRequest, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() { return Err("先写下要画什么。".into()) }
    if prompt.len() > PROMPT_LIMIT { return Err("描述太长了，请缩短一些。".into()) }
    let model = profile.model.trim();
    if model.is_empty() { return Err("生图模型还没有名称。请在「设置 › AI › 模型」里填写。".into()) }
    let key = profile.key();
    if key.is_empty() { return Err("生图模型的服务还没有 API Key。请在「设置 › AI › 模型」里填写，或设置对应的环境变量。".into()) }
    let base = profile.base_url.trim().trim_end_matches('/');
    if !base.starts_with("https://") && !base.starts_with("http://") { return Err("模型服务地址应以 https:// 或 http:// 开头。".into()) }
    let bearer = vec![("authorization".to_string(), format!("Bearer {key}")), ("content-type".to_string(), "application/json".to_string())];
    let proxy = profile.proxy.trim().to_string();
    let lower = model.to_lowercase();
    Ok(match interface(profile) {
        Interface::Images => {
            let mut body = json!({"model": model, "prompt": prompt, "n": 1});
            if lower.contains("gpt-image") || lower.contains("chatgpt-image") {
                // These always answer with the picture itself, and refuse to be told so.
                body["size"] = json!(shape.pick(["1024x1024", "1536x1024", "1024x1536"]));
                body["output_format"] = json!("png");
            } else {
                // Without this most services answer with an address that soon expires.
                body["response_format"] = json!("b64_json");
                if lower.contains("seedream") {
                    body["size"] = json!(shape.pick(["2048x2048", "2496x1664", "1664x2496"]));
                    body["watermark"] = json!(false);
                } else if lower.contains("dall-e-3") {
                    body["size"] = json!(shape.pick(["1024x1024", "1792x1024", "1024x1792"]));
                } else if !lower.contains("imagen") {
                    body["size"] = json!(shape.pick(["1024x1024", "1536x1024", "1024x1536"]));
                }
            }
            HttpRequest { url: format!("{base}/images/generations"), headers: bearer, body, proxy }
        }
        Interface::Gemini => {
            let body = json!({
                "contents": [{"role": "user", "parts": [{"text": prompt}]}],
                "generationConfig": {"responseModalities": ["TEXT", "IMAGE"], "imageConfig": {"aspectRatio": shape.ratio()}},
            });
            let name = model.strip_prefix("models/").unwrap_or(model);
            HttpRequest { url: format!("{}/models/{name}:generateContent", gemini_root(base)), headers: vec![("x-goog-api-key".to_string(), key), ("content-type".to_string(), "application/json".to_string())], body, proxy }
        }
        Interface::Chat => {
            let mut body = json!({"model": model, "messages": [{"role": "user", "content": prompt}], "modalities": ["image", "text"], "stream": false});
            if shape != Shape::Square { body["image_config"] = json!({"aspect_ratio": shape.ratio()}) }
            HttpRequest { url: format!("{base}/chat/completions"), headers: bearer, body, proxy }
        }
    })
}

/// Where Gemini's own interface is, given the address chat goes to: Google's
/// OpenAI-compatible address ends in `/openai`, a relay's in `/v1`.
fn gemini_root(base: &str) -> String {
    let root = base.strip_suffix("/openai").unwrap_or(base);
    let root = root.strip_suffix("/v1").unwrap_or(root);
    if root.ends_with("/v1beta") || root.ends_with("/v1alpha") { root.to_string() } else { format!("{root}/v1beta") }
}

/// What an answer holds: the picture, or where it is to be fetched from.
#[derive(Clone, Debug, PartialEq)]
pub enum Made {
    Picture(Vec<u8>),
    Address(String),
}

/// A picture given as text: a `data:` URL, an address, or bare base64.
fn from_text(text: &str) -> Option<Made> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("data:") { return unbase64(rest.split_once("base64,")?.1).map(Made::Picture) }
    if text.starts_with("https://") || text.starts_with("http://") { return Some(Made::Address(text.to_string())) }
    None
}

/// The first picture written into running text: a `data:` URL or a Markdown image.
fn in_words(text: &str) -> Option<Made> {
    if let Some(at) = text.find("data:image/") {
        let url: String = text[at..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, ')' | '"' | '\'' | '>')).collect();
        if let Some(made) = from_text(&url) { return Some(made) }
    }
    let at = text.find("![")?;
    let rest = &text[at..];
    let address = rest[rest.find("](")? + 2..].split([')', ' ']).next()?;
    from_text(address).filter(|made| matches!(made, Made::Address(_)))
}

/// What a model said in place of a picture, shortened, for the error.
fn said(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { return "模型没有返回图片。".into() }
    let short: String = text.chars().take(200).collect();
    format!("模型没有返回图片，它说：{short}{}", if text.chars().count() > 200 { "…" } else { "" })
}

/// Find the picture in a service's answer.
pub fn read(interface: Interface, reply: &Value) -> Result<Made, String> {
    // Some services say what went wrong in a 200.
    if let Some(error) = reply["error"]["message"].as_str().or(reply["error"].as_str()) { return Err(format!("模型服务报错：{error}")) }
    match interface {
        Interface::Images => {
            let first = &reply["data"][0];
            if let Some(made) = first["b64_json"].as_str().and_then(unbase64).map(Made::Picture) { return Ok(made) }
            if let Some(made) = first["url"].as_str().and_then(from_text) { return Ok(made) }
            Err("模型没有返回图片。".into())
        }
        Interface::Gemini => {
            let parts = reply["candidates"][0]["content"]["parts"].as_array().cloned().unwrap_or_default();
            for part in &parts {
                let inline = if part["inlineData"].is_object() { &part["inlineData"] } else { &part["inline_data"] };
                if let Some(picture) = inline["data"].as_str().and_then(unbase64) { return Ok(Made::Picture(picture)) }
            }
            if let Some(reason) = reply["promptFeedback"]["blockReason"].as_str() { return Err(format!("这个描述被模型服务拒绝了（{reason}）。")) }
            let words: Vec<&str> = parts.iter().filter_map(|part| part["text"].as_str()).collect();
            Err(said(&words.join(" ")))
        }
        Interface::Chat => {
            let message = &reply["choices"][0]["message"];
            let picture = &message["images"][0];
            if let Some(made) = picture["image_url"]["url"].as_str().or(picture["url"].as_str()).or(picture["image_url"].as_str()).and_then(from_text) { return Ok(made) }
            if let Some(made) = picture["b64_json"].as_str().and_then(unbase64).map(Made::Picture) { return Ok(made) }
            let mut words = String::new();
            match &message["content"] {
                Value::String(text) => words.push_str(text),
                Value::Array(parts) => {
                    for part in parts {
                        if let Some(made) = part["image_url"]["url"].as_str().or(part["image_url"].as_str()).and_then(from_text) { return Ok(made) }
                        if let Some(text) = part["text"].as_str() { words.push_str(text) }
                    }
                }
                _ => {}
            }
            in_words(&words).ok_or_else(|| said(&words))
        }
    }
}

/// The bytes base64 text stands for, in either alphabet, with or without
/// padding or line breaks; `None` for what is not base64 or is empty.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut held, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        held = held << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((held >> bits) as u8);
            held &= (1 << bits) - 1;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{Protocol, base64};

    fn profile(base: &str, model: &str, interface: &str) -> Profile {
        Profile { protocol: Protocol::OpenAI, base_url: base.into(), api_key: "k".into(), model: model.into(), interface: interface.into(), ..Default::default() }
    }

    #[test]
    fn base64_both_ways() {
        for bytes in [&b"f"[..], b"fo", b"foo", b"foob", &[0xff, 0xfe, 0x00, 0x7f, 0x80]] {
            assert_eq!(unbase64(&base64(bytes)).as_deref(), Some(bytes));
        }
        // Line breaks, no padding, the URL alphabet; not what is no base64 at all.
        assert_eq!(unbase64("Zm9v\nYg").as_deref(), Some(&b"foob"[..]));
        assert_eq!(unbase64("__79").as_deref(), Some(&[0xff, 0xfe, 0xfd][..]));
        assert_eq!((unbase64(""), unbase64("not base64!")), (None, None));
    }

    #[test]
    fn which_interface() {
        assert_eq!(guess("https://generativelanguage.googleapis.com/v1beta/openai", "gemini-3-pro-image-preview"), Interface::Gemini);
        assert_eq!(guess("https://generativelanguage.googleapis.com/v1beta/openai", "imagen-4.0-generate-001"), Interface::Images);
        assert_eq!(guess("https://relay.example.com/v1", "gemini-3.1-flash-image-preview"), Interface::Chat);
        assert_eq!(guess("https://api.openai.com/v1", "gpt-image-2"), Interface::Images);
        assert_eq!(guess("https://ark.cn-beijing.volces.com/api/v3", "doubao-seedream-4-0-250828"), Interface::Images);
        // What the person chose stands, whatever the names say.
        assert_eq!(interface(&profile("https://relay.example.com/v1", "gemini-3-pro-image-preview", "gemini")), Interface::Gemini);
        assert_eq!(interface(&profile("https://api.openai.com/v1", "gpt-image-2", "nonsense")), Interface::Images);
        assert!(["gpt-image-2", "doubao-seedream-4-0-250828", "gemini-3-pro-image-preview", "FLUX.1-dev", "dall-e-3"].iter().all(|name| looks_like(name)));
        assert!(!["gpt-6.1", "deepseek-v4-pro", "gemini-3-pro", "claude-opus-5-5"].iter().any(|name| looks_like(name)));
    }

    #[test]
    fn requests() {
        // OpenAI's own: the picture comes as it is, a PNG, and `response_format` is not sent.
        let openai = request(&profile("https://api.openai.com/v1/", "gpt-image-2", ""), " a root tip ", Shape::Wide).unwrap();
        assert_eq!(openai.url, "https://api.openai.com/v1/images/generations");
        assert_eq!(openai.body, json!({"model": "gpt-image-2", "prompt": "a root tip", "n": 1, "size": "1536x1024", "output_format": "png"}));
        assert_eq!(openai.headers[0], ("authorization".to_string(), "Bearer k".to_string()));
        // Seedream: its own sizes, no watermark, the picture in the answer.
        let ark = request(&profile("https://ark.cn-beijing.volces.com/api/v3", "doubao-seedream-4-0-250828", ""), "根尖", Shape::Tall).unwrap();
        assert_eq!(ark.url, "https://ark.cn-beijing.volces.com/api/v3/images/generations");
        assert_eq!(ark.body, json!({"model": "doubao-seedream-4-0-250828", "prompt": "根尖", "n": 1, "response_format": "b64_json", "size": "1664x2496", "watermark": false}));
        // Gemini's own, from the address chat goes to, with its own key header.
        let gemini = request(&profile("https://generativelanguage.googleapis.com/v1beta/openai/", "models/gemini-3-pro-image-preview", ""), "根尖", Shape::Square).unwrap();
        assert_eq!(gemini.url, "https://generativelanguage.googleapis.com/v1beta/models/gemini-3-pro-image-preview:generateContent");
        assert_eq!(gemini.headers[0], ("x-goog-api-key".to_string(), "k".to_string()));
        assert_eq!(gemini.body["generationConfig"], json!({"responseModalities": ["TEXT", "IMAGE"], "imageConfig": {"aspectRatio": "1:1"}}));
        assert_eq!(gemini.body["contents"][0]["parts"][0]["text"], "根尖");
        // Through a relay: chat completions by default, Gemini's own under /v1beta when chosen.
        let relay = request(&profile("https://relay.example.com/v1", "gemini-3.1-flash-image-preview", ""), "根尖", Shape::Wide).unwrap();
        assert_eq!(relay.url, "https://relay.example.com/v1/chat/completions");
        assert_eq!((relay.body["modalities"].clone(), relay.body["image_config"]["aspect_ratio"].as_str(), relay.body["stream"].as_bool()), (json!(["image", "text"]), Some("3:2"), Some(false)));
        let native = request(&profile("https://relay.example.com/v1", "gemini-3.1-flash-image-preview", "gemini"), "根尖", Shape::Wide).unwrap();
        assert_eq!(native.url, "https://relay.example.com/v1beta/models/gemini-3.1-flash-image-preview:generateContent");
        // What cannot be asked says why.
        assert!(request(&profile("https://api.openai.com/v1", "gpt-image-2", ""), "  ", Shape::Square).unwrap_err().contains("画什么"));
        assert!(request(&profile("https://api.openai.com/v1", " ", ""), "x", Shape::Square).unwrap_err().contains("名称"));
        assert!(request(&profile("api.openai.com/v1", "gpt-image-2", ""), "x", Shape::Square).unwrap_err().contains("https://"));
        let keyless = Profile { api_key: String::new(), key_env: "WISP_TEST_NO_SUCH_KEY".into(), ..profile("https://api.openai.com/v1", "gpt-image-2", "") };
        assert!(request(&keyless, "x", Shape::Square).unwrap_err().contains("API Key"));
    }

    #[test]
    fn answers() {
        let png = base64(b"\x89PNG\r\n\x1a\n...");
        let picture = Made::Picture(b"\x89PNG\r\n\x1a\n...".to_vec());
        // The Images API: the picture itself, or where it is.
        assert_eq!(read(Interface::Images, &json!({"data": [{"b64_json": png}]})), Ok(picture.clone()));
        assert_eq!(read(Interface::Images, &json!({"data": [{"url": "https://cdn.example.com/a.png?sig=1"}]})), Ok(Made::Address("https://cdn.example.com/a.png?sig=1".into())));
        assert!(read(Interface::Images, &json!({"data": []})).is_err());
        assert_eq!(read(Interface::Images, &json!({"error": {"message": "quota"}})), Err("模型服务报错：quota".into()));
        // Gemini: among the parts, in either spelling; words alone are said back.
        assert_eq!(read(Interface::Gemini, &json!({"candidates": [{"content": {"parts": [{"text": "Here it is"}, {"inlineData": {"mimeType": "image/png", "data": png}}]}}]})), Ok(picture.clone()));
        assert_eq!(read(Interface::Gemini, &json!({"candidates": [{"content": {"parts": [{"inline_data": {"mime_type": "image/png", "data": png}}]}}]})), Ok(picture.clone()));
        assert_eq!(read(Interface::Gemini, &json!({"candidates": [{"content": {"parts": [{"text": "I cannot  draw\nthat."}]}}]})), Err("模型没有返回图片，它说：I cannot draw that.".into()));
        assert!(read(Interface::Gemini, &json!({"promptFeedback": {"blockReason": "SAFETY"}})).unwrap_err().contains("SAFETY"));
        // Chat completions: beside the message, among its parts, or written into its words.
        let url = format!("data:image/png;base64,{png}");
        assert_eq!(read(Interface::Chat, &json!({"choices": [{"message": {"content": "", "images": [{"type": "image_url", "image_url": {"url": url}}]}}]})), Ok(picture.clone()));
        assert_eq!(read(Interface::Chat, &json!({"choices": [{"message": {"content": [{"type": "text", "text": "ok"}, {"type": "image_url", "image_url": {"url": url}}]}}]})), Ok(picture.clone()));
        assert_eq!(read(Interface::Chat, &json!({"choices": [{"message": {"content": format!("好的：![image]({url})")}}]})), Ok(picture));
        assert_eq!(read(Interface::Chat, &json!({"choices": [{"message": {"content": "![图](https://cdn.example.com/b.png) 画好了"}}]})), Ok(Made::Address("https://cdn.example.com/b.png".into())));
        assert_eq!(read(Interface::Chat, &json!({"choices": [{"message": {"content": "我不能画这个。"}}]})), Err("模型没有返回图片，它说：我不能画这个。".into()));
    }
}
