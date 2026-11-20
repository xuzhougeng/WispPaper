//! A card the assistant puts under its answer (the tool `offer_action`): a sentence and a few buttons, each one of a
//! fixed set of things the window knows how to do. Nothing runs when the card is offered; a button runs only when
//! the person presses it. Outside the window (the command line, MCP) there is no card: the result says so.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

/// The kinds of button, with what each does, as the tool's schema tells the model.
pub const KINDS: [(&str, &str); 4] = [
    ("reply", "替用户说一句话：按下后把 text 作为用户的下一句发出，用来给出几个可选的下一步"),
    ("promote_library", "建成文献库：打开把这次对话建成文献库的对话框（只在还没有建文献库的对话里）"),
    ("find_pdfs", "找全文：为还没有全文的文献找开放获取的 PDF（只在文献库里）"),
    ("open_page", "打开文献库的一个页面：page 是 screen（筛选）、write（写作）、figures（图片库）、extract（提取）、sources（检索与导入）或 papers（文献列表）（只在文献库里）"),
];

/// The pages `open_page` may name.
pub const PAGES: [&str; 6] = ["screen", "write", "figures", "extract", "sources", "papers"];

/// The most buttons one card holds.
pub const MOST: usize = 3;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offer {
    /// The sentence on the card.
    pub text: String,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Action {
    /// One of `KINDS`.
    pub kind: String,
    /// The button's words; empty for the kind's own (the window's label).
    pub label: String,
    /// For `reply`, what is said; for `open_page`, the page.
    pub arg: String,
}

/// The arguments' schema, for the tool's entry in the catalog.
pub fn properties() -> Value {
    let kinds: Vec<&str> = KINDS.iter().map(|(kind, _)| *kind).collect();
    let described: Vec<String> = KINDS.iter().map(|(kind, what)| format!("{kind}：{what}")).collect();
    json!({
        "text": {"type": "string", "description": "卡片上的一句话：说明为什么请用户决定，例如「这次对话收进了 126 篇文献，建成文献库后会出现在首页。」"},
        "actions": {
            "type": "array",
            "description": format!("1 到 {MOST} 个按钮，最推荐的放第一个。kind 只能是：{}。", described.join("；")),
            "items": {"type": "object", "properties": {
                "kind": {"type": "string", "enum": kinds},
                "label": {"type": "string", "description": "按钮上的字，四五个字；省略时用这种按钮的默认名称。"},
                "text": {"type": "string", "description": "reply 时按下后发出的那句话，用用户的口吻。"},
                "page": {"type": "string", "enum": PAGES, "description": "open_page 时要打开的页面。"}
            }, "required": ["kind"]}
        }
    })
}

/// The tool's arguments as an offer, or why they are not one.
pub fn parse(args: &Value) -> Result<Offer> {
    let text = args["text"].as_str().unwrap_or_default().trim().to_string();
    if text.is_empty() { bail!("需要 text：卡片上的一句话。") }
    let given = args["actions"].as_array().ok_or_else(|| anyhow!("需要 actions：1 到 {MOST} 个按钮。"))?;
    if given.is_empty() || given.len() > MOST { bail!("actions 需要 1 到 {MOST} 个按钮，现在是 {} 个。", given.len()) }
    let mut actions = vec![];
    for action in given {
        let kind = action["kind"].as_str().unwrap_or_default().trim();
        if !KINDS.iter().any(|(known, _)| *known == kind) { bail!("没有这种按钮：「{kind}」。kind 只能是 {}。", KINDS.iter().map(|(kind, _)| *kind).collect::<Vec<_>>().join("、")) }
        let label = action["label"].as_str().unwrap_or_default().trim().to_string();
        let arg = match kind {
            "reply" => {
                let said = action["text"].as_str().unwrap_or_default().trim();
                if said.is_empty() { bail!("reply 按钮需要 text：按下后发出的那句话。") }
                said.to_string()
            }
            "open_page" => {
                let page = action["page"].as_str().unwrap_or_default().trim();
                if !PAGES.contains(&page) { bail!("open_page 按钮需要 page，只能是 {}。", PAGES.join("、")) }
                page.to_string()
            }
            _ => String::new(),
        };
        // A reply without words of its own is labelled by what it says.
        let label = if label.is_empty() && kind == "reply" { arg.clone() } else { label };
        actions.push(Action { kind: kind.to_string(), label, arg });
    }
    Ok(Offer { text, actions })
}

/// What the tool answers: the offer is checked, and nothing was done.
pub fn offered(args: &Value) -> Result<Value> {
    let offer = parse(args)?;
    Ok(json!({
        "offered": offer.actions.len(),
        "next": "在 Wisp Study 的窗口里，这张卡片显示在你这条回答的下面，用户按下按钮才会执行，现在什么都没有做。不要说已经做了，也不要在回答里把这些选项再问一遍；回答简短收尾即可。通过命令行或 MCP 调用时没有卡片：请用文字告诉用户这些选项。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_holds_only_buttons_the_window_knows() {
        let offer = parse(&json!({"text": " 收进了 3 篇。 ", "actions": [{"kind": "promote_library"}, {"kind": "reply", "text": "再找几篇综述"}, {"kind": "open_page", "page": "screen", "label": "去筛选"}]})).unwrap();
        assert_eq!(offer.text, "收进了 3 篇。");
        assert_eq!(offer.actions.iter().map(|a| (a.kind.as_str(), a.label.as_str(), a.arg.as_str())).collect::<Vec<_>>(), [("promote_library", "", ""), ("reply", "再找几篇综述", "再找几篇综述"), ("open_page", "去筛选", "screen")]);
        assert!(parse(&json!({"text": "x", "actions": [{"kind": "delete_library"}]})).unwrap_err().to_string().contains("没有这种按钮"));
        assert!(parse(&json!({"text": "x", "actions": [{"kind": "reply"}]})).unwrap_err().to_string().contains("需要 text"));
        assert!(parse(&json!({"text": "x", "actions": [{"kind": "open_page", "page": "settings"}]})).is_err());
        assert!(parse(&json!({"text": "x", "actions": []})).is_err());
        assert!(parse(&json!({"text": "x", "actions": [{"kind": "find_pdfs"}, {"kind": "find_pdfs"}, {"kind": "find_pdfs"}, {"kind": "find_pdfs"}]})).is_err());
        assert!(parse(&json!({"actions": [{"kind": "find_pdfs"}]})).unwrap_err().to_string().contains("需要 text"));
        assert_eq!(offered(&json!({"text": "x", "actions": [{"kind": "find_pdfs"}]})).unwrap()["offered"], 1);
    }
}
