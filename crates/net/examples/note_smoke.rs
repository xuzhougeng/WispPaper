//! Explicit live smoke test. Requires an isolated WISP_STUDY_HOME and a parent
//! page supplied by the person. Creates one test page and leaves it for inspection.
use serde_json::json;
use wisp_core::{ai::{Said, Transcript}, libraries, model::Paper, notes, store::{Conversation, Store}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::var("WISP_STUDY_HOME")?);
    let parent = std::env::args().nth(1).ok_or("Supply a parent page explicitly")?;
    wisp_net::set_proxy(&std::env::var("NOTION_TEST_PROXY").unwrap_or_default());
    let info = match libraries::list(&root).into_iter().find(|l| l.name == "笔记导出测试") {
        Some(info) => info,
        None => libraries::create(&root, "笔记导出测试", "虚构内容，仅用于功能验证", "blue")?,
    };
    let mut store = Store::open(&info.dir)?;
    let mut paper = Paper { id: wisp_core::model::new_id("P"), title: "WispStudy 笔记保存测试（虚构文献）".into(), journal: "测试期刊".into(), year: Some(2024), notes: "这是一条虚构测试笔记。\n\n- 保留原始笔记\n- 验证中文与 **Markdown**\n\n[参考链接](https://example.com)".into(), ..Default::default() };
    if let Some(existing) = store.list_papers()?.into_iter().find(|p| p.title == paper.title && !p.id.is_empty()) { paper = existing }
    else { store.save_paper(&mut paper)?; }
    let mut conversation = Conversation { paper_id: paper.id.clone(), transcript: Transcript { items: vec![
        Said::User { text: "测试图片和原文摘录\n\n> 这是虚构的原文摘录。".into(), images: vec!["I-test".into()], files: vec![], prompt: String::new() },
        Said::Answer { text: "# 测试回答\n\n此内容只用于验证排版，不是研究结论。".into() },
    ], ..Default::default() }, ..Default::default() };
    if let Some(existing) = store.list_conversations(&paper.id)?.first() { conversation = store.conversation(&existing.id)?.ok_or("Conversation missing")? }
    else { store.save_conversation(&mut conversation)?; }
    // A valid one-pixel PNG, synthetic test data.
    let png = [137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,1,0,0,0,1,8,4,0,0,0,181,28,12,2,0,0,0,11,73,68,65,84,120,218,99,100,248,15,0,1,5,1,1,39,24,227,102,0,0,0,0,73,69,78,68,174,66,96,130];
    store.save_conversation_image(&conversation.id, "I-test", "image/png", &png)?;
    let args = json!({"paperId": paper.id, "conversations": [{"id": conversation.id}], "parent": parent});
    let doc = notes::from_args(&store, &info, &args)?;
    if !info.dir.join("note.md").exists() { println!("local {}", notes::export(&doc, &info.dir.join("note.md"))?); }
    let saved = wisp_net::notes::publish(&mut store, &info, &args).map_err(std::io::Error::other)?;
    println!("saved {saved}");
    let duplicate = wisp_net::notes::publish(&mut store, &info, &args).map_err(std::io::Error::other)?;
    assert_eq!(duplicate["duplicate"], true);
    println!("duplicate skipped");
    let mut append = args.clone();
    append["markdown"] = json!("## 第二次保存\n\n这是追加验证，第一页的笔记、图片和手工内容应保留。");
    let appended = wisp_net::notes::publish(&mut store, &info, &append).map_err(std::io::Error::other)?;
    assert_eq!(saved["page"], appended["page"]);
    println!("appended {appended}");
    let settings = wisp_core::settings::Settings::load(&wisp_core::settings::settings_file())?;
    let connector = settings.connectors.iter().find(|c| c.target == wisp_core::connector::NOTION_MCP).ok_or("Notion missing")?;
    let result = wisp_net::mcp::call(connector, "notion-fetch", &json!({"id": saved["page"]})).map_err(std::io::Error::other)?;
    std::fs::write(info.dir.join("notion-readback.json"), &result)?;
    assert!(result.contains("第二次保存") && result.contains("虚构测试笔记") && (result.contains("notion-file") || result.contains("image")));
    assert_eq!(store.get_paper(&paper.id)?.notes, paper.notes);
    println!("readback and original-note checks passed; library {}", info.dir.display());
    Ok(())
}
