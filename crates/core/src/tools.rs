//! The tools an agent works a library with. One table, three ways in: the
//! `wisp` command line (a tool name and flags read off its schema), the MCP
//! server (`mcp`) and the assistant inside the app. All of them end in
//! `call`, so a rule written here holds everywhere.
//!
//! Every tool here only reads. Extracting the page text of a stored PDF the
//! first time it is searched fills a cache; it changes nothing a person sees.

use crate::complete;
use crate::filter::{Filters, filter_papers};
use crate::fulltext::{self, terms};
use crate::libraries::{self, LibraryInfo};
use crate::model::{FULLTEXTS, Paper, READINGS, SCREENINGS, one_of};
use crate::openaccess;
use crate::pdftext;
use crate::sources::{self, Download, Fetch};
use crate::store::{SavedSearch, Store, Suggestion};
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub struct ToolDef {
    /// What the window calls it while it runs: 「检索全文」.
    pub title: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema of the arguments object.
    pub input_schema: Value,
    /// Whether it changes the library; the others only read.
    pub writes: bool,
    /// Whether the assistant in the window asks the person before it runs:
    /// the tools that bring papers in, take them out, or drop a saved search.
    pub confirm: bool,
}

/// A tool that works inside one library: it also takes `library`.
/// The tools that change the library.
const WRITING: [&str; 7] = ["save_search", "remove_search", "run_search", "undo_import", "suggest_screening", "sync_searches", "find_pdf"];
/// Of those, the ones the window's assistant runs only with the person's
/// consent. Saving a search or suggesting a screening decision changes
/// nothing a person has not yet to act on, so they run as asked.
const CONFIRMING: [&str; 4] = ["run_search", "undo_import", "remove_search", "find_pdf"];

fn scoped(name: &'static str, title: &'static str, description: &'static str, mut properties: Value, required: &[&str]) -> ToolDef {
    properties["library"] = json!({"type": "string", "description": "文献库的 ID 或名称；只有一个文献库时可以省略。"});
    ToolDef { name, title, description, input_schema: json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false}), writes: WRITING.contains(&name), confirm: CONFIRMING.contains(&name) }
}

/// The stored values of a status table, for a schema's `enum`.
fn keys(table: &[(&'static str, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(key, _)| *key).collect()
}

pub fn catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_libraries",
            title: "列出文献库",
            description: "列出所有文献库：ID、名称、主题和文献数量。其他工具用 library 参数指定其中一个。",
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            writes: false,
            confirm: false,
        },
        ToolDef {
            name: "sync_searches",
            title: "更新检索式",
            description: "运行到期的检索式并导入新文献。只处理开启了自动更新的文献库，其中已启用、且距上次运行超过设定间隔的检索式（上次失败的最多一小时后重试）。force 为 true 时不看间隔，运行所有已启用的检索式。不指定 library 时处理所有文献库。适合交给系统的计划任务定期执行。",
            input_schema: json!({"type": "object", "properties": {
                "library": {"type": "string", "description": "只更新这一个文献库（ID 或名称）；省略时更新所有文献库。"},
                "force": {"type": "boolean", "description": "不看更新间隔和是否开启自动更新，运行所有已启用的检索式。"},
            }, "additionalProperties": false}),
            writes: true,
            confirm: false,
        },
        scoped("library_status", "文献库概况", "一个文献库的概况：主题、文献数量、收录判断和阅读状态的分布、自定义词条，以及各状态值在界面上的显示名称。", json!({}), &[]),
        scoped(
            "list_papers",
            "查询文献",
            "查询文献库中的文献，返回简要记录和匹配总数。规则与界面上的列表相同：不指定 screening 时不包含判为「不相关」的文献，回收站中的文献不返回。",
            json!({
                "query": {"type": "string", "description": "在题目、作者、DOI、期刊、摘要、标签、词条和笔记中搜索；空格分开的每个词都必须出现。"},
                "screening": {"type": "string", "enum": keys(&SCREENINGS), "description": "收录判断。"},
                "reading": {"type": "string", "enum": keys(&READINGS), "description": "阅读状态。"},
                "fulltext": {"type": "string", "enum": keys(&FULLTEXTS), "description": "全文状态：stored 表示已保存 PDF 并读出了文字，textless 表示有 PDF 但没有读出文字。"},
                "tag": {"type": "string", "description": "带有这个标签。"},
                "year": {"type": "integer", "description": "发表年份。"},
                "sort": {"type": "string", "description": "newest（默认），或 title / year / journal / authors 加上 -asc、-desc，例如 year-desc。"},
                "limit": {"type": "integer", "description": "最多返回多少篇，默认 50，最大 500。"},
                "offset": {"type": "integer", "description": "跳过前多少篇，用于翻页。"},
            }),
            &[],
        ),
        scoped("get_paper", "读取文献", "按 ID 读取一篇文献的完整记录：书目信息、摘要、笔记、标签、自定义词条和附件。", json!({"id": {"type": "string", "description": "文献 ID，例如 list_papers 返回的 id。"}}), &["id"]),
        scoped(
            "search_fulltext",
            "检索全文",
            "在已保存的全文中检索短语或关键词，返回命中的文献、页码和逐字片段。不区分大小写和空白；整句命中排在各词分散命中之前。没有保存全文的文献不在检索范围内。",
            json!({
                "query": {"type": "string", "description": "要找的短语或关键词。"},
                "paperId": {"type": "string", "description": "只在这一篇文献中检索。"},
                "limit": {"type": "integer", "description": "最多返回多少处命中，默认 20，最大 100。"},
            }),
            &["query"],
        ),
        scoped(
            "find_sentences",
            "查找候选句",
            "写作时的参考：从文献库里各篇文献的同一部分（摘要、导言、方法、结果、讨论）找出句子，看别人在这个位置是怎么写的。给出 query 或 queries 时返回与它意思最接近的句子（按词匹配，英文效果最好）；不给时返回各篇文献这一部分开头的句子。句子逐字取自已保存的全文（摘要取自文献记录），带页码，可用于核对和引用；写作时应借鉴写法并注明出处，不要照抄。没有保存全文的文献只有摘要可查。",
            json!({
                "section": {"type": "string", "enum": keys(&crate::structure::SECTIONS), "description": "论文的哪个部分：abstract 摘要、introduction 导言、methods 方法、results 结果、discussion 讨论（含结论）、other 其他正文（如综述的主题章节）。"},
                "query": {"type": "string", "description": "正在写的句子或要表达的意思，最好用英文关键词。"},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "几组检索词（例如一组讲内容、一组讲论述方式、一组是同义说法），各自排序后合并，比一组词找得全；可与 query 同时给出。"},
                "paperId": {"type": "string", "description": "只看这一篇文献。"},
                "tag": {"type": "string", "description": "只看带有这个标签的文献。"},
                "starred": {"type": "boolean", "description": "为 true 时只看加了星标的文献。"},
                "language": {"type": "string", "enum": keys(&crate::structure::LANGUAGES), "description": "只要这种语言写的句子（写英文稿时别的语言的句子当不了范文）；不给时不分语言。认不出语言的文本不会被排除。"},
                "context": {"type": "boolean", "description": "为 true 时每句另附同一篇文献同一部分里的前一句和后一句（before、after），便于看清上下文。"},
                "limit": {"type": "integer", "description": "最多返回多少句，默认 20，最大 100。"},
            }),
            &["section"],
        ),
        scoped(
            "read_fulltext",
            "阅读全文",
            "按页读取一篇文献的全文，用于核对上下文和逐字引用。一次最多 8 页。",
            json!({
                "paperId": {"type": "string", "description": "文献 ID。"},
                "page": {"type": "integer", "description": "从第几页开始，默认 1。"},
                "pages": {"type": "integer", "description": "读几页，默认 4，最大 8。"},
            }),
            &["paperId"],
        ),
        scoped(
            "find_citations",
            "查找可引用的文献",
            "为一段文字逐句找出文献库中可能支持它的文献：按词语在题目、摘要、笔记和已保存的全文里匹配，返回每句的候选文献，全文命中的附页码和原文片段。只是按词语匹配的候选，是否真的支持这句话需要读原文判断。",
            json!({
                "text": {"type": "string", "description": "要配引用的文字，可以是一句或一段。"},
                "perSentence": {"type": "integer", "description": "每句最多几篇候选，默认 3，最大 10。"},
            }),
            &["text"],
        ),
        scoped(
            "find_metadata",
            "按题目查找记录",
            "为一篇缺 DOI、题目或作者的文献，按题目到 Crossref、Europe PMC 和 arXiv 查找可能对应的记录，不向文献库写入任何内容。返回每个来源的情况（命中条数，或没查成的原因）和候选记录：题目、作者、年份、期刊、DOI、PMID、与所查题目的相似度、一致和不一致的地方、是否已是库里的另一篇文献。候选只是候选：题目相近不等于同一篇，预印本和正式版是两条记录。把候选和差异告诉用户，由用户在窗口里（文献详情的「按题目补全」）选定并确认写入的字段；这个工具和其他工具都不会替用户写入。不给 title 时用文献现有的题目；题目还是文件名时，用 PDF 首页读出的题目。",
            json!({
                "paperId": {"type": "string", "description": "要补全的文献的 ID。"},
                "title": {"type": "string", "description": "用来查找的题目；不写则用文献现有的题目或 PDF 首页的题目。"},
                "author": {"type": "string", "description": "已知的一位作者的姓名，用来缩小范围。"},
                "year": {"type": "integer", "description": "已知的发表年份，用来比对。"},
            }),
            &["paperId"],
        ),
        scoped(
            "search_online",
            "试检索",
            "在文献数据库里试运行一个检索式，不向文献库写入任何内容。返回数据库的命中总数和最新的若干条记录，并标出每条是否已在文献库中、用户对它的收录判断，用来判断检索式是否靠谱。各来源的检索语法不同：pubmed 和 europepmc 支持布尔运算符和字段，arxiv 用 ti: / abs: / cat: 前缀，crossref 只按词语匹配。",
            json!({
                "source": {"type": "string", "enum": sources::KINDS.iter().map(|(key, _)| *key).collect::<Vec<_>>(), "description": "文献数据库。"},
                "query": {"type": "string", "description": "按该来源的语法写的检索式。"},
                "limit": {"type": "integer", "description": "取回多少条最新记录，默认 20，最大 100。"},
            }),
            &["source", "query"],
        ),
        scoped("list_searches", "已保存的检索式", "列出文献库保存的检索式：来源、检索式、每次取回的篇数，以及最近一次运行的结果。", json!({}), &[]),
        scoped(
            "save_search",
            "保存检索式",
            "新建或修改一个保存的检索式。不带 id 时新建，必须给出 source 和 query；带 id 时只改动给出的项。保存不会运行它。",
            json!({
                "id": {"type": "string", "description": "要修改的检索式的 ID；新建时省略。"},
                "source": {"type": "string", "enum": sources::KINDS.iter().map(|(key, _)| *key).collect::<Vec<_>>(), "description": "文献数据库。"},
                "query": {"type": "string", "description": "按该来源的语法写的检索式。"},
                "name": {"type": "string", "description": "给人看的名称；省略时用来源的名字。"},
                "maxResults": {"type": "integer", "description": "每次运行取回多少条最新记录，默认 50，最大 200。"},
                "enabled": {"type": "boolean", "description": "是否参与「全部运行」和定时更新，默认是。"},
            }),
            &[],
        ),
        scoped("remove_search", "删除检索式", "删除一个保存的检索式。它导入过的文献和运行记录都保留。", json!({"id": {"type": "string", "description": "检索式的 ID。"}}), &["id"]),
        scoped(
            "run_search",
            "运行检索式",
            "运行一个保存的检索式，把取回的记录导入文献库：已有的文献（按 DOI、PMID、题目识别，回收站里的也算）被跳过。新文献的收录判断都是「待判断」。返回批次号，可以用 undo_import 整批撤销。",
            json!({"id": {"type": "string", "description": "检索式的 ID。"}}),
            &["id"],
        ),
        scoped("list_imports", "导入记录", "最近的导入批次：哪个检索式、何时运行、数据库命中总数、取回、新增和跳过的篇数，以及失败原因。", json!({"limit": {"type": "integer", "description": "最多返回多少条，默认 20。"}}), &[]),
        scoped(
            "undo_import",
            "撤销导入",
            "撤销一个导入批次：删除它新增的、还没有人处理过的文献。已经做过收录判断、读过、收藏、写过笔记、加过标签或附件的文献保留，并在结果里列出。",
            json!({"batchId": {"type": "string", "description": "run_search 或 list_imports 给出的批次号。"}}),
            &["batchId"],
        ),
        scoped(
            "find_pdf",
            "查找开放全文",
            "为还没有 PDF 的文献查找开放获取的全文，下载后附到原来的条目上，并读出文字。只找可以免费合法获取的版本：arXiv、OpenAlex 和 Europe PMC 列出的开放获取文件、以及 Springer / BMC、Nature、Frontiers 的开放获取文章；不登录，不绕过付费墙，找不到就如实说没有。给出 paperId 时只处理这一篇，否则处理文献库里没有 PDF 的文献，一次最多 limit 篇。会把文献的 DOI 或 PMID 发给 OpenAlex、Europe PMC 和出版商。",
            json!({
                "paperId": {"type": "string", "description": "只为这一篇文献查找；省略时处理文献库里没有 PDF 的文献。"},
                "limit": {"type": "integer", "description": "省略 paperId 时一次最多处理几篇，默认 10，最多 50。"},
                "lookOnly": {"type": "boolean", "description": "只查找、不下载：返回每篇文献的候选地址。"},
            }),
            &[],
        ),
        scoped(
            "suggest_screening",
            "提出收录建议",
            "为一篇文献提出收录判断的建议，供用户在筛选页确认。这不会改变文献的收录判断：判断只由用户本人做出。同一篇文献的新建议会替换旧的。按文献库的筛选目标判断（library_status 的 screeningGoal；为空时按主题 topic）。建议前先读这篇文献的记录（get_paper），有全文时读全文（read_fulltext）。",
            json!({
                "id": {"type": "string", "description": "文献 ID。"},
                "verdict": {"type": "string", "enum": ["included", "excluded", "irrelevant", "uncertain"], "description": "included：符合文献库的主题，应当收录；excluded：不完全符合但相关、值得留着备查（界面上显示为「相关」）；irrelevant：与主题无关；uncertain：信息不足，无法判断。"},
                "reason": {"type": "string", "description": "1 到 3 句理由，说出文中的具体依据。"},
                "excerpt": {"type": "string", "description": "支持判断的一段原文，逐字摘自摘要或全文。不在原文中的引文不会被保存。"},
                "confidence": {"type": "number", "description": "对这个判断的把握，0 到 1。"},
                "author": {"type": "string", "description": "提出建议的是谁，例如模型或 agent 的名字。"},
            }),
            &["id", "verdict", "reason"],
        ),
    ]
}

/// The tools of the assistant inside an open library: no choosing a library.
pub fn library_catalog() -> Vec<ToolDef> {
    let mut tools = catalog();
    // The tools that range over every library have no place inside one.
    tools.retain(|t| t.name != "list_libraries" && t.name != "sync_searches");
    for tool in &mut tools {
        if let Some(properties) = tool.input_schema["properties"].as_object_mut() { properties.remove("library"); }
    }
    tools
}

/// The tool `name` and whether `args` fit its schema's names.
fn checked(name: &str, args: &Value) -> Result<ToolDef, String> {
    let Some(tool) = catalog().into_iter().find(|t| t.name == name) else { return Err(format!("没有这个工具：{name}")) };
    let Some(given) = args.as_object() else { return Err("参数应为一个 JSON 对象。".into()) };
    if let Some(unknown) = given.keys().find(|key| tool.input_schema["properties"].get(key.as_str()).is_none()) {
        return Err(format!("{name} 没有参数 {unknown}。"));
    }
    if let Some(missing) = tool.input_schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).find(|key| args[*key].is_null()) {
        return Err(format!("{name} 需要参数 {missing}。"));
    }
    Ok(tool)
}

/// What a tool result may add to a model's context; a longer one is cut and says so.
pub const MAX_OUTPUT_BYTES: usize = 60_000;

/// A tool result as the text a model reads.
pub fn output_text(value: &Value) -> String {
    let text = value.to_string();
    if text.len() <= MAX_OUTPUT_BYTES { return text }
    let cut = text.floor_char_boundary(MAX_OUTPUT_BYTES);
    format!("{}\n…[结果过长，只显示了 {} 字节中的前 {cut} 字节；请缩小范围后重试]", &text[..cut], text.len())
}

/// One line for the window about a call: 「检索全文：chromium」.
pub fn describe_call(name: &str, args: &Value) -> String {
    let title = catalog().into_iter().find(|t| t.name == name).map_or(name.to_string(), |t| t.title.to_string());
    // Words first, then numbers, each group by name: the order a JSON object
    // keeps its keys in depends on how serde_json was built.
    let (mut words, mut numbers) = (vec![], vec![]);
    for (key, value) in args.as_object().into_iter().flatten().filter(|(key, _)| key.as_str() != "library") {
        match (key.as_str(), value) {
            ("page", Value::Number(n)) => numbers.push((key, format!("第 {n} 页起"))),
            (_, Value::String(text)) if !text.trim().is_empty() => words.push((key, text.split_whitespace().collect::<Vec<_>>().join(" "))),
            (_, Value::Number(n)) => numbers.push((key, format!("{key} {n}"))),
            _ => {}
        }
    }
    words.sort();
    numbers.sort();
    let detail: String = words.into_iter().chain(numbers).map(|(_, part)| part).collect::<Vec<_>>().join(" · ");
    if detail.is_empty() { return title }
    if detail.chars().count() > 80 { format!("{title}：{}…", detail.chars().take(80).collect::<String>()) } else { format!("{title}：{detail}") }
}

/// One line for the window about what came back.
pub fn describe_result(result: &Result<Value, String>) -> String {
    match result {
        Err(error) => error.clone(),
        Ok(value) => {
            if let Some(total) = value["total"].as_u64() {
                format!("{total} 条结果")
            } else if let Some(pages) = value["pages"].as_array() {
                format!("读了 {} 页", pages.len())
            } else if let Some(sentences) = value["sentences"].as_array() {
                format!("{} 句", sentences.len())
            } else {
                "完成".into()
            }
        }
    }
}

/// Run a tool against the data folder `root`. The error is a sentence for
/// whoever called: a person at a terminal or a model.
pub fn call(root: &Path, name: &str, args: &Value, fetch: Fetch, download: Download) -> Result<Value, String> {
    checked(name, args)?;
    if name == "sync_searches" { return sync_searches(root, args, fetch, crate::time::now_millis()).map_err(|e| format!("{e:#}")) }
    if name == "list_libraries" {
        let all: Vec<Value> = libraries::list(root).iter().map(|l| json!({"id": l.id, "name": l.name, "topic": l.topic, "papers": l.paper_count, "autoSyncHours": l.auto_sync_hours})).collect();
        return Ok(json!({"libraries": all}));
    }
    let info = find_library(root, args["library"].as_str().unwrap_or(""))?;
    let mut store = Store::open(&info.dir).map_err(|e| format!("{e:#}"))?;
    call_in(&mut store, &info, name, args, fetch, download)
}

/// Run a library tool on a library that is already open.
pub fn call_in(store: &mut Store, info: &LibraryInfo, name: &str, args: &Value, fetch: Fetch, download: Download) -> Result<Value, String> {
    checked(name, args)?;
    dispatch(store, info, name, args, fetch, download).map_err(|e| format!("{e:#}"))
}

fn dispatch(store: &mut Store, info: &LibraryInfo, name: &str, args: &Value, fetch: Fetch, download: Download) -> Result<Value> {
    match name {
        "find_pdf" => find_pdf(store, args, fetch, download),
        "find_citations" => find_citations(store, args),
        "find_sentences" => find_sentences(store, args),
        "find_metadata" => find_metadata(store, args, fetch),
        "search_online" => search_online(store, args, fetch),
        // With how each database wants its queries written, for whoever composes the next one.
        "list_searches" => Ok(json!({"searches": store.list_searches()?, "syntax": sources::KINDS.iter().map(|(key, _)| (key.to_string(), json!(sources::syntax(key)))).collect::<Map<String, Value>>()})),
        "save_search" => save_search(store, args),
        "remove_search" => store.remove_search(text(args, "id")?).map(|()| json!({"removed": true})),
        "run_search" => run_search(store, text(args, "id")?, fetch),
        "list_imports" => Ok(json!({"imports": store.list_batches(number(args, "limit", 20, 200)?)?})),
        "undo_import" => undo_import(store, args),
        "suggest_screening" => suggest_screening(store, info, args),
        "library_status" => library_status(store, info),
        "list_papers" => list_papers(store, info, args),
        "get_paper" => get_paper(store, args),
        "search_fulltext" => search_fulltext(store, args),
        "read_fulltext" => read_fulltext(store, args),
        _ => Err(anyhow!("没有这个工具：{name}")),
    }
}

/// The library `wanted` names by id or by name; the only one when it is empty.
pub fn find_library(root: &Path, wanted: &str) -> Result<LibraryInfo, String> {
    let mut all = libraries::list(root);
    let names = |all: &[LibraryInfo]| all.iter().map(|l| format!("「{}」", l.name)).collect::<Vec<_>>().join("、");
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return match all.len() {
            0 => Err(format!("{} 里还没有文献库。", root.display())),
            1 => Ok(all.remove(0)),
            _ => Err(format!("有多个文献库，请用 library 指定其中一个：{}", names(&all))),
        };
    }
    let found: Vec<usize> = (0..all.len()).filter(|&i| all[i].id == wanted || all[i].name.to_lowercase() == wanted.to_lowercase()).collect();
    match found[..] {
        [i] => Ok(all.remove(i)),
        [] if all.is_empty() => Err(format!("{} 里还没有文献库。", root.display())),
        [] => Err(format!("没有名为「{wanted}」的文献库。现有：{}", names(&all))),
        _ => Err(format!("有多个文献库叫「{wanted}」，请改用 ID。")),
    }
}

// ---- arguments

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match &args[key] {
        Value::Null => Ok(""),
        Value::String(value) => Ok(value.trim()),
        _ => bail!("参数 {key} 应为字符串。"),
    }
}

fn number(args: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
    match &args[key] {
        Value::Null => Ok(default),
        value => value.as_u64().map(|n| (n as usize).min(max)).ok_or_else(|| anyhow!("参数 {key} 应为非负整数。")),
    }
}

fn choice<'a>(args: &'a Value, key: &str, table: &[(&str, &str)]) -> Result<&'a str> {
    let value = text(args, key)?;
    if !value.is_empty() && !one_of(table, value) { bail!("参数 {key} 应为 {} 之一。", table.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("、")) }
    Ok(value)
}

// ---- the tools

fn tally(papers: &[Paper], table: &[(&str, &str)], value: impl Fn(&Paper) -> &str) -> Value {
    Value::Object(table.iter().map(|(key, _)| (key.to_string(), json!(papers.iter().filter(|p| value(p) == *key).count()))).collect())
}

fn library_status(store: &mut Store, info: &LibraryInfo) -> Result<Value> {
    let papers = store.list_papers()?;
    let labels = |table: &[(&str, &str)]| Value::Object(table.iter().map(|(key, label)| (key.to_string(), json!(label))).collect());
    let fields: Vec<Value> = store.list_fields()?.iter().map(|f| json!({"name": f.name, "kind": f.kind.as_str(), "options": f.options})).collect();
    Ok(json!({
        "id": info.id,
        "name": info.name,
        "topic": info.topic,
        // What screening suggestions judge against; empty means by the topic.
        "screeningGoal": info.criteria,
        "folder": info.dir.display().to_string(),
        "papers": papers.len(),
        "inTrash": store.list_trash()?.len(),
        "screening": tally(&papers, &SCREENINGS, |p| &p.screening),
        "reading": tally(&papers, &READINGS, |p| &p.reading),
        "fulltext": tally(&papers, &FULLTEXTS, |p| &p.fulltext),
        "fields": fields,
        // The stored keys are not what the window shows: `excluded` reads 「相关」.
        "labels": {"screening": labels(&SCREENINGS), "reading": labels(&READINGS), "fulltext": labels(&FULLTEXTS)},
    }))
}

fn list_papers(store: &mut Store, info: &LibraryInfo, args: &Value) -> Result<Value> {
    let filters = Filters {
        search: text(args, "query")?.into(),
        screening: choice(args, "screening", &SCREENINGS)?.into(),
        reading: choice(args, "reading", &READINGS)?.into(),
        fulltext: choice(args, "fulltext", &FULLTEXTS)?.into(),
        tag: text(args, "tag")?.into(),
        year: if args["year"].is_null() { String::new() } else { number(args, "year", 0, 9999)?.to_string() },
        sort: text(args, "sort")?.into(),
        ..Default::default()
    };
    let (limit, offset) = (number(args, "limit", 50, 500)?, number(args, "offset", 0, usize::MAX)?);
    let papers = store.list_papers()?;
    // What an assistant has proposed for each paper, beside what its owner decided.
    let suggestions = store.list_suggestions()?;
    let suggested: BTreeMap<&str, &str> = suggestions.iter().map(|s| (s.paper_id.as_str(), s.verdict.as_str())).collect();
    let shown = filter_papers(&papers, &filters, crate::time::now_millis());
    let page: Vec<Value> = shown
        .iter()
        .skip(offset)
        .take(limit)
        .map(|&i| {
            let p = &papers[i];
            json!({"id": p.id, "title": p.title, "authors": p.short_authors(), "year": p.year, "journal": p.journal, "doi": p.doi, "pmid": p.pmid,
                "screening": p.screening, "reading": p.reading, "fulltext": p.fulltext, "starred": p.starred, "tags": p.tags, "suggested": suggested.get(p.id.as_str())})
        })
        .collect();
    Ok(json!({"library": info.name, "total": shown.len(), "offset": offset, "papers": page}))
}

fn get_paper(store: &mut Store, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "id")?)?;
    let mut out = serde_json::to_value(&paper)?;
    // Custom fields by the name a person gave them, in the library's order.
    let mut fields = Map::new();
    for field in store.list_fields()? {
        if let Some(values) = paper.fields.get(&field.id) { fields.insert(field.name, json!(values)); }
    }
    out["fields"] = Value::Object(fields);
    out["link"] = json!(paper.link());
    out["suggestion"] = json!(store.suggestion(&paper.id)?);
    let mut files = vec![];
    for file in store.paper_files(&paper.id)? {
        let pages = match store.fulltext_pages(&file.id)? {
            Some(Ok(pages)) => json!(pages.len()),
            _ => Value::Null,
        };
        files.push(json!({"id": file.id, "kind": file.kind, "title": file.title, "format": file.format, "sizeBytes": file.size_bytes, "path": store.file_path(&file).display().to_string(), "textPages": pages}));
    }
    out["files"] = json!(files);
    Ok(out)
}

/// Extract the page text of the full-text files that have none cached yet:
/// those of one paper, or of the whole library.
fn extract_missing(store: &mut Store, paper_id: Option<&str>) -> Result<()> {
    // Most calls find everything read and sorted: one look says so, without going through the files.
    if !store.structure_wanted(paper_id.unwrap_or(""))? { return Ok(()) }
    let files = match paper_id {
        Some(id) => store.paper_files(id)?,
        None => store.list_files()?,
    };
    for file in files.iter().filter(|f| f.kind == "fulltext") {
        if !store.needs_structure(&file.id)? { continue }
        let pages = crate::fulltext::reread(&store.file_path(file), store.fulltext_pages(&file.id)?);
        store.save_fulltext_pages(&file.id, &pages)?;
    }
    store.restructure_outdated()?;
    Ok(())
}

fn find_sentences(store: &mut Store, args: &Value) -> Result<Value> {
    let section = choice(args, "section", &crate::structure::SECTIONS)?;
    if section.is_empty() { bail!("请用 section 指明论文的哪个部分。") }
    let mut queries: Vec<String> = match &args["queries"] {
        Value::Null => vec![],
        Value::Array(items) => items.iter().map(|item| item.as_str().map(|q| q.trim().to_string()).ok_or_else(|| anyhow::anyhow!("queries 应是一组字符串。"))).collect::<Result<_>>()?,
        _ => bail!("queries 应是一组字符串。"),
    };
    let query = text(args, "query")?.trim();
    if !query.is_empty() { queries.insert(0, query.to_string()) }
    queries.retain(|q| !q.is_empty());
    let only = text(args, "paperId")?;
    let tag = text(args, "tag")?;
    let starred = args["starred"].as_bool().unwrap_or(false);
    let context = args["context"].as_bool().unwrap_or(false);
    let language = choice(args, "language", &crate::structure::LANGUAGES)?;
    let limit = number(args, "limit", 20, 100)?.max(1);
    extract_missing(store, (!only.is_empty()).then_some(only))?;
    let papers: Vec<Paper> = store.list_papers()?.into_iter().filter(|p| p.screening != "irrelevant" && (only.is_empty() || p.id == only) && (tag.is_empty() || p.tags.iter().any(|t| t == tag)) && (!starred || p.starred)).collect();
    let by_id: BTreeMap<&str, &Paper> = papers.iter().map(|p| (p.id.as_str(), p)).collect();
    // The abstract is the record's; the other parts are read off the stored full text.
    let found: Vec<crate::store::PaperSentence> = if section == "abstract" {
        papers
            .iter()
            .flat_map(|p| {
                let kept = crate::structure::split_sentences(&p.abstract_text).into_iter().filter(|s| crate::structure::usable(s)).enumerate();
                let written = crate::structure::language(std::slice::from_ref(&p.abstract_text));
                kept.map(|(position, text)| crate::store::PaperSentence { paper_id: p.id.clone(), sentence: crate::structure::Sentence { section: "abstract".into(), position, text, language: written.to_string(), ..Default::default() } }).collect::<Vec<_>>()
            })
            .collect()
    } else {
        store.section_sentences(section, only)?.into_iter().filter(|s| by_id.contains_key(s.paper_id.as_str())).collect()
    };
    // A sentence in another language is no model for this one; a text too short to tell stays.
    let in_all = found.len();
    let found: Vec<crate::store::PaperSentence> = found.into_iter().filter(|s| language.is_empty() || s.sentence.language.is_empty() || s.sentence.language == language).collect();
    let texts: Vec<&str> = found.iter().map(|s| s.sentence.text.as_str()).collect();
    let owners: Vec<&str> = found.iter().map(|s| s.paper_id.as_str()).collect();
    let picked: Vec<usize> = if queries.is_empty() {
        // How each paper opens the part: first sentences first, paper by paper.
        let mut order: Vec<usize> = (0..found.len()).collect();
        order.sort_by_key(|i| (found[*i].sentence.position, *i));
        order.into_iter().take(limit).collect()
    } else {
        // Each query ranked on its own over the part's words (made once and kept), the lists merged.
        let index = crate::retrieval::index_for(&texts);
        let lists: Vec<Vec<(usize, f32)>> = queries.iter().map(|q| index.rank(q)).collect();
        crate::retrieval::fuse(&lists, &owners, limit)
    };
    // The sentence next to one, in the same paper and part, if there is one.
    let beside = |index: usize, step: isize| -> Option<&str> {
        let other = index.checked_add_signed(step)?;
        let (here, there) = (&found[index], found.get(other)?);
        (there.paper_id == here.paper_id && there.sentence.position as isize == here.sentence.position as isize + step).then_some(there.sentence.text.as_str())
    };
    let sentences: Vec<Value> = picked
        .into_iter()
        .map(|index| {
            let (paper, sentence) = (by_id[owners[index]], &found[index].sentence);
            let mut out = json!({"paperId": paper.id, "title": paper.title, "cite": paper.citation(), "journal": paper.journal, "position": sentence.position, "text": sentence.text});
            if sentence.page > 0 { out["page"] = json!(sentence.page) }
            if !sentence.heading.is_empty() { out["heading"] = json!(sentence.heading) }
            if context {
                if let Some(before) = beside(index, -1) { out["before"] = json!(before) }
                if let Some(after) = beside(index, 1) { out["after"] = json!(after) }
            }
            out
        })
        .collect();
    let with_sentences: std::collections::BTreeSet<&str> = owners.iter().copied().collect();
    Ok(json!({
        "section": section,
        "query": queries.first().map_or("", String::as_str),
        "queries": queries,
        "sentences": sentences,
        // What there was to choose from: a short list must not pass for "nobody writes this".
        "sentencesInSection": found.len(),
        "papersWithSection": with_sentences.len(),
        "papersWithout": papers.len() - with_sentences.len(),
        // Left out for being written in another language than the one asked for.
        "sentencesInOtherLanguages": in_all - found.len(),
    }))
}

/// A text ready for matching: lowercase, every whitespace character a space.
/// Byte for byte as long as the original, so a position found in it is a
/// position in the original.
fn searchable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            _ if c.is_whitespace() && c.len_utf8() == 1 => out.push(' '),
            (Some(l), None) if l.len_utf8() == c.len_utf8() => out.push(l),
            _ => out.push(c),
        }
    }
    out
}

/// Whether a page answers the query, how well, and where: the whole phrase
/// beats any number of scattered words, and of the words at least half must appear.
fn page_match(hay: &str, phrase: &str, words: &[String]) -> Option<(f32, usize, bool)> {
    if let Some(at) = hay.find(phrase) { return Some((1000. + hay.matches(phrase).count() as f32, at, true)) }
    let found: Vec<usize> = words.iter().filter_map(|word| hay.find(word.as_str())).collect();
    if found.is_empty() || found.len() * 2 < words.len() { return None }
    Some((found.len() as f32, found[0], false))
}

/// The passage around byte `at` of a page, as printed apart from its line breaks.
fn snippet(page: &str, at: usize) -> String {
    let start = page.floor_char_boundary(at.saturating_sub(100));
    let end = page.ceil_char_boundary((at + 220).min(page.len()));
    page[start..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn search_fulltext(store: &mut Store, args: &Value) -> Result<Value> {
    let query = text(args, "query")?;
    let phrase = searchable(query).split_whitespace().collect::<Vec<_>>().join(" ");
    if phrase.is_empty() { bail!("请提供要检索的短语或关键词。") }
    let words = terms(query);
    let only = text(args, "paperId")?;
    let limit = number(args, "limit", 20, 100)?.max(1);
    extract_missing(store, (!only.is_empty()).then_some(only))?;
    let papers = store.list_papers()?;
    let titles: BTreeMap<&str, &str> = papers.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
    let texts = store.all_fulltext()?;
    let mut hits = vec![];
    for (paper_id, (_, pages)) in texts.iter().filter(|(id, _)| only.is_empty() || id.as_str() == only) {
        for (n, page) in pages.iter().enumerate() {
            if let Some((score, at, exact)) = page_match(&searchable(page), &phrase, &words) { hits.push((score, paper_id.as_str(), n, snippet(page, at), exact)) }
        }
    }
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(&b.2)));
    let total = hits.len();
    let shown: Vec<Value> = hits.into_iter().take(limit).map(|(_, id, n, text, exact)| json!({"paperId": id, "title": titles.get(id).copied().unwrap_or(""), "page": n + 1, "exact": exact, "snippet": text})).collect();
    // The papers with a PDF whose text could not be had: the search did not see into them.
    let mut unreadable: Vec<Value> = vec![];
    for (_, paper_id, reason) in store.fulltext_failures()?.into_iter().filter(|(_, id, _)| only.is_empty() || id.as_str() == only) {
        if unreadable.last().is_some_and(|last| last["paperId"] == paper_id.as_str()) { continue }
        unreadable.push(json!({"paperId": paper_id, "title": titles.get(paper_id.as_str()).copied().unwrap_or(""), "kind": fulltext::failure_kind(&reason), "reason": reason}));
    }
    let mut found = json!({
        "query": query,
        "total": total,
        "hits": shown,
        // What the search could see: an answer must not pass for "the library says nothing".
        "papersSearched": texts.keys().filter(|id| only.is_empty() || id.as_str() == only).count(),
        "papersWithoutText": papers.iter().filter(|p| (only.is_empty() || p.id == only) && !texts.contains_key(&p.id)).count(),
    });
    if !unreadable.is_empty() {
        found["unreadableCount"] = json!(unreadable.len());
        unreadable.truncate(UNREADABLE_LISTED);
        found["unreadable"] = json!(unreadable);
        found["note"] = json!("unreadable 里的文献有 PDF，但文字没有读出来，这次检索看不到它们的正文：没有命中不说明它们没有相关内容。reason 是读取时得到的结果，不要据此推测文件的其他情况。");
    }
    Ok(found)
}

fn read_fulltext(store: &mut Store, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "paperId")?)?;
    extract_missing(store, Some(&paper.id))?;
    let mut failure = String::new();
    let mut found = None;
    for file in store.paper_files(&paper.id)?.into_iter().filter(|f| f.kind == "fulltext") {
        match store.fulltext_pages(&file.id)? {
            Some(Ok(pages)) => {
                found = Some(pages);
                break;
            }
            Some(Err(error)) => failure = error,
            None => {}
        }
    }
    let Some(pages) = found else {
        if failure.is_empty() { bail!("「{}」没有保存全文，只有书目信息和摘要。", paper.title) }
        // Said to a model as much as to a person: what is known, and where the knowledge ends.
        bail!("「{}」有 PDF，但全文的文字没有读出来：{failure}现在只有它的书目信息和摘要可用；这句话说的是读取的结果，不要据此推测文件的其他情况。", paper.title)
    };
    let from = number(args, "page", 1, usize::MAX)?.max(1);
    let count = number(args, "pages", 4, 8)?.max(1);
    if from > pages.len() { bail!("这篇文献的全文只有 {} 页。", pages.len()) }
    let read: Vec<Value> = pages.iter().enumerate().skip(from - 1).take(count).map(|(n, page)| json!({"page": n + 1, "text": page})).collect();
    Ok(json!({"paperId": paper.id, "title": paper.title, "pageCount": pages.len(), "pages": read}))
}

/// A name for a fetched file: how the paper is cited, as a file name.
fn file_name(paper: &Paper) -> String {
    let base = if paper.authors.trim().is_empty() { paper.title.clone() } else { paper.citation() };
    let name: String = base.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { ' ' } else { c }).collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}.pdf", if name.is_empty() { "全文".to_string() } else { name.chars().take(80).collect() })
}

/// Look for an open-access full text of one paper and keep it with the paper.
fn fetch_pdf(store: &mut Store, paper: &Paper, look_only: bool, fetch: Fetch, download: Download) -> Result<Value> {
    let (candidates, mut failures) = openaccess::candidates(paper, fetch);
    let listed: Vec<Value> = candidates.iter().map(|c| json!({"url": c.url, "source": c.source, "version": c.version})).collect();
    let mut row = json!({"paperId": paper.id, "title": paper.title});
    if look_only {
        row["outcome"] = json!(if candidates.is_empty() { "not_found" } else { "candidates" });
        row["candidates"] = json!(listed);
        if !failures.is_empty() { row["reason"] = json!(failures.join("；")) }
        return Ok(row);
    }
    for candidate in &candidates {
        match download(&candidate.url) {
            Ok(bytes) if openaccess::is_pdf(&bytes) => {
                let file = store.import_fetched(&paper.id, &file_name(paper), &bytes, "fulltext", &candidate.url, &candidate.source, candidate.version)?;
                // Saved is not yet readable: the text is read at once, and what came of it said.
                let pages = fulltext::file_pages(&store.file_path(&file));
                store.save_fulltext_pages(&file.id, &pages)?;
                row["outcome"] = json!("saved");
                row["source"] = json!(candidate.source);
                row["version"] = json!(candidate.version);
                row["url"] = json!(candidate.url);
                row["sizeBytes"] = json!(file.size_bytes);
                match pages {
                    Ok(pages) => row["pages"] = json!(pages.len()),
                    Err(reason) => {
                        row["textKind"] = json!(fulltext::failure_kind(&reason));
                        row["textReason"] = json!(reason);
                    }
                }
                return Ok(row);
            }
            Ok(_) => failures.push(format!("{}：返回的不是 PDF，多半是登录页或验证页", candidate.source)),
            Err(err) => failures.push(format!("{}：{err}", candidate.source)),
        }
    }
    row["outcome"] = json!(if candidates.is_empty() { "not_found" } else { "failed" });
    // A file is listed and was not handed out: some publishers give it to a browser only.
    if !candidates.is_empty() { failures.push("这些地址不向程序提供文件时，可以在浏览器里打开文献的链接下载，再把 PDF 拖进窗口".into()) }
    if candidates.is_empty() && failures.is_empty() { failures.push("没有找到开放获取的全文。可以打开文献的链接自己下载，再把 PDF 拖进窗口：它会附到这一条上。".into()) }
    row["reason"] = json!(failures.join("；"));
    Ok(row)
}

fn find_pdf(store: &mut Store, args: &Value, fetch: Fetch, download: Download) -> Result<Value> {
    let only = text(args, "paperId")?;
    let limit = number(args, "limit", 10, 50)?.max(1);
    let look_only = args["lookOnly"].as_bool().unwrap_or(false);
    let known = openaccess::lookable;
    let (todo, remaining, nameless) = if only.is_empty() {
        let lacking: Vec<Paper> = store.list_papers()?.into_iter().filter(|p| matches!(p.fulltext.as_str(), "missing" | "linked")).collect();
        let nameless = lacking.iter().filter(|p| !known(p)).count();
        let mut todo: Vec<Paper> = lacking.into_iter().filter(known).collect();
        let remaining = todo.len().saturating_sub(limit);
        todo.truncate(limit);
        (todo, remaining, nameless)
    } else {
        let paper = store.get_paper(only)?;
        if matches!(paper.fulltext.as_str(), "stored" | "textless") { bail!("「{}」已经有 PDF 了。", paper.title) }
        if !known(&paper) { bail!("「{}」没有 DOI、PMID 或 arXiv 编号，无从查起。先补全它的编号。", paper.title) }
        (vec![paper], 0, 0)
    };
    let mut rows = vec![];
    for paper in &todo {
        rows.push(fetch_pdf(store, paper, look_only, fetch, download)?);
    }
    let count = |outcome: &str| rows.iter().filter(|row| row["outcome"] == outcome).count();
    Ok(json!({
        "checked": rows.len(),
        "saved": count("saved"),
        "notFound": count("not_found"),
        "failed": count("failed"),
        // Papers without a PDF this call did not get to, and those with no number to look them up by.
        "remaining": remaining,
        "withoutIdentifier": nameless,
        "papers": rows,
    }))
}

/// The sentences of a text: full stops of either script, question and
/// exclamation marks, semicolons and line breaks end one. A Latin full stop
/// only does when a space follows, so "0.05" and "e.g.x" stay whole.
fn sentences(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let ends = match c {
            '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n' => true,
            '.' => chars.peek().is_none_or(|next| next.is_whitespace()),
            _ => false,
        };
        if c != '\n' { current.push(c) }
        if ends {
            if !current.trim().is_empty() { out.push(current.trim().to_string()) }
            current.clear();
        }
    }
    if !current.trim().is_empty() { out.push(current.trim().to_string()) }
    out
}

/// The papers without readable text that a search names; the rest are counted.
const UNREADABLE_LISTED: usize = 20;
const MAX_SENTENCES: usize = 40;

fn find_citations(store: &mut Store, args: &Value) -> Result<Value> {
    let text = text(args, "text")?;
    if text.is_empty() { bail!("请提供要配引用的文字。") }
    let per_sentence = number(args, "perSentence", 3, 10)?.max(1);
    extract_missing(store, None)?;
    let papers: Vec<Paper> = store.list_papers()?.into_iter().filter(|p| p.screening != "irrelevant").collect();
    let texts = store.all_fulltext()?;
    // Lowercased once: every sentence is matched against every record and page.
    let records: Vec<String> = papers.iter().map(|p| searchable(&format!("{} {} {} {}", p.title, p.abstract_text, p.tags.join(" "), p.notes))).collect();
    let pages: Vec<Vec<String>> = papers.iter().map(|p| texts.get(&p.id).map(|(_, pages)| pages.iter().map(|page| searchable(page)).collect()).unwrap_or_default()).collect();
    let all = sentences(text);
    let mut out = vec![];
    for sentence in all.iter().take(MAX_SENTENCES) {
        let words = terms(sentence);
        let mut candidates = vec![];
        // A sentence of one or two words says too little to match on.
        if words.len() >= 3 {
            for (i, paper) in papers.iter().enumerate() {
                let in_record = words.iter().filter(|word| records[i].contains(word.as_str())).count();
                // The page holding the most of the sentence's words, and where its first one stands.
                let best = pages[i].iter().enumerate().map(|(n, page)| (words.iter().filter(|word| page.contains(word.as_str())).count(), n)).max_by_key(|&(count, n)| (count, std::cmp::Reverse(n)));
                let (count, page) = match best {
                    Some((count, n)) if count > in_record => (count, Some(n)),
                    _ => (in_record, None),
                };
                if count * 2 < words.len() { continue }
                let mut candidate = json!({"paperId": paper.id, "title": paper.title, "year": paper.year, "matched": count, "of": words.len(), "in": if page.is_some() { "fulltext" } else { "record" }});
                if let Some(n) = page {
                    let original = &texts[&paper.id].1[n];
                    let at = words.iter().filter_map(|word| pages[i][n].find(word.as_str())).min().unwrap_or(0);
                    candidate["page"] = json!(n + 1);
                    candidate["snippet"] = json!(snippet(original, at));
                }
                candidates.push((count, candidate));
            }
        }
        candidates.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        out.push(json!({"sentence": sentence, "candidates": candidates.into_iter().take(per_sentence).map(|(_, c)| c).collect::<Vec<_>>()}));
    }
    Ok(json!({
        "sentences": out,
        "sentencesSkipped": all.len().saturating_sub(MAX_SENTENCES),
        "papersSearched": papers.len(),
        "papersWithoutText": papers.iter().filter(|p| !texts.contains_key(&p.id)).count(),
    }))
}

// ---- searching the literature databases

fn truth(args: &Value, key: &str, default: bool) -> Result<bool> {
    match &args[key] {
        Value::Null => Ok(default),
        Value::Bool(value) => Ok(*value),
        _ => bail!("参数 {key} 应为 true 或 false。"),
    }
}

/// The title to look a paper up by, and where it is from: the paper's own,
/// or, while that is still the name of its file, the one its PDF shows.
pub fn lookup_title(store: &Store, paper: &Paper) -> Result<(String, &'static str)> {
    let files = store.paper_files(&paper.id)?;
    if !complete::placeholder_title(paper, &files) { return Ok((paper.title.clone(), "record")) }
    let shown = files.iter().filter(|file| file.format == "pdf").find_map(|file| pdftext::title_guess(std::fs::read(store.file_path(file)).ok()?));
    Ok(shown.map_or((String::new(), "none"), |title| (title, "pdf")))
}

fn find_metadata(store: &mut Store, args: &Value, fetch: Fetch) -> Result<Value> {
    let paper = store.get_paper(text(args, "paperId")?)?;
    let files = store.paper_files(&paper.id)?;
    let (title, from) = match text(args, "title")? {
        "" => lookup_title(store, &paper)?,
        given => (given.to_string(), "given"),
    };
    if title.trim().is_empty() { bail!("「{}」的题目还是文件名，PDF 首页也没有读出题目。用 title 参数给出题目再查。", paper.title) }
    let year = match &args["year"] {
        Value::Null => None,
        value => Some(value.as_i64().filter(|y| (1800..=2200).contains(y)).ok_or_else(|| anyhow!("参数 year 应为年份，例如 2019。"))? as i32),
    };
    let query = complete::Query { title: title.trim().to_string(), author: text(args, "author")?.to_string(), year };
    let found = complete::find(&query, fetch);
    // No source could be asked: there is nothing to tell candidates from.
    if found.asked.iter().all(|asked| !asked.error.is_empty()) { bail!("{}", found.asked.iter().map(|asked| format!("{}：{}", asked.source, asked.error)).collect::<Vec<_>>().join("；")) }
    let placeholder = complete::placeholder_title(&paper, &files);
    let mut candidates = vec![];
    for candidate in &found.candidates {
        let record = &candidate.paper;
        let held = store.existing_paper(record)?.filter(|id| *id != paper.id);
        candidates.push(json!({
            "title": record.title, "authors": record.authors, "year": record.year, "journal": record.journal, "status": record.publication_status, "doi": record.doi, "pmid": record.pmid,
            "sources": candidate.sources, "similarity": (candidate.similarity * 100.).round() / 100., "matches": candidate.same, "differences": candidate.differs, "partialAuthors": candidate.partial_authors,
            "link": record.link(), "inLibrary": held, "wouldFill": if candidate.similarity >= complete::ALIKE_ENOUGH { complete::suggested(&paper, record, placeholder) } else { vec![] },
        }));
    }
    Ok(json!({
        "paper": {"id": paper.id, "title": paper.title, "titleIsFileName": placeholder},
        "query": {"title": query.title, "titleFrom": from, "author": query.author, "year": query.year},
        "sources": found.asked.iter().map(|asked| json!({"source": asked.source, "found": asked.found, "error": asked.error})).collect::<Vec<_>>(),
        "candidates": candidates,
        "note": "候选需要人核对：在窗口里打开这篇文献，用「按题目补全」选定记录和要写入的字段。",
    }))
}

fn search_online(store: &mut Store, args: &Value, fetch: Fetch) -> Result<Value> {
    let (kind, query) = (text(args, "source")?, text(args, "query")?);
    let found = sources::search(kind, query, number(args, "limit", 20, 100)?.max(1), fetch).map_err(|e| anyhow!(e))?;
    let mut known = 0;
    let mut results = vec![];
    for mut paper in found.papers {
        paper.normalize();
        // What the library already holds for this record, and what its owner decided about it.
        let existing = match store.existing_paper(&paper)? {
            Some(id) => {
                known += 1;
                let held = store.get_paper(&id)?;
                json!({"id": held.id, "screening": held.screening, "inTrash": store.list_trash()?.iter().any(|entry| entry.paper.id == held.id)})
            }
            None => Value::Null,
        };
        let summary: String = paper.abstract_text.chars().take(400).collect();
        results.push(json!({"title": paper.title, "authors": paper.short_authors(), "year": paper.year, "journal": paper.journal, "doi": paper.doi, "pmid": paper.pmid, "abstract": summary, "inLibrary": existing}));
    }
    Ok(json!({"source": kind, "query": query, "total": found.total, "fetched": results.len(), "new": results.len() - known, "inLibrary": known, "results": results}))
}

fn save_search(store: &mut Store, args: &Value) -> Result<Value> {
    let id = text(args, "id")?;
    let mut search = if id.is_empty() {
        if text(args, "source")?.is_empty() || text(args, "query")?.is_empty() { bail!("新建检索式需要 source 和 query。") }
        SavedSearch { max_results: 50, enabled: true, ..Default::default() }
    } else {
        store.list_searches()?.into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("检索式不存在：{id}"))?
    };
    if !args["source"].is_null() { search.kind = text(args, "source")?.into() }
    if !args["query"].is_null() { search.query = text(args, "query")?.into() }
    if !args["name"].is_null() { search.name = text(args, "name")?.into() }
    search.max_results = number(args, "maxResults", search.max_results, usize::MAX)?;
    search.enabled = truth(args, "enabled", search.enabled)?;
    store.save_search(&mut search)?;
    Ok(serde_json::to_value(&search)?)
}

/// Run a saved search and import what it brings back as one batch. A run
/// that fails is recorded too, so the search shows what went wrong.
fn run_search(store: &mut Store, id: &str, fetch: Fetch) -> Result<Value> {
    let search = store.list_searches()?.into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("检索式不存在：{id}"))?;
    let kind_name = sources::KINDS.iter().find(|(key, _)| *key == search.kind).map_or(search.kind.as_str(), |(_, name)| name);
    let label = format!("{kind_name}：{}", search.query);
    let found = match sources::search(&search.kind, &search.query, search.max_results, fetch) {
        Ok(found) => found,
        Err(error) => {
            store.record_failed_run(&search.id, &label, &error)?;
            bail!(error)
        }
    };
    let (batch, result) = store.import_batch(&search.id, &label, found.total, found.papers)?;
    let added: Vec<Value> = result.added.iter().take(50).map(|p| json!({"id": p.id, "title": p.title, "year": p.year})).collect();
    Ok(json!({"batchId": batch.id, "search": search.name, "total": batch.remote_total, "fetched": batch.fetched, "added": batch.added, "skipped": batch.skipped, "addedPapers": added}))
}

fn undo_import(store: &mut Store, args: &Value) -> Result<Value> {
    let undone = store.undo_batch(text(args, "batchId")?)?;
    Ok(json!({"removed": undone.removed, "kept": undone.kept.iter().map(|(id, title)| json!({"id": id, "title": title})).collect::<Vec<_>>()}))
}

// ---- scheduled updates

const HOUR_MS: i64 = 3_600_000;

/// The searches whose turn it is: enabled, and never run or last run at
/// least `hours` ago. One that failed is tried again after an hour at most.
pub fn due_searches(searches: &[SavedSearch], hours: u32, now_ms: i64) -> Vec<&SavedSearch> {
    if hours == 0 { return vec![] }
    searches
        .iter()
        .filter(|search| search.enabled)
        .filter(|search| match &search.last_run {
            None => true,
            Some(run) => {
                let wait = if run.error.is_empty() { hours as i64 } else { (hours as i64).min(1) };
                now_ms - crate::time::millis(&run.started_at) >= wait * HOUR_MS
            }
        })
        .collect()
}

/// Run what is due in every library that updates on its own (or in the one
/// named), or everything enabled when forced.
fn sync_searches(root: &Path, args: &Value, fetch: Fetch, now_ms: i64) -> Result<Value> {
    let force = truth(args, "force", false)?;
    let wanted = text(args, "library")?;
    let all = if wanted.is_empty() { libraries::list(root) } else { vec![find_library(root, wanted).map_err(|e| anyhow!(e))?] };
    let mut runs = vec![];
    let (mut added, mut failed) = (0, 0);
    for info in all.iter().filter(|info| force || info.auto_sync_hours > 0) {
        let mut store = Store::open(&info.dir)?;
        let searches = store.list_searches()?;
        let due: Vec<String> = if force { searches.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect() } else { due_searches(&searches, info.auto_sync_hours, now_ms).iter().map(|s| s.id.clone()).collect() };
        for id in due {
            // One search failing does not stop the others.
            match run_search(&mut store, &id, fetch) {
                Ok(run) => {
                    added += run["added"].as_u64().unwrap_or(0);
                    runs.push(json!({"library": info.name, "libraryId": info.id, "search": run["search"], "added": run["added"], "skipped": run["skipped"], "batchId": run["batchId"]}));
                }
                Err(error) => {
                    failed += 1;
                    runs.push(json!({"library": info.name, "libraryId": info.id, "searchId": id, "error": format!("{error:#}")}));
                }
            }
        }
    }
    Ok(json!({"runs": runs, "added": added, "failed": failed}))
}

// ---- screening

/// Record what an agent thinks the screening decision should be. The
/// decision itself is the person's and is not touched here; a quotation the
/// paper's text does not hold is not kept as evidence.
fn suggest_screening(store: &mut Store, info: &LibraryInfo, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "id")?)?;
    extract_missing(store, Some(&paper.id))?;
    let pages = store.paper_pages(&paper.id)?;
    let quote = text(args, "excerpt")?;
    let found = if quote.is_empty() { None } else { crate::ai::locate_quote(quote, &paper, pages.as_deref()) };
    let confidence = match &args["confidence"] {
        Value::Null => None,
        value => Some(value.as_f64().ok_or_else(|| anyhow!("参数 confidence 应为 0 到 1 之间的数字。"))?),
    };
    let author = text(args, "author")?;
    let mut suggestion = Suggestion {
        paper_id: paper.id.clone(),
        verdict: text(args, "verdict")?.into(),
        reason: text(args, "reason")?.into(),
        excerpt: if found.is_some() { quote.into() } else { String::new() },
        page: found.flatten(),
        basis: if pages.is_some() { "fulltext" } else if paper.abstract_text.is_empty() { "none" } else { "abstract" }.into(),
        confidence,
        author: if author.is_empty() { "agent".into() } else { author.into() },
        // An agent is told the goal by library_status; its suggestion is taken as made for it.
        criteria: Some(info.criteria.clone()),
        ..Default::default()
    };
    store.save_suggestion(&mut suggestion)?;
    Ok(json!({
        "saved": true,
        "paperId": paper.id,
        "excerptKept": quote.is_empty() || found.is_some(),
        "page": suggestion.page,
        // Said every time: an agent that reads this knows the paper is still undecided.
        "screening": paper.screening,
        "note": "建议已保存，文献的收录判断没有改变，由用户在筛选页确认。",
    }))
}

// ---- the command line

/// `paperId` as a flag: `paper-id`.
fn flag_name(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A tool's arguments from command-line words: `--paper-id P-1 --limit 5`,
/// `--limit=5`, `-l 名称` for `--library`; a bare word fills the next
/// required argument, so `get_paper P-1` works.
pub fn cli_args(schema: &Value, words: &[String]) -> Result<Value, String> {
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let mut required = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str);
    let mut out = Map::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let (key, mut value) = if word == "-l" {
            ("library".to_string(), None)
        } else if let Some(flag) = word.strip_prefix("--") {
            let (flag, value) = flag.split_once('=').map_or((flag, None), |(f, v)| (f, Some(v.to_string())));
            let Some(key) = properties.keys().find(|key| flag_name(key) == flag || key.as_str() == flag) else {
                return Err(format!("没有参数 --{flag}。可用：{}", properties.keys().map(|k| format!("--{}", flag_name(k))).collect::<Vec<_>>().join(" ")));
            };
            (key.clone(), value)
        } else {
            let Some(key) = required.find(|key| !out.contains_key(*key)) else { return Err(format!("多余的参数：{word}")) };
            (key.to_string(), Some(word.clone()))
        };
        let kind = properties.get(&key).and_then(|p| p["type"].as_str()).unwrap_or("string");
        if kind == "boolean" && value.is_none() { value = Some("true".into()) }
        let Some(value) = value.or_else(|| words.next().cloned()) else { return Err(format!("--{} 后面需要一个值。", flag_name(&key))) };
        let parsed = match kind {
            "integer" => json!(value.parse::<i64>().map_err(|_| format!("--{} 应为整数：{value}", flag_name(&key)))?),
            "number" => json!(value.parse::<f64>().map_err(|_| format!("--{} 应为数字：{value}", flag_name(&key)))?),
            "boolean" => json!(value == "true"),
            _ => json!(value),
        };
        out.insert(key, parsed);
    }
    Ok(Value::Object(out))
}

/// What `wisp <tool> --help` prints.
pub fn usage(tool: &ToolDef) -> String {
    let required: Vec<&str> = tool.input_schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut out = format!("wisp {}{}\n\n{}\n", tool.name, required.iter().map(|r| format!(" <{}>", flag_name(r))).collect::<String>(), tool.description);
    for (key, property) in tool.input_schema["properties"].as_object().into_iter().flatten() {
        let mut line = format!("\n  --{} {}", flag_name(key), property["description"].as_str().unwrap_or(""));
        if let Some(values) = property["enum"].as_array() { line.push_str(&format!("（{}）", values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | "))) }
        out.push_str(&line);
    }
    out.push('\n');
    out
}

#[cfg(test)]
pub(crate) mod tests {
    /// No tool in these tests may reach the network.
    pub(crate) fn offline(url: &str) -> Result<String, String> {
        Err(format!("测试中不应联网：{url}"))
    }

    pub(crate) fn no_download(url: &str) -> Result<Vec<u8>, String> {
        Err(format!("测试中不应联网：{url}"))
    }

    fn call(root: &Path, name: &str, args: &Value) -> Result<Value, String> {
        super::call(root, name, args, &offline, &no_download)
    }

    use super::*;
    use crate::model::{FieldDef, new_id};
    use std::path::PathBuf;

    /// A data folder with one library: two papers, the first with a two-page text.
    pub(crate) fn sample() -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("wisp-tools-{}", new_id("T")));
        let info = libraries::create(&root, "根", "根尖发育", "green").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let mut species = FieldDef { name: "物种".into(), ..Default::default() };
        store.save_field(&mut species).unwrap();
        let mut atlas = Paper { title: "Root stem cell niche".into(), doi: "10.1000/root".into(), authors: "Zhang Wei; Li Na; Wang Fang".into(), year: Some(2021), abstract_text: "WOX5 maintains the quiescent centre.".into(), tags: vec!["atlas".into()], ..Default::default() };
        atlas.fields.insert(species.id.clone(), vec!["Arabidopsis".into()]);
        let atlas = store.add_paper(atlas).unwrap();
        let mut leaf = store.add_paper(Paper { title: "Leaf development".into(), year: Some(2019), ..Default::default() }).unwrap();
        leaf.decide("irrelevant", "");
        store.save_paper(&mut leaf).unwrap();
        // A text file is one page; the two pages a PDF would give go straight into the cache.
        let source = root.join("root.txt");
        std::fs::write(&source, "placeholder").unwrap();
        let file = store.import_file(&atlas.id, &source, "fulltext").unwrap();
        store.save_fulltext_pages(&file.id, &Ok(vec!["Introduction.\nThe root tip of Arabidopsis.".into(), "Libraries were prepared with\n10x Genomics Chromium. WOX5 marks the quiescent centre.".into()])).unwrap();
        (root, atlas.id)
    }

    #[test]
    fn libraries_and_status() {
        let (root, _) = sample();
        let listed = call(&root, "list_libraries", &json!({})).unwrap();
        assert_eq!((listed["libraries"][0]["name"].as_str(), listed["libraries"][0]["papers"].as_u64()), (Some("根"), Some(2)));
        // The only library needs no name; by name or id it is the same one.
        let status = call(&root, "library_status", &json!({})).unwrap();
        assert_eq!(call(&root, "library_status", &json!({"library": "根"})).unwrap(), status);
        assert_eq!((status["papers"].as_u64(), status["screening"]["irrelevant"].as_u64(), status["fulltext"]["stored"].as_u64()), (Some(2), Some(1), Some(1)));
        assert_eq!((status["labels"]["screening"]["excluded"].as_str(), status["fields"][0]["name"].as_str()), (Some("相关"), Some("物种")));

        assert!(call(&root, "library_status", &json!({"library": "叶"})).unwrap_err().contains("「根」"));
        libraries::create(&root, "叶", "", "blue").unwrap();
        assert!(call(&root, "library_status", &json!({})).unwrap_err().contains("多个文献库"));
        assert_eq!(call(&root, "library_status", &json!({"library": "叶"})).unwrap()["papers"], 0);
        assert!(find_library(&root.join("nowhere"), "").unwrap_err().contains("还没有文献库"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn papers() {
        let (root, id) = sample();
        // Like the window's list: the paper marked irrelevant shows only when asked for.
        let all = call(&root, "list_papers", &json!({})).unwrap();
        assert_eq!((all["total"].as_u64(), all["papers"][0]["authors"].as_str()), (Some(1), Some("Zhang Wei 等")));
        assert_eq!(call(&root, "list_papers", &json!({"screening": "irrelevant"})).unwrap()["papers"][0]["title"], "Leaf development");
        assert_eq!(call(&root, "list_papers", &json!({"query": "wox5 niche", "year": 2021})).unwrap()["total"], 1);
        assert_eq!(call(&root, "list_papers", &json!({"query": "wox5", "year": 2019})).unwrap()["total"], 0);
        let paged = call(&root, "list_papers", &json!({"limit": 1, "offset": 1})).unwrap();
        assert_eq!((paged["total"].as_u64(), paged["papers"].as_array().map(Vec::len)), (Some(1), Some(0)));

        let paper = call(&root, "get_paper", &json!({"id": id})).unwrap();
        assert_eq!((paper["doi"].as_str(), paper["fields"]["物种"][0].as_str(), paper["link"].as_str()), (Some("10.1000/root"), Some("Arabidopsis"), Some("https://doi.org/10.1000/root")));
        assert_eq!((paper["files"][0]["title"].as_str(), paper["files"][0]["textPages"].as_u64()), (Some("root.txt"), Some(2)));
        assert!(Path::new(paper["files"][0]["path"].as_str().unwrap()).exists());

        for (name, args, message) in [
            ("get_paper", json!({"id": "P-none"}), "文献不存在"),
            ("get_paper", json!({}), "需要参数 id"),
            ("list_papers", json!({"screening": "maybe"}), "pending"),
            ("list_papers", json!({"limit": "ten"}), "非负整数"),
            ("list_papers", json!({"colour": "red"}), "没有参数 colour"),
            ("list_papers", json!([]), "JSON 对象"),
            ("drop_everything", json!({}), "没有这个工具"),
        ] {
            let error = call(&root, name, &args).unwrap_err();
            assert!(error.contains(message), "{name}: {error}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn sentences_for_writing() {
        let root = std::env::temp_dir().join(format!("wisp-tools-{}", new_id("T")));
        let info = libraries::create(&root, "根", "根尖发育", "green").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let paper = |title: &str, authors: &str, abstract_text: &str| Paper { title: title.into(), authors: authors.into(), year: Some(2021), abstract_text: abstract_text.into(), ..Default::default() };
        let soil = store.add_paper(Paper { tags: vec!["soil".into()], starred: true, ..paper("Roots in compacted soil", "Zhu, Mingyuan; Hsu, Che-Wei; Pandey, Bipin", "Roots adapt to compacted soil by remodelling their cell walls in outer tissues. We profiled rice roots at single-cell resolution.") }).unwrap();
        let atlas = store.add_paper(paper("A root atlas", "Zhang, Tian-Qi; Wang, Jia-Wei", "")).unwrap();
        let text = |intro: &str, results: &str| format!("Introduction\n\n{intro}\n\nResults\n\nA heading of the paper\n\n{results}\n\nReferences\n\n1. Someone, A. A paper that is cited here and nowhere else. Journal 1, 1 (2020).");
        for (id, name, body) in [
            (&soil.id, "soil.txt", text("Soil compaction reduces root penetration and thereby limits the uptake of water. However, how roots respond to compacted soil at the cellular level remains poorly understood.", "We profiled more than 47,000 root cells grown in compacted soil. Cell wall genes were induced in the outer tissues of roots in compacted soil.")),
            (&atlas.id, "atlas.txt", text("Roots grow continuously because of the stem cells at their tips. Single-cell RNA sequencing has transformed the study of root development in recent years.", "About 15,000 cells were profiled from the root tips of seedlings.")),
        ] {
            let source = root.join(name);
            std::fs::write(&source, body).unwrap();
            store.import_file(id, &source, "fulltext").unwrap();
        }
        drop(store);
        // No question: how each paper opens the part.
        let openers = call(&root, "find_sentences", &json!({"section": "introduction"})).unwrap();
        assert_eq!((openers["sentencesInSection"].as_u64(), openers["papersWithSection"].as_u64(), openers["papersWithout"].as_u64()), (Some(4), Some(2), Some(0)));
        assert_eq!(openers["sentences"].as_array().unwrap().iter().map(|s| s["position"].as_u64().unwrap()).collect::<Vec<_>>(), [0, 0, 1, 1]);
        // A question: the sentences that say it, with where they are from.
        let found = call(&root, "find_sentences", &json!({"section": "introduction", "query": "how roots respond to compaction is poorly understood", "limit": 1})).unwrap();
        let best = &found["sentences"][0];
        assert_eq!((best["paperId"].as_str(), best["cite"].as_str(), best["page"].as_u64()), (Some(soil.id.as_str()), Some("Zhu et al., 2021"), Some(1)));
        assert!(best["text"].as_str().unwrap().starts_with("However, how roots respond"));
        // With its context: the sentence before it in the same part, and none after (it closes the introduction).
        let around = call(&root, "find_sentences", &json!({"section": "introduction", "query": "how roots respond to compaction is poorly understood", "limit": 1, "context": true})).unwrap();
        assert!(around["sentences"][0]["before"].as_str().unwrap().starts_with("Soil compaction reduces") && around["sentences"][0]["after"].is_null());
        assert!(best["before"].is_null());
        // Several groups of words, merged: each finds its own paper.
        let both = call(&root, "find_sentences", &json!({"section": "introduction", "queries": ["compacted soil", "stem cells tips"]})).unwrap();
        assert_eq!((both["queries"].as_array().unwrap().len(), both["query"].as_str()), (2, Some("compacted soil")));
        let papers: std::collections::BTreeSet<&str> = both["sentences"].as_array().unwrap().iter().map(|s| s["paperId"].as_str().unwrap()).collect();
        assert_eq!(papers.len(), 2);
        assert!(call(&root, "find_sentences", &json!({"section": "introduction", "queries": "soil"})).is_err());
        // Narrowed to a tag, or to the starred papers: only the soil paper is either.
        for narrow in [json!({"section": "introduction", "tag": "soil"}), json!({"section": "introduction", "starred": true})] {
            let found = call(&root, "find_sentences", &narrow).unwrap();
            assert!(found["sentences"].as_array().unwrap().iter().all(|s| s["paperId"] == soil.id.as_str()) && found["papersWithSection"] == 1, "{narrow}");
        }
        // The same words in another part are another matter; the paper's own heading comes along.
        let results = call(&root, "find_sentences", &json!({"section": "results", "query": "compacted soil", "paperId": soil.id})).unwrap();
        assert_eq!((results["sentences"].as_array().unwrap().len(), results["sentences"][0]["heading"].as_str()), (2, Some("A heading of the paper")));
        assert_eq!(call(&root, "find_sentences", &json!({"section": "methods"})).unwrap()["sentences"], json!([]));
        // The abstract is the record's, with or without a stored text.
        let abstracts = call(&root, "find_sentences", &json!({"section": "abstract", "query": "cell walls"})).unwrap();
        assert_eq!((abstracts["sentences"][0]["paperId"].as_str(), abstracts["sentences"][0].get("page"), abstracts["papersWithout"].as_u64()), (Some(soil.id.as_str()), None, Some(1)));
        assert!(call(&root, "find_sentences", &json!({"section": "preface"})).is_err());
        // Asked for one language, sentences of another are left out and counted; a text too short to tell stays.
        let english = call(&root, "find_sentences", &json!({"section": "introduction", "language": "en"})).unwrap();
        assert_eq!(english["sentencesInOtherLanguages"].as_u64().unwrap() + english["sentencesInSection"].as_u64().unwrap(), openers["sentencesInSection"].as_u64().unwrap());
        let chinese = call(&root, "find_sentences", &json!({"section": "introduction", "language": "zh"})).unwrap();
        assert!(chinese["sentences"].as_array().unwrap().iter().all(|s| !s["text"].as_str().unwrap().is_ascii()) || chinese["sentencesInOtherLanguages"].as_u64() == Some(0));
        assert!(call(&root, "find_sentences", &json!({"section": "introduction", "language": "xx"})).is_err());
        assert_eq!(describe_call("find_sentences", &json!({"section": "introduction", "query": "soil"})), "查找候选句：soil · introduction");
    }

    #[test]
    fn fulltext() {
        let (root, id) = sample();
        // The phrase runs over a line break and differs in case.
        let found = call(&root, "search_fulltext", &json!({"query": "prepared with 10X genomics"})).unwrap();
        let hit = &found["hits"][0];
        assert_eq!((found["total"].as_u64(), hit["page"].as_u64(), hit["exact"].as_bool(), hit["paperId"].as_str()), (Some(1), Some(2), Some(true), Some(id.as_str())));
        assert!(hit["snippet"].as_str().unwrap().contains("prepared with 10x Genomics Chromium."));
        assert_eq!((found["papersSearched"].as_u64(), found["papersWithoutText"].as_u64()), (Some(1), Some(1)));
        // Scattered words: each page holds one of the two. One word out of four is not a hit.
        let loose = call(&root, "search_fulltext", &json!({"query": "WOX5 Arabidopsis"})).unwrap();
        assert_eq!((loose["total"].as_u64(), loose["hits"][0]["exact"].as_bool()), (Some(2), Some(false)));
        assert_eq!(call(&root, "search_fulltext", &json!({"query": "wox5 auxin gradient cytokinin"})).unwrap()["total"], 0);
        assert_eq!(call(&root, "search_fulltext", &json!({"query": "chromium", "paperId": "P-other"})).unwrap()["total"], 0);
        assert!(call(&root, "search_fulltext", &json!({"query": "  "})).is_err());

        let read = call(&root, "read_fulltext", &json!({"paperId": id, "page": 2})).unwrap();
        assert_eq!((read["pageCount"].as_u64(), read["pages"].as_array().map(Vec::len), read["pages"][0]["page"].as_u64()), (Some(2), Some(1), Some(2)));
        assert!(read["pages"][0]["text"].as_str().unwrap().starts_with("Libraries were prepared"));
        assert!(call(&root, "read_fulltext", &json!({"paperId": id, "page": 3})).unwrap_err().contains("只有 2 页"));
        let leaf = call(&root, "list_papers", &json!({"screening": "irrelevant"})).unwrap()["papers"][0]["id"].as_str().unwrap().to_string();
        assert!(call(&root, "read_fulltext", &json!({"paperId": leaf})).unwrap_err().contains("没有保存全文"));

        // A PDF neither reader makes sense of: the paper says so in its status, and a
        // search names it as not seen into, without guessing at what the file is.
        let broken = root.join("leaf.pdf");
        std::fs::write(&broken, "not a pdf").unwrap();
        Store::open(&libraries::list(&root)[0].dir).unwrap().import_file(&leaf, &broken, "fulltext").unwrap();
        let unread = call(&root, "read_fulltext", &json!({"paperId": leaf})).unwrap_err();
        assert!(unread.contains("文字没有读出来") && unread.contains("不要据此推测") && !unread.contains("扫描件。"), "{unread}");
        let found = call(&root, "search_fulltext", &json!({"query": "chromium"})).unwrap();
        assert_eq!((found["total"].as_u64(), found["unreadableCount"].as_u64(), found["unreadable"][0]["paperId"].as_str(), found["unreadable"][0]["kind"].as_str()), (Some(1), Some(1), Some(leaf.as_str()), Some("parser")));
        assert!(found["note"].as_str().unwrap().contains("没有命中不说明"));
        assert_eq!(call(&root, "search_fulltext", &json!({"query": "chromium", "paperId": id})).unwrap().get("unreadable"), None);
        assert_eq!(call(&root, "list_papers", &json!({"fulltext": "textless", "screening": "irrelevant"})).unwrap()["papers"][0]["id"].as_str(), Some(leaf.as_str()));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn open_full_texts_are_fetched() {
        let (root, id) = sample();
        // The indexes, as they answer for the two papers; the publisher hands a PDF out, a login page elsewhere.
        let index = |url: &str| -> Result<String, String> {
            if url.contains("openalex") && url.contains("root") {
                Ok(json!({"open_access": {"is_oa": true}, "best_oa_location": {"is_oa": true, "pdf_url": "https://repo.example/login.pdf", "version": "acceptedVersion"}, "locations": []}).to_string())
            } else if url.contains("openalex") {
                Ok(json!({"open_access": {"is_oa": false}, "best_oa_location": null, "locations": []}).to_string())
            } else {
                Ok(json!({"resultList": {"result": []}}).to_string())
            }
        };
        let files = |url: &str| -> Result<Vec<u8>, String> { if url.contains("repo.example") { Ok(b"<html>Sign in</html>".to_vec()) } else { Ok(b"%PDF-1.4 not really a pdf".to_vec()) } };
        let find = |args: Value| super::call(&root, "find_pdf", &args, &index, &files);
        // A paper that has its PDF is not looked for again.
        assert!(find(json!({"paperId": id})).unwrap_err().contains("已经有 PDF"));
        // The paper without a number cannot be looked up; said, not skipped in silence.
        let leaf = call(&root, "list_papers", &json!({"screening": "irrelevant"})).unwrap()["papers"][0]["id"].as_str().unwrap().to_string();
        assert!(find(json!({"paperId": leaf})).unwrap_err().contains("无从查起"));
        let all = find(json!({})).unwrap();
        assert_eq!((all["checked"].as_u64(), all["withoutIdentifier"].as_u64()), (Some(0), Some(1)));
        // A third paper: open access at a publisher this knows, a manuscript behind a login elsewhere.
        let mut store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let soil = store.add_paper(Paper { title: "Soil and root".into(), authors: "Zhu, Mo; Lee, An".into(), year: Some(2021), doi: "10.1186/root-soil-1".into(), ..Default::default() }).unwrap();
        let closed = store.add_paper(Paper { title: "Behind a wall".into(), doi: "10.1016/closed".into(), ..Default::default() }).unwrap();
        drop(store);
        let looked = find(json!({"paperId": soil.id, "lookOnly": true})).unwrap();
        assert_eq!(looked["papers"][0]["candidates"].as_array().unwrap().iter().map(|c| c["source"].as_str().unwrap()).collect::<Vec<_>>(), ["Springer", "OpenAlex"]);
        assert_eq!(call(&root, "get_paper", &json!({"id": soil.id})).unwrap()["fulltext"], "linked");
        let found = find(json!({})).unwrap();
        assert_eq!((found["checked"].as_u64(), found["saved"].as_u64(), found["notFound"].as_u64(), found["failed"].as_u64()), (Some(2), Some(1), Some(1), Some(0)));
        let saved = found["papers"].as_array().unwrap().iter().find(|row| row["paperId"] == soil.id.as_str()).unwrap();
        // Kept under the paper, named after it, with where it came from; its text could not be read, and that is said.
        assert_eq!((saved["outcome"].as_str(), saved["source"].as_str(), saved["version"].as_str(), saved["textKind"].as_str()), (Some("saved"), Some("Springer"), Some("published"), Some("parser")));
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let file = store.paper_files(&soil.id).unwrap().remove(0);
        assert_eq!((file.title.as_str(), file.kind.as_str()), ("Zhu and Lee, 2021.pdf", "fulltext"));
        assert_eq!(store.file_origins().unwrap()[&file.id], ("https://link.springer.com/content/pdf/10.1186/root-soil-1.pdf".to_string(), "Springer".to_string(), "published".to_string()));
        assert!(found["papers"].as_array().unwrap().iter().any(|row| row["paperId"] == closed.id.as_str() && row["outcome"] == "not_found"));
        // What stands in for a PDF is not kept as one.
        let walled = |_: &str| -> Result<Vec<u8>, String> { Ok(b"<html>Sign in</html>".to_vec()) };
        let again = store.paper_files(&closed.id).unwrap().len();
        drop(store);
        let mut store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let third = store.add_paper(Paper { title: "Another".into(), doi: "10.1186/root-soil-2".into(), ..Default::default() }).unwrap();
        drop(store);
        let open = |_: &str| -> Result<String, String> { Ok(json!({"open_access": {"is_oa": true}, "best_oa_location": null, "locations": [], "resultList": {"result": []}}).to_string()) };
        let failed = super::call(&root, "find_pdf", &json!({"paperId": third.id}), &open, &walled).unwrap();
        assert_eq!((failed["failed"].as_u64(), again), (Some(1), 0));
        assert!(failed["papers"][0]["reason"].as_str().unwrap().contains("返回的不是 PDF"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn citations_for_a_text() {
        let (root, id) = sample();
        assert_eq!(sentences("Roots grow (p < 0.05). 根尖有干细胞。WOX5 is key;\nDone"), ["Roots grow (p < 0.05).", "根尖有干细胞。", "WOX5 is key;", "Done"]);
        let found = call(&root, "find_citations", &json!({"text": "Libraries were prepared with the Chromium platform. WOX5 maintains the quiescent centre of the root. Too short."})).unwrap();
        let sentence = |n: usize| &found["sentences"][n];
        assert_eq!(found["sentences"].as_array().unwrap().len(), 3);
        // The methods sentence is only in the full text, on page 2; the second is the abstract.
        assert_eq!((sentence(0)["candidates"][0]["paperId"].as_str(), sentence(0)["candidates"][0]["in"].as_str(), sentence(0)["candidates"][0]["page"].as_u64()), (Some(id.as_str()), Some("fulltext"), Some(2)));
        assert!(sentence(0)["candidates"][0]["snippet"].as_str().unwrap().contains("Libraries were prepared"));
        assert_eq!((sentence(1)["candidates"][0]["in"].as_str(), sentence(1)["candidates"][0]["page"].as_u64()), (Some("record"), None));
        assert_eq!(sentence(2)["candidates"], json!([]));
        // The paper marked irrelevant is never a candidate.
        assert_eq!((found["papersSearched"].as_u64(), found["papersWithoutText"].as_u64()), (Some(1), Some(0)));
        assert_eq!(call(&root, "find_citations", &json!({"text": "Unrelated sentence about volcanic eruptions in Iceland."})).unwrap()["sentences"][0]["candidates"], json!([]));
        std::fs::remove_dir_all(root).ok();
    }

    /// A PubMed that knows two records, one of which the sample library holds by title.
    #[test]
    fn records_by_title() {
        let (root, atlas) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        // A PDF taken in without a DOI: its title is the name of its file.
        let bare = store.add_paper(Paper { title: "zhang2019".into(), source: "PDF 导入".into(), ..Default::default() }).unwrap();
        let source = root.join("zhang2019.txt");
        std::fs::write(&source, "text").unwrap();
        store.import_file(&bare.id, &source, "fulltext").unwrap();
        let records = |url: &str| -> Result<String, String> {
            if url.contains("crossref") {
                Ok(r#"{"message":{"items":[{"DOI":"10.1000/ROOT","title":["Root stem-cell niche"],"author":[{"family":"Zhang","given":"Wei"}],"issued":{"date-parts":[[2021]]},"container-title":["Plant Cell"],"type":"journal-article"},{"DOI":"10.1000/other","title":["Root stem cell niches of rice"],"type":"journal-article"}]}}"#.into())
            } else if url.contains("arxiv") {
                Ok("<feed></feed>".into())
            } else {
                Err("连接超时".into())
            }
        };
        let online = |args: Value| super::call(&root, "find_metadata", &args, &records, &no_download);
        // Nothing to look for: the file name is not a title, and a text file shows none.
        assert!(online(json!({"paperId": bare.id})).unwrap_err().contains("题目还是文件名"));
        let found = online(json!({"paperId": bare.id, "title": "Root stem cell niche", "year": 2021})).unwrap();
        assert_eq!((found["query"]["titleFrom"].as_str(), found["paper"]["titleIsFileName"].as_bool()), (Some("given"), Some(true)));
        assert_eq!((found["sources"][0]["found"].as_u64(), found["sources"][1]["error"].as_str()), (Some(2), Some("连接超时")));
        // The record the library already holds as another paper is said to be that; nothing is written.
        let first = &found["candidates"][0];
        assert_eq!((first["doi"].as_str(), first["similarity"].as_f64(), first["inLibrary"].as_str(), first["matches"][1].as_str()), (Some("10.1000/root"), Some(1.0), Some(atlas.as_str()), Some("年份 2021")));
        assert_eq!(first["wouldFill"], json!(["title", "authors", "year", "journal", "status", "doi"]));
        assert_eq!((found["candidates"][1]["inLibrary"].clone(), found["candidates"].as_array().unwrap().len()), (Value::Null, 2));
        assert_eq!(store.get_paper(&bare.id).unwrap().title, "zhang2019");
        // A paper with a title of its own is looked up by it.
        assert_eq!(online(json!({"paperId": atlas})).unwrap()["query"]["titleFrom"], "record");
        assert!(online(json!({"paperId": atlas, "year": 12})).unwrap_err().contains("year"));
        assert!(call(&root, "find_metadata", &json!({"paperId": atlas})).unwrap_err().contains("不应联网"));
        std::fs::remove_dir_all(root).ok();
    }

    fn pubmed(url: &str) -> Result<String, String> {
        if url.contains("esearch") { return Ok(r#"{"esearchresult":{"count":"88","idlist":["1","2"]}}"#.into()) }
        Ok("<PubmedArticleSet>\
<PubmedArticle><MedlineCitation><PMID>1</PMID><Article><Journal><JournalIssue><PubDate><Year>2024</Year></PubDate></JournalIssue><Title>Plant Cell</Title></Journal><ArticleTitle>Root stem cell niche.</ArticleTitle></Article></MedlineCitation></PubmedArticle>\
<PubmedArticle><MedlineCitation><PMID>2</PMID><Article><Journal><JournalIssue><PubDate><Year>2025</Year></PubDate></JournalIssue><Title>Nature Plants</Title></Journal><ArticleTitle>A new root atlas</ArticleTitle><Abstract><AbstractText>Cells of the root.</AbstractText></Abstract></Article></MedlineCitation></PubmedArticle>\
</PubmedArticleSet>"
            .into())
    }

    #[test]
    fn searches_from_trial_to_undo() {
        let (root, id) = sample();
        let online = |name: &str, args: Value| super::call(&root, name, &args, &pubmed, &no_download);
        // A trial writes nothing and says what the library already holds.
        let trial = online("search_online", json!({"source": "pubmed", "query": "root[Title]", "limit": 5})).unwrap();
        assert_eq!((trial["total"].as_u64(), trial["fetched"].as_u64(), trial["new"].as_u64(), trial["inLibrary"].as_u64()), (Some(88), Some(2), Some(1), Some(1)));
        assert_eq!((trial["results"][0]["inLibrary"]["id"].as_str(), trial["results"][0]["inLibrary"]["screening"].as_str()), (Some(id.as_str()), Some("pending")));
        assert_eq!((trial["results"][1]["inLibrary"].clone(), trial["results"][1]["abstract"].as_str()), (Value::Null, Some("Cells of the root.")));
        assert_eq!(call(&root, "list_papers", &json!({})).unwrap()["total"], 1);
        assert!(call(&root, "search_online", &json!({"source": "pubmed", "query": "x"})).unwrap_err().contains("不应联网"));

        assert!(online("save_search", json!({"query": "x"})).unwrap_err().contains("source 和 query"));
        let saved = online("save_search", json!({"source": "pubmed", "query": "root[Title]", "maxResults": 10})).unwrap();
        let search = saved["id"].as_str().unwrap().to_string();
        assert_eq!((saved["name"].as_str(), saved["maxResults"].as_u64(), saved["enabled"].as_bool()), (Some("PubMed"), Some(10), Some(true)));
        // Changing one thing leaves the rest.
        let renamed = online("save_search", json!({"id": search, "name": "根", "enabled": false})).unwrap();
        assert_eq!((renamed["name"].as_str(), renamed["query"].as_str(), renamed["enabled"].as_bool()), (Some("根"), Some("root[Title]"), Some(false)));

        let run = online("run_search", json!({"id": search})).unwrap();
        assert_eq!((run["total"].as_u64(), run["fetched"].as_u64(), run["added"].as_u64(), run["skipped"].as_u64()), (Some(88), Some(2), Some(1), Some(1)));
        assert_eq!(run["addedPapers"][0]["title"], "A new root atlas");
        let added = call(&root, "list_papers", &json!({"query": "atlas"})).unwrap();
        assert_eq!((added["papers"][0]["screening"].as_str(), added["papers"][0]["pmid"].as_str()), (Some("pending"), Some("2")));
        // A failed run is on the record of the search.
        assert!(call(&root, "run_search", &json!({"id": search})).unwrap_err().contains("不应联网"));
        let listed = call(&root, "list_searches", &json!({})).unwrap();
        assert!(listed["searches"][0]["lastRun"]["error"].as_str().unwrap().contains("不应联网"));
        let imports = call(&root, "list_imports", &json!({})).unwrap();
        assert_eq!((imports["imports"].as_array().unwrap().len(), imports["imports"][1]["label"].as_str()), (2, Some("PubMed：root[Title]")));

        let undone = call(&root, "undo_import", &json!({"batchId": run["batchId"]})).unwrap();
        assert_eq!((undone["removed"].as_u64(), undone["kept"].as_array().map(Vec::len)), (Some(1), Some(0)));
        assert_eq!(call(&root, "list_papers", &json!({})).unwrap()["total"], 1);
        assert_eq!(call(&root, "remove_search", &json!({"id": search})).unwrap()["removed"], true);
        // Exactly the tools on the list are marked as writing.
        let (mut marked, mut listed): (Vec<&str>, Vec<&str>) = (catalog().iter().filter(|t| t.writes).map(|t| t.name).collect(), WRITING.to_vec());
        marked.sort();
        listed.sort();
        assert_eq!(marked, listed);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn updates_run_what_is_due() {
        use crate::store::ImportBatch;
        let hour = HOUR_MS;
        let now = crate::time::millis("2026-11-11T12:00:00.000Z");
        let search = |name: &str, enabled: bool, ran: Option<(&str, &str)>| SavedSearch {
            name: name.into(),
            enabled,
            last_run: ran.map(|(at, error)| ImportBatch { started_at: at.into(), error: error.into(), ..Default::default() }),
            ..Default::default()
        };
        let searches = [
            search("never", true, None),
            search("off", false, None),
            search("fresh", true, Some(("2026-11-11T06:00:00.000Z", ""))),
            search("old", true, Some(("2026-11-10T11:00:00.000Z", ""))),
            search("failed lately", true, Some(("2026-11-11T11:30:00.000Z", "x"))),
            search("failed a while ago", true, Some(("2026-11-11T10:30:00.000Z", "x"))),
        ];
        let names = |hours: u32| due_searches(&searches, hours, now).iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(24), ["never", "old", "failed a while ago"]);
        assert_eq!(names(6), ["never", "fresh", "old", "failed a while ago"]);
        assert!(names(0).is_empty());
        assert_eq!(now - crate::time::millis("2026-11-11T06:00:00.000Z"), 6 * hour);

        // In a data folder: only libraries that update on their own, unless forced.
        let (root, _) = sample();
        let online = |args: Value| super::call(&root, "sync_searches", &args, &pubmed, &no_download);
        super::call(&root, "save_search", &json!({"source": "pubmed", "query": "root[Title]"}), &pubmed, &no_download).unwrap();
        assert_eq!(online(json!({})).unwrap(), json!({"runs": [], "added": 0, "failed": 0}));
        let mut info = find_library(&root, "").unwrap();
        info.auto_sync_hours = 24;
        libraries::save(&info).unwrap();
        let first = online(json!({})).unwrap();
        assert_eq!((first["added"].as_u64(), first["runs"][0]["library"].as_str(), first["runs"][0]["added"].as_u64()), (Some(1), Some("根"), Some(1)));
        // Just run: not due again. Forced: runs, and finds nothing new.
        assert_eq!(online(json!({})).unwrap()["runs"], json!([]));
        let forced = online(json!({"library": "根", "force": true})).unwrap();
        assert_eq!((forced["added"].as_u64(), forced["runs"][0]["skipped"].as_u64()), (Some(0), Some(2)));
        // A failure is reported beside the rest, not instead of it.
        let failed = call(&root, "sync_searches", &json!({"force": true})).unwrap();
        assert_eq!(failed["failed"], 1);
        assert!(failed["runs"][0]["error"].as_str().unwrap().contains("不应联网"));
        assert_eq!(call(&root, "list_libraries", &json!({})).unwrap()["libraries"][0]["autoSyncHours"], 24);
        assert!(library_catalog().iter().all(|t| t.name != "sync_searches"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn suggestions_are_not_decisions() {
        let (root, id) = sample();
        let said = call(&root, "suggest_screening", &json!({"id": id, "verdict": "included", "reason": "用了 10x 平台", "excerpt": "prepared with 10x Genomics Chromium", "confidence": 0.8, "author": "codex"})).unwrap();
        assert_eq!((said["excerptKept"].as_bool(), said["page"].as_u64(), said["screening"].as_str()), (Some(true), Some(2), Some("pending")));
        let paper = call(&root, "get_paper", &json!({"id": id})).unwrap();
        assert_eq!((paper["screening"].as_str(), paper["suggestion"]["verdict"].as_str(), paper["suggestion"]["author"].as_str(), paper["suggestion"]["basis"].as_str()), (Some("pending"), Some("included"), Some("codex"), Some("fulltext")));
        assert_eq!(call(&root, "list_papers", &json!({})).unwrap()["papers"][0]["suggested"], "included");
        // An invented quotation is dropped, and the caller is told.
        let invented = call(&root, "suggest_screening", &json!({"id": id, "verdict": "irrelevant", "reason": "r", "excerpt": "This paper is about leaves only."})).unwrap();
        assert_eq!(invented["excerptKept"], false);
        assert_eq!(call(&root, "get_paper", &json!({"id": id})).unwrap()["suggestion"]["excerpt"], "");
        for (args, message) in [(json!({"id": id, "verdict": "pending", "reason": "r"}), "建议应为"), (json!({"id": id, "verdict": "included"}), "需要参数 reason"), (json!({"id": id, "verdict": "included", "reason": "r", "confidence": "high"}), "confidence"), (json!({"id": "P-none", "verdict": "included", "reason": "r"}), "文献不存在")] {
            let error = call(&root, "suggest_screening", &args).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let schema = &catalog().into_iter().find(|t| t.name == "suggest_screening").unwrap().input_schema;
        assert_eq!(cli_args(schema, &["P-1".into(), "included".into(), "理由".into(), "--confidence".into(), "0.7".into()]).unwrap(), json!({"id": "P-1", "verdict": "included", "reason": "理由", "confidence": 0.7}));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn what_the_window_shows() {
        assert_eq!(describe_call("search_fulltext", &json!({"query": "10x  Genomics", "limit": 5})), "检索全文：10x Genomics · limit 5");
        assert_eq!(describe_call("read_fulltext", &json!({"paperId": "P-1", "page": 3})), "阅读全文：P-1 · 第 3 页起");
        assert_eq!(describe_call("library_status", &json!({})), "文献库概况");
        assert_eq!(describe_result(&Ok(json!({"total": 3}))), "3 条结果");
        assert_eq!(describe_result(&Ok(json!({"pages": [1, 2]}))), "读了 2 页");
        assert_eq!(describe_result(&Err("没有".into())), "没有");
        let long = json!({"text": "页".repeat(MAX_OUTPUT_BYTES)});
        let shown = output_text(&long);
        assert!(shown.len() < MAX_OUTPUT_BYTES + 200 && shown.contains("结果过长"));
        assert_eq!(output_text(&json!({"a": 1})), r#"{"a":1}"#);
        // Inside a library there is no library to choose.
        let inside = library_catalog();
        assert!(inside.iter().all(|t| t.name != "list_libraries" && t.input_schema["properties"].get("library").is_none()));
        assert_eq!(inside.len(), catalog().len() - 2);
    }

    #[test]
    fn text_is_extracted_on_first_use() {
        let root = std::env::temp_dir().join(format!("wisp-tools-{}", new_id("T")));
        let info = libraries::create(&root, "L", "", "blue").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let paper = store.add_paper(Paper { title: "Notes".into(), ..Default::default() }).unwrap();
        let source = root.join("notes.md");
        std::fs::write(&source, "Auxin sets the gradient.").unwrap();
        store.import_file(&paper.id, &source, "fulltext").unwrap();
        drop(store);
        assert_eq!(call(&root, "search_fulltext", &json!({"query": "auxin"})).unwrap()["hits"][0]["snippet"], "Auxin sets the gradient.");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn matching_keeps_positions() {
        // Characters whose lowercase form has another length stay as they are.
        for text in ["İstanbul\tROOT\u{3000}tip", "ǅ and ß", "根尖 WOX5"] {
            assert_eq!(searchable(text).len(), text.len(), "{text}");
        }
        assert_eq!(searchable("A\nB\tc"), "a b c");
        let page = format!("{}根尖 marker", "é".repeat(120));
        let at = searchable(&page).find("marker").unwrap();
        assert!(snippet(&page, at).ends_with("根尖 marker"));
    }

    #[test]
    fn command_line_arguments() {
        let tools = catalog();
        let schema = |name: &str| &tools.iter().find(|t| t.name == name).unwrap().input_schema;
        let words = |line: &str| line.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(cli_args(schema("get_paper"), &words("P-1 -l 根")).unwrap(), json!({"id": "P-1", "library": "根"}));
        assert_eq!(cli_args(schema("search_fulltext"), &words("chromium --paper-id P-1 --limit=5")).unwrap(), json!({"query": "chromium", "paperId": "P-1", "limit": 5}));
        assert_eq!(cli_args(schema("list_papers"), &[]).unwrap(), json!({}));
        for (name, line, message) in [("list_papers", "stray", "多余的参数"), ("list_papers", "--limit ten", "应为整数"), ("list_papers", "--limit", "需要一个值"), ("list_papers", "--colour red", "没有参数 --colour"), ("get_paper", "--paper-id 1", "可用：--id --library")] {
            let error = cli_args(schema(name), &words(line)).unwrap_err();
            assert!(error.contains(message), "{line}: {error}");
        }
        let help = usage(tools.iter().find(|t| t.name == "read_fulltext").unwrap());
        assert!(help.starts_with("wisp read_fulltext <paper-id>\n") && help.contains("--pages 读几页"));
    }

    #[test]
    fn every_tool_is_callable() {
        let (root, _) = sample();
        for tool in catalog() {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            // An empty call may be refused for what it lacks, but never because the name leads nowhere.
            if let Err(error) = call(&root, tool.name, &json!({})) { assert!(!error.contains("没有这个工具"), "{}: {error}", tool.name) }
            // Only a tool that changes the library has anything to ask the person about.
            assert!(tool.writes || !tool.confirm, "{}", tool.name);
        }
        assert_eq!(library_catalog().iter().filter(|tool| tool.confirm).map(|tool| tool.name).collect::<Vec<_>>().len(), 4);
        std::fs::remove_dir_all(root).ok();
    }
}
