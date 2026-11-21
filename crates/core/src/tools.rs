//! The tools an agent works a library with. One table, three ways in: the
//! `wisp` command line (a tool name and flags read off its schema), the MCP
//! server (`mcp`) and the assistant inside the app. All of them end in
//! `call`, so a rule written here holds everywhere.
//!
//! Mutating tools are marked for the read-only MCP mode and the window's
//! confirmation flow. Reading a stored PDF may also fill its text cache.

mod local;
mod web;

use crate::complete;
use crate::extract::{self, Proposal};
use crate::filter::{Filters, filter_papers};
use crate::fulltext::{self, terms};
use crate::libraries::{self, LibraryInfo};
use crate::model::{FULLTEXTS, Paper, READINGS, SCREENINGS, one_of};
use crate::openaccess;
use crate::organise;
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

/// The tools that change the library.
const WRITING: [&str; 27] = ["create_library", "import_library", "export_library", "import_pdf", "import_references", "import_zotero", "save_search", "remove_search", "run_search", "undo_import", "suggest_screening", "suggest_field", "sync_searches", "find_pdf", "import_url", "organise_papers", "edit_section", "set_outline", "comment_manuscript", "export_note", "save_note_to_notion", "resolve_note_save", "save_cite_style", "restyle_docx", "import_docx_references", "import_manuscript", "make_slides"];
/// Of those, the ones the window's assistant runs only with the person's
/// consent. Saving a search, suggesting a screening decision or proposing
/// a field's value changes nothing a person has not yet to act on, so they
/// run as asked. The two that write a manuscript (`MANUSCRIPT`) are asked
/// about everywhere but in the conversation that writes that manuscript,
/// where the person has handed the pen over. `comment_manuscript` is not among them: a comment changes no
/// word of the manuscript, it waits for the person like a screening suggestion.
const CONFIRMING: [&str; 20] = ["import_library", "export_library", "import_pdf", "import_references", "import_zotero", "run_search", "undo_import", "remove_search", "find_pdf", "import_url", "organise_papers", "edit_section", "set_outline", "export_note", "save_note_to_notion", "resolve_note_save", "restyle_docx", "import_docx_references", "import_manuscript", "make_slides"];
/// The tools that write a manuscript (`compose`).
pub const MANUSCRIPT: [&str; 2] = ["edit_section", "set_outline"];

/// A tool that works inside one library: it also takes `library`.
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
        scoped("list_note_sources", "选择笔记来源", "列出这篇文献的历史对话、问题序号和 Notion 保存记录。paperId 留空列出文献库级对话。先读取它，再选择要导出的问答。", json!({"paperId": {"type": "string"}}), &[]),
        scoped("prepare_note", "预览笔记", "将个人笔记和选定问答组成 Markdown 草稿。保留原内容和图片，缺失的元数据不补造。只读，不改本地笔记。先用此工具预览，再保存或导出。", crate::notes::properties(None), &[]),
        scoped("export_note", "导出笔记到本地", "将笔记草稿导出为 Markdown 和相邻图片目录。path 必须是用户指定的 .md 文件路径；不覆盖已有文件。不改原笔记。", crate::notes::properties(Some(("path", "用户指定的本地 .md 文件路径；已有文件不会覆盖。"))), &["path"]),
        scoped("save_note_to_notion", "保存笔记到 Notion", "通过设置中已启用的 Notion MCP 保存笔记。parent 必须是用户明确指定的父页面链接或 ID，不能猜测。首存创建子页面，后续保存追加到同一文献页面，完全相同的草稿跳过。原笔记不变。结果不确定时停止，不能自动重试或自行清除待确认记录；让用户在 Notion 核对。", crate::notes::properties(Some(("parent", "用户明确指定的 Notion 父页面链接或 ID。"))), &["parent"]),
        scoped("resolve_note_save", "核对 Notion 保存结果", "仅在用户已检查 Notion 并明确告知保存结果后使用。saved=true 同时给出实际写入的 page 页面链接，或 saved=false 表示用户确认完全没有写入。不要根据网络错误推断。部分写入不能标为未写入。", json!({"id": {"type": "string"}, "saved": {"type": "boolean"}, "page": {"type": "string"}}), &["id", "saved"]),
        ToolDef {
            name: "list_libraries",
            title: "列出文献库",
            description: "列出所有文献库：ID、名称、主题和文献数量。其他工具用 library 参数指定其中一个。首页上还没有建成文献库的对话不在其中。",
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            writes: false,
            confirm: false,
        },
        ToolDef {
            name: "create_library",
            title: "新建文献库",
            description: "在数据根目录中新建独立文献库，返回真实 ID、名称、主题和目录。每次调用都会创建一个新库，导入时用返回的 id 指定 library。",
            input_schema: json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "文献库名称，必填，最多 60 个字符。"},
                "topic": {"type": "string", "description": "主题描述，最多 2000 个字符。"},
                "color": {"type": "string", "enum": libraries::COLORS, "description": "标识色，默认 blue。"},
            }, "required": ["name"], "additionalProperties": false}),
            writes: true,
            confirm: true,
        },
        ToolDef {
            name: "import_library",
            title: "导入文献库文件",
            description: "把另一个 Wisp Study 导出的 .wispstudy 文件（或改名前的 Wisp Paper 导出的 .wisppaper 文件）导入为新的文献库，连同其中的文献、笔记、对话、稿件和附件。总是新建，不与已有文献库合并；同名时名称后加编号。返回新库的 ID、名称和文献数量。",
            input_schema: json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "本机 .wispstudy 或 .wisppaper 文件路径，建议使用绝对路径。"},
            }, "required": ["path"], "additionalProperties": false}),
            writes: true,
            confirm: true,
        },
        scoped("export_library", "导出文献库文件", "把整个文献库导出为一个 .wispstudy 文件，供另一台电脑上的 Wisp Study 用 import_library 导入：文献、词条、筛选、笔记、对话、稿件和图片库，可选是否包含全文 PDF 等附件。不改文献库；不覆盖已有文件。", json!({
            "path": {"type": "string", "description": "用户指定的 .wispstudy 文件路径；已有文件不会覆盖。"},
            "attachments": {"type": "boolean", "description": "是否包含全文 PDF 和其他附件，默认 true。为 false 时文件小得多，但对方的文献没有全文，PDF 上的标注和读出的全文也不带过去。"},
        }), &["path"]),
        scoped("import_pdf", "导入本地 PDF", "复制本地 PDF 到文献库；path 也可以是文件夹，递归导入其中的 PDF，子文件夹名称转为标签。离线读取前两页中的 DOI，未补全的题目使用文件名；不联网查询元数据。已有文献保留原记录并补附件，回收站条目跳过。返回逐项结果；failed 表示存在导入问题的项数，textless 表示已保存但没有读出全文的项数。", json!({
            "path": {"type": "string", "description": "本机 PDF 或文件夹路径，建议使用绝对路径。"},
            "paperId": {"type": "string", "description": "把单个 PDF 附到这篇已有文献；不能与文件夹一起使用。"},
        }), &["path"]),
        scoped("import_references", "导入 BibTeX / RIS", "读取本地 BibTeX 或 RIS 文件，按内容自动识别格式并导入参考文献记录。按 DOI、PMID、题目去重，回收站条目也会跳过；不自动导入 PDF，不联网。返回新增和跳过的数量及文献 ID。", json!({
            "path": {"type": "string", "description": "本机 BibTeX / RIS 文件路径，建议使用绝对路径。"},
        }), &["path"]),
        scoped("import_zotero", "导入 Zotero", "从本机 Zotero 6 / 7 的 zotero.sqlite 导入个人库和群组库的文献、标签、笔记及可访问的本地 PDF。只读源数据库、复制附件；已有文献保留原记录并补 PDF，回收站条目跳过。不联网。建议先退出 Zotero；返回逐项结果，必须检查 failed 和 results 中的 problems。", json!({
            "path": {"type": "string", "description": "本机 Zotero 数据目录中的 zotero.sqlite 路径；storage 目录应在它旁边。"},
        }), &["path"]),
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
        scoped("library_status", "文献库概况", "一个文献库的概况：主题、文献数量、收录判断和阅读状态的分布、自定义词条、已有的标签，以及各状态值在界面上的显示名称。", json!({}), &[]),
        scoped(
            "list_pdf_annotations",
            "读取 PDF 标注",
            "读取用户在 PDF 上保存的下划线、高亮、方框、便签、截图标记（cut：用户圈出来准备截图的区域，多是图或表）及笔记。quote 是选中的原文（方框、便签和截图标记为空），note 是用户的笔记或便签内容，不是论文结论。返回文献和附件 ID、1-based 页码、boxes（从页面左上角起的 PDF 点坐标 [left, top, right, bottom]：下划线和高亮每行一个，方框和截图标记是框住的区域，便签是它所在的点）、类型、颜色（color：yellow、green、blue、pink、purple、red、orange，是用户自己的分类；下划线和便签为空）和时间。回收站中的文献不返回。",
            json!({
                "paperId": {"type": "string", "description": "只返回这篇文献的标注。"},
                "resourceId": {"type": "string", "description": "只返回这个 PDF 附件的标注，附件 ID 见 get_paper 的 files。"},
                "page": {"type": "integer", "minimum": 1, "description": "只返回 PDF 的这一页（从 1 开始）。"},
                "kind": {"type": "string", "enum": ["underline", "highlight", "box", "note", "cut"], "description": "只返回下划线、高亮、方框、便签或截图标记（cut）。"},
                "query": {"type": "string", "description": "在原文摘录和用户笔记中搜索；空格分开的每个词都必须出现。"},
                "limit": {"type": "integer", "description": "最多返回多少条，默认 50，最多 500。"},
                "offset": {"type": "integer", "description": "跳过多少条，用于翻页。"},
            }),
            &[],
        ),
        scoped(
            "list_figures",
            "查询图片库",
            "查询图片库：从文献 PDF 里截下来收藏的图片，各自的图号、图注、标签、备注，以及它出自哪篇文献的哪一页；drawn 为 true 的是生图模型按描述画的，不出自任何文献。file 是图片文件在本机的路径。回收站中文献的图片不返回。",
            json!({
                "query": {"type": "string", "description": "在图号、图注、标签、备注，以及所属文献的题目、作者、期刊和年份中搜索；空格分开的每个词都必须出现。"},
                "tag": {"type": "string", "description": "只返回带这个图片标签的图片。"},
                "paper": {"type": "string", "description": "只返回这篇文献（ID）的图片。"},
                "limit": {"type": "integer", "description": "最多返回多少张，默认 50，最多 500。"},
                "offset": {"type": "integer", "description": "跳过前面多少张，用于翻页。"},
            }),
            &[],
        ),
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
        scoped("journal_metrics", "查期刊分区与影响因子", "查一种期刊的影响因子和分区：JCR 影响因子与最佳分区（Q1–Q4）、中科院分区表升级版大类分区与是否 Top、新锐期刊分区表分区，以及预警名单。数据随应用附带，不联网。给 journal（刊名，全称或 PubMed 式缩写，或 ISSN），或 paperId（用这篇文献的期刊）。查不到时 found 为 false。", json!({"journal": {"type": "string", "description": "刊名或 ISSN，例如 Nature Communications、Nat Commun、2041-1723。"}, "paperId": {"type": "string", "description": "文献 ID；不给 journal 时用这篇文献的期刊。"}}), &[]),
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
            "为一段文字逐句找出文献库中可能支持它的文献：每句话按文献的题目、摘要、笔记和全文里的句子检索，返回候选文献和它自己最贴近的一句原文（in 是这句所在的部分，全文里的附页码）。只是按词语找出的候选，是否真的支持这句话需要读原文判断。文字和文献不是同一种语言时（返回的 language 是文献的语言），用 queries 给每句配上那种语言的检索词，否则找不到。一句话该引什么看它在做什么，用 intents 说明：转述前人发现、与前人比较或说到方法的句子，优先给出文献自己（导言以外）的原句；讲背景的句子把综述（review 为 true）排在前面；作者自己的话不查。原句里按作者和年份点了名的文献列在 names 里，带 paperId 的是文献库里有的：原句只是在转述时，出处是它们，不是这篇。",
            json!({
                "text": {"type": "string", "description": "要配引用的文字，可以是一句或一段。"},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "各句的检索词，按句子的顺序，用文献的语言写（术语、同义词、缩写，空格分隔）；不需要的句子给空字符串。"},
                "intents": {"type": "array", "items": {"type": "string"}, "description": "各句在做什么，按句子的顺序：background（领域背景、共识）、finding（转述前人的具体发现）、method（方法、工具、数据）、gap（未解决的问题）、compare（与前人结果比较）、own（本文自己的话，不查）；不清楚的给空字符串。"},
                "perSentence": {"type": "integer", "description": "每句最多几篇候选，默认 3，最大 10。"},
            }),
            &["text"],
        ),
        scoped(
            "find_passages",
            "查找引用的出处",
            "在一篇文献里找出最贴近给定句子的原文：稿件里的一句话引用了这篇文献，它依据的是文献的哪一句。按文献的摘要和全文里的句子检索，每句返回几句候选原文（in 是它所在的部分，全文里的附页码；fair 为 false 的只是有个别词相同）。只是按词语找出的候选，是否真的支持这句话需要读原文判断。句子和文献不是同一种语言时，在 queries 里给出文献所用语言的检索词。只读。",
            json!({
                "paperId": {"type": "string", "description": "文献 ID。"},
                "texts": {"type": "array", "items": {"type": "string"}, "description": "稿件里引用这篇文献的句子，可以不止一句。"},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "各句的检索词，按 texts 的顺序，用文献的语言写；不需要的给空字符串。"},
                "limit": {"type": "integer", "description": "每句最多几句候选原文，默认 5，最大 10。"},
            }),
            &["paperId", "texts"],
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
                "maxResults": {"type": "integer", "description": format!("每次运行取回多少条最新记录，默认 50，最大 {}。多于 {} 条时分几次向数据库要。", sources::MAX_RESULTS, sources::PAGE)},
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
            "撤销一个导入批次：把它新增的、还没有人处理过的文献移出文献库（不进回收站，所以以后的检索还能再导入）；移出的记录另存为文献库文件夹里的 removed/undone-import-<批次号>.bib（结果里的 setAside），可以用 import_references 再导入。已经做过收录判断、读过、收藏、写过笔记、加过标签或附件、提过问、被稿件引用的文献保留，并在结果里列出。",
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
            "import_url",
            "按网址添加",
            "把一个网址作为一篇文献加进文献库：文献在出版商网站上的页面，或者 PDF 本身的地址。只请求这一个网址一次（用本机的网络，校园网能下载的就能取到），读网页上的 citation_doi、citation_pmid、citation_pdf_url 等标签，按 DOI / PMID 查询题目、作者等信息后添加（已有的文献不重复添加），再下载网页给出的 PDF；拿不到时再找开放获取的版本（同 find_pdf）。网页打不开但网址里写着 DOI 时，按这个 DOI 添加。不登录，不带 cookie，不冒充浏览器，不跟随网页上的其他链接；遇到付费墙或验证页如实说明，并告诉用户可以在浏览器里下载后把 PDF 拖进窗口。认不出 DOI、PMID 也没有 PDF 时不添加。会把 DOI 或 PMID 发给 Crossref、Europe PMC 和 OpenAlex。",
            json!({"url": {"type": "string", "description": "以 http:// 或 https:// 开头的文献页面或 PDF 地址，一次一个。"}}),
            &["url"],
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
        scoped(
            "suggest_field",
            "提出词条取值",
            "为一篇文献的一个自定义词条提出取值，供用户在提取页确认。这不会改动词条：词条只由用户本人写入。同一篇文献同一词条的新建议会替换旧的。词条的名称、类型、说明和选项见 library_status 的 fields。提出前先读这篇文献的记录（get_paper），有全文时读全文（read_fulltext）；只依据文中写明的内容，文中没有说明时 value 留空。",
            json!({
                "id": {"type": "string", "description": "文献 ID。"},
                "field": {"type": "string", "description": "词条的名称或 ID。"},
                "value": {"type": "string", "description": "取值。多值词条的几个值用分号隔开；单选词条必须是它的选项之一；留空表示文中没有说明。"},
                "excerpt": {"type": "string", "description": "支持这个取值的一句原文，逐字摘自摘要或全文。不在原文中的引文不会被保存，没有原文依据的取值不会默认勾选。"},
                "author": {"type": "string", "description": "提出建议的是谁，例如模型或 agent 的名字。"},
            }),
            &["id", "field"],
        ),
        scoped(
            "organise_papers",
            "整理文献",
            "整理文献库里已有的文献：加或去标签、设或取消星标、改阅读状态、在「我的笔记」末尾追加一段（注明是助手所记，不覆盖已有笔记）。一次调用可以改许多篇，要么全部写入，要么一篇也不写。只在用户要求整理时使用；窗口里的助手运行前，用户会先看到每一篇要改什么。不能改收录判断（用 suggest_screening 提出建议）和自定义词条（用 suggest_field）。返回每篇文献实际改了什么；已经是所要求的样子的不会改动。",
            json!({
                "changes": {
                    "type": "array",
                    "description": "每项是一篇文献的改动；没写的方面保持不变。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "文献 ID。"},
                            "addTags": {"type": "array", "items": {"type": "string"}, "description": "要加上的标签。优先用文献库里已有的标签（library_status 的 tags），不要为同一个意思另造一个。"},
                            "removeTags": {"type": "array", "items": {"type": "string"}, "description": "要去掉的标签。"},
                            "starred": {"type": "boolean", "description": "true 加星标，false 取消。"},
                            "reading": {"type": "string", "enum": keys(&READINGS), "description": "阅读状态：未读、已浏览、已精读。这是用户自己的阅读记录，只在用户说明时修改。"},
                            "note": {"type": "string", "description": "追加到「我的笔记」末尾的一段话。"},
                        },
                        "required": ["id"],
                    },
                },
            }),
            &["changes"],
        ),
        scoped("list_cite_styles", "引用样式列表", "列出导出 Word 时可用的引用样式（CSL，与 Zotero 用的是同一种定义文件）：common 是常用的内置样式，added 是用户添加的和 Zotero 里装的，builtin 是全部内置样式。每个样式有 key 和名称。只读。", json!({}), &[]),
        scoped(
            "read_cite_style",
            "读取引用样式",
            "读取一个引用样式的 CSL 定义（XML）。用户想要一个内置样式稍作改动的版本时，先读最接近的那个，在它的基础上改，再用 try_cite_style 试排、save_cite_style 保存。只读。",
            json!({"style": {"type": "string", "description": "样式的 key，来自 list_cite_styles，例如 csl:nature、file:my-journal.csl。"}}),
            &["style"],
        ),
        scoped(
            "try_cite_style",
            "试排引用样式",
            "用文献库里的文献试排一个引用样式，不保存：返回正文里的引用（单篇、多篇）和参考文献条目各是什么样子，italic、bold、superscript 标出斜体、粗体、上标的文字。改 CSL 时每改一次都先试排，把结果给用户看。CSL 写错时返回错误所在。只读。文献记录里没有卷、期、页码，试排结果里也不会有。",
            json!({
                "csl": {"type": "string", "description": "要试的 CSL 定义（完整的 XML）。与 style 二选一。"},
                "style": {"type": "string", "description": "已有样式的 key。与 csl 二选一。"},
                "paperIds": {"type": "array", "items": {"type": "string"}, "description": "用哪些文献试排；省略时取文献库里最近的几篇。"},
                "language": {"type": "string", "description": "稿件的语言代码（en、zh…），决定 et al. 与「等」这类用语；省略为 en。"}
            }),
            &[],
        ),
        scoped(
            "save_cite_style",
            "保存引用样式",
            "把一份 CSL 定义保存为用户的引用样式（数据文件夹的 styles/ 下），之后出现在写作页「导出」菜单的引用样式里，由用户自己选用。保存前先用 try_cite_style 试排并得到用户认可。<info> 里的 <title> 是菜单里显示的名称，<id> 要与原样式不同。同名文件会被替换，原来的内容留在 styles/removed/。返回样式的 key 和试排结果。",
            json!({
                "name": {"type": "string", "description": "文件名，例如 my-journal.csl；只用字母、数字和连字符。"},
                "csl": {"type": "string", "description": "完整的 CSL 定义（XML）。"}
            }),
            &["name", "csl"],
        ),
        scoped(
            "read_docx_citations",
            "读 Word 文件的引用",
            "读一个在别处写好的 Word 文件（.docx）引用了什么，不改文件：正文里的每处引用、参考文献表的每一条，以及每一条是怎样识别出来的。文件带 Zotero / Mendeley 的引用域时，文献信息直接取自域（resolved 为 field，不需要猜）；否则按文字读：「参考文献」「References」标题后的段落是条目，正文里的 [1]、[2, 4–6]、上标编号或（Zhang et al., 2024a; Li & Wang, 2019）、张三等（2020）与条目对上。每条的 resolved：library（是文献库里的文献）、lookup（联网查到）、text（按规则从这条文字里读出，sure 为 guess 时请核对 read 的内容）、given（你给的）、unresolved（没能识别）。citations.keptAsWritten 是重排时会保持原样的引用和原因。只读。",
            json!({
                "path": {"type": "string", "description": "本机 .docx 文件的路径。"},
                "lookup": {"type": "boolean", "description": "为 true 时，对文献库里没有的条目联网查 Crossref：有 DOI 的按 DOI 查，文字读不全的按文字查（只采用题目与条目相符的记录）。默认 false。"},
                "entries": {"type": "array", "description": "你替规则读懂的条目：n 是参考文献表里的第几条（从 1 起），csl 是这一条的 CSL-JSON（type、title、author 的 family / given、issued 的 date-parts、container-title、volume、issue、page、DOI…）。只转写条目文字里写着的信息。", "items": {"type": "object", "properties": {"n": {"type": "integer"}, "csl": {"type": "object"}}, "required": ["n", "csl"]}}
            }),
            &["path"],
        ),
        scoped(
            "restyle_docx",
            "重排 Word 文件的引用",
            "把一个 Word 文件（.docx）的引用和参考文献表按一个 CSL 引用样式重排，写成一个新文件：正文里的每处引用就地换成新样式的写法，并且是点了能跳到对应条目的域；原来的参考文献表换成新样式的表；其余内容、格式、图表、批注都不动。原文件不改，输出路径上已有文件时不覆盖。先用 read_docx_citations 看清识别结果并告诉用户，再调用这个工具。样式可以是 list_cite_styles 里的 key，也可以直接给一份 CSL 定义（用 read_cite_style 读出最接近的样式改好、try_cite_style 试排后再用，例如「Nature 但用方括号」）。没能识别的条目原样留在表末，引用它们的地方、带前后缀的引用保持原样，结果里逐条说明。Zotero / EndNote 的引用域被重排后是普通的跳转域，不再由它们管理。",
            json!({
                "path": {"type": "string", "description": "本机 .docx 文件的路径。"},
                "output": {"type": "string", "description": "新文件的路径（.docx）；已有文件不会覆盖。省略时写在原文件旁边，文件名加 -restyled。"},
                "style": {"type": "string", "description": "引用样式的 key，例如 csl:nature、csl:apa、csl:gb-7714-2015-numeric、file:my-journal.csl。与 csl 二选一。"},
                "csl": {"type": "string", "description": "一份完整的 CSL 定义（XML），不保存。与 style 二选一。"},
                "track": {"type": "boolean", "description": "为 true 时把改动写成 Word 的修订，用户可以在 Word 里逐处接受或拒绝。默认 false。"},
                "author": {"type": "string", "description": "修订的作者名；省略为 Wisp Study。"},
                "language": {"type": "string", "description": "样式用语的语言（en、zh…：et al. 与「等」）；省略时按文件正文的语言。"},
                "lookup": {"type": "boolean", "description": "同 read_docx_citations。"},
                "entries": {"type": "array", "description": "同 read_docx_citations。", "items": {"type": "object", "properties": {"n": {"type": "integer"}, "csl": {"type": "object"}}, "required": ["n", "csl"]}}
            }),
            &["path"],
        ),
        scoped(
            "import_docx_references",
            "从 Word 文件收进参考文献",
            "把一个 Word 文件（.docx）参考文献表里的文献收进文献库，不改文件：识别方式同 read_docx_citations（Zotero / Mendeley 的引用域直接取文献信息，否则按「参考文献」「References」标题后的条目逐条读）。lookup 默认开：有 DOI 的按 DOI 查 Crossref，按文字读出但没有 DOI 的按文字查 Crossref 补上 DOI 和期刊信息（只采用题目和年份相符的记录）。按 DOI、PMID、题目去重，文献库里已有的、回收站里的都跳过，所以可以放心重复调用。没能识别的条目不会收进来，结果的 unresolved 里逐条列出原文；你读懂其中一条后，用 entries 给出它的 CSL-JSON 再调用一次（给了 entries 时只收这几条，不再联网）。checkAgainst 里是按规则读出、没有核对过的条目，请提醒用户核对。",
            json!({
                "path": {"type": "string", "description": "本机 .docx 文件的路径。"},
                "lookup": {"type": "boolean", "description": "联网查 Crossref 补全和核对条目。默认 true；为 false 时只用文件里的信息。"},
                "entries": {"type": "array", "description": "你替规则读懂的条目：n 是参考文献表里的第几条（从 1 起），csl 是这一条的 CSL-JSON（type、title、author 的 family / given、issued 的 date-parts、container-title、volume、issue、page、DOI…）。只转写条目文字里写着的信息，不要补写没有的。给出时只收这几条。", "items": {"type": "object", "properties": {"n": {"type": "integer"}, "csl": {"type": "object"}}, "required": ["n", "csl"]}}
            }),
            &["path"],
        ),
        scoped(
            "import_manuscript",
            "导入稿件",
            "把一个在别处写好的稿件文件导入写作页，成为一份新稿件，等同于用户在写作页点「导入」：Word（.docx）、Markdown（.md）或纯文本（.txt）。原文件不改。正文按文件的标题分节（Word 文件没有用标题样式时，按看起来像标题的段落分节，结果的 guessedHeadings 列出它们，用户可以在写作页调整）；Word 文件里的批注一起带进来，表格和图片留在文件的副本里，从写作页导出 Word 时还在原处。正文里的引用保持文件里的写法，不会变成文献库的引用；要把文件参考文献表里的文献收进文献库，另用 import_docx_references。同一个文件已经是稿件时不会再导入一份（already 为 true）。导入后用 read_manuscript 读它、comment_manuscript 留审阅意见、edit_section 改它。",
            json!({
                "path": {"type": "string", "description": "本机稿件文件的路径：.docx、.md、.markdown 或 .txt。"}
            }),
            &["path"],
        ),
        scoped(
            "make_slides",
            "做幻灯片",
            "把内容做成一份 PowerPoint 幻灯片（.pptx），写成一个新文件：读一篇文献做汇报、组会讲文献、把几篇文献的比较讲成几页，都用它。你只说每页有什么（标题、要点、图、图注、出处、讲稿），版面由程序排：字号按内容选，要点一页放不下时接着排到下一页，图和要点并排或上下放；第一页是标题页，不用写在 slides 里。先用 read_fulltext 读过文献再写，内容以文献为准；每页 3 到 5 条要点，每条一句话，展开讲的话放 notes。图来自图片库（先用 list_figures 按 paper 查这篇文献已收藏的图，figure 给图片的 ID，图注和出处会自动带上）或本机的 PNG / JPEG 文件（image）；文献的图还没有收藏时，先出没有图的版本，并告诉用户在 PDF 页上框选收藏图片后可以重做。只有 title 的一页是分节页。输出路径上已有文件时不覆盖。",
            json!({
                "title": {"type": "string", "description": "标题页的标题；省略时用 paperId 那篇文献的题目。"},
                "subtitle": {"type": "string", "description": "标题页标题下的一行（报告人、场合、日期…）；省略且给了 paperId 时写文献的作者、年份和期刊。"},
                "paperId": {"type": "string", "description": "讲的是哪篇文献（文献 ID）。给了以后，没有写 source 的页在页脚写它的出处。"},
                "language": {"type": "string", "enum": ["zh", "en"], "description": "幻灯片的语言；省略时按标题判断。"},
                "slides": {
                    "type": "array",
                    "description": "标题页之后的各页，按顺序。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string", "description": "这一页的标题，一行以内。"},
                            "bullets": {"type": "array", "items": {"type": "string"}, "description": "要点，每条一句话；以两个空格开头的是上一条下面的小点。可以用 **粗体** 标出关键词。"},
                            "figure": {"type": "string", "description": "图片库里一张图的 ID（见 list_figures）。"},
                            "image": {"type": "string", "description": "本机 PNG 或 JPEG 文件的路径；与 figure 只给一个。"},
                            "caption": {"type": "string", "description": "图下面的图注；用 figure 时省略就是图片库里的图号和图注。"},
                            "source": {"type": "string", "description": "页脚的出处，例如「Zhang et al., 2024 · p. 5」；省略时按 figure 或 paperId 写。不要写 P1 这样只在这次对话里有意义的编号。"},
                            "notes": {"type": "string", "description": "讲稿备注：讲这一页时要说的话，不显示在幻灯片上。"}
                        }
                    }
                },
                "output": {"type": "string", "description": "新文件的路径（.pptx）；已有文件不会覆盖。省略时写在用户的「文档」文件夹，文件名是标题。"}
            }),
            &["slides"],
        ),
        scoped("list_manuscripts", "稿件列表", "列出用户在写作页的稿件：题目、语言、各节的名称、属于论文的哪一部分和字数。只读。", json!({}), &[]),
        scoped(
            "read_manuscript",
            "读取稿件",
            "读取用户在写作页写的稿件正文，可以只读其中一节。只读；要改稿件用 edit_section。稿件里的引用写作 [@Key]，Key 对应文献库里的一篇文献；每节的 passages 是已记下的引用出处：稿件里的哪句话（sentence）依据文献的哪句原文（passage，page 为 0 时出自摘要）。",
            json!({
                "manuscript": {"type": "string", "description": "稿件的 ID 或题目；只有一份稿件时可以省略。"},
                "section": {"type": "string", "description": "只读这一节：节的 ID 或名称。省略时返回所有写了内容的节。"},
            }),
            &[],
        ),
        scoped(
            "comment_manuscript",
            "写批注",
            "在写作页稿件的一节上留批注，像 Word 审阅模式里那样，供用户逐条处理：每条针对这一节里的一处原文（quote，从 read_manuscript 读到的正文里逐字摘出，不跨段落，尽量短但能唯一定位，各条不重叠），comment 说明问题或理由，需要改文字时 replacement 给出整体替换 quote 的文字（只提醒时省略）。批注不改稿件正文：要不要采纳由用户决定，导出 Word 时写成 Word 的批注。在稿件里找不到原文的批注不会留下，列在 missed 里，可以改正 quote 后再试。每次最多 12 条，先说最要紧的。",
            json!({
                "manuscript": {"type": "string", "description": "稿件的 ID 或题目；只有一份稿件时可以省略。"},
                "section": {"type": "string", "description": "批注所在的一节：节的 ID 或名称。"},
                "remarks": {
                    "type": "array",
                    "description": "各条批注，按在这一节里的先后排列。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "quote": {"type": "string", "description": "从这一节逐字摘出的原文。"},
                            "comment": {"type": "string", "description": "批注：指出问题，或说明建议的修改和理由。用用户提问的语言写。"},
                            "replacement": {"type": "string", "description": "建议用来整体替换 quote 的文字，用稿件的语言写；只提醒、不改文字时省略。"},
                        },
                        "required": ["quote", "comment"],
                    },
                },
            }),
            &["section", "remarks"],
        ),
        scoped(
            "edit_section",
            "写稿件",
            "写或修改写作页稿件的一节。只在用户要求你动笔（写、改、补、删稿件里的文字）时使用。两种用法取其一：edits 逐处修改，每项的 old 是从这一节现在的文字里逐字摘出的原文（先用 read_manuscript 读），new 是改成的文字，old 留空表示加在这一节末尾，new 留空表示删去；text 是这一节的全部新正文，用于空的一节或整节重写。正文是 Markdown，用稿件的语言写，节的名称不要写进正文。引用写作 [@Key]（Key 见 read_manuscript 里已有的引用），不知道 Key 时写 [@文献ID]，写入时会换成这篇文献的引用键；不要引用文献库里没有的文献。改动前这一节的文字会留作一个历史版本，用户可以随时换回去。返回改了几处；missed 是没有在这一节里找到原文、因而没有改的地方。",
            json!({
                "manuscript": {"type": "string", "description": "稿件的 ID 或题目；只有一份稿件时可以省略。"},
                "section": {"type": "string", "description": "要写的一节：节的 ID 或名称。"},
                "edits": {
                    "type": "array",
                    "description": "逐处修改，按在这一节里的先后排列，互不重叠。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old": {"type": "string", "description": "从这一节逐字摘出的原文，尽量短但能唯一定位；留空表示加在末尾。"},
                            "new": {"type": "string", "description": "改成的文字；留空表示删去。插入新内容时，先原样写出 old，再接上新内容。"},
                        },
                        "required": ["old", "new"],
                    },
                },
                "text": {"type": "string", "description": "这一节的全部新正文（与 edits 二选一）。"},
            }),
            &["section"],
        ),
        scoped(
            "set_outline",
            "定大纲",
            "设定写作页一份稿件的大纲：它有哪些节、各叫什么、按什么顺序，也可以改题目和正文语言（只改这两样时不给 sections，大纲保持原样）。sections 是完整的新大纲：保留的节带上它的 id（或现在的名称），可以改名、改类型、换位置；不带 id 的是新加的节。没有列出的节，从没写过内容的会去掉，写过内容的不会删除，排到最后并在 kept 里说明。kind 是这一节相当于论文的哪一部分，决定写这一节时参考文献库里文献的哪一部分；综述自拟的各节用 other。只在用户要求拟定或调整大纲时使用。",
            json!({
                "manuscript": {"type": "string", "description": "稿件的 ID 或题目；只有一份稿件时可以省略。"},
                "title": {"type": "string", "description": "稿件的新题目；省略时不改。"},
                "language": {"type": "string", "enum": crate::manuscript::LANGUAGES.iter().map(|(code, _)| *code).collect::<Vec<_>>(), "description": "稿件正文用什么语言写；省略时不改。"},
                "sections": {
                    "type": "array",
                    "description": "各节，按先后排列。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "保留现有的一节：它的 ID 或现在的名称。新加的节不写。"},
                            "name": {"type": "string", "description": "这一节的名称。"},
                            "kind": {"type": "string", "enum": keys(&crate::structure::SECTIONS), "description": "相当于论文的哪一部分；省略时现有的节保持原样，新加的节按名称判断。"},
                        },
                        "required": ["name"],
                    },
                },
            }),
            &[],
        ),
        scoped(
            "offer_action",
            "请用户决定",
            "在这条回答的下面放一张卡片：一句话和 1 到 3 个按钮，用户按下才执行，现在什么都不做。用在一件事做完、下一步该由用户决定的时候，例如收进文献后请用户把对话建成文献库，或给出几个可选的下一步（reply）。不要每条回答都用；同样的意思不要在回答里再问一遍。按钮只能是固定的几种（见 actions）。",
            crate::offer::properties(),
            &["text", "actions"],
        ),
    ]
}

/// The tools of the assistant inside an open library: no choosing a library.
pub fn library_catalog() -> Vec<ToolDef> {
    let mut tools = catalog();
    // The tools that range over every library have no place inside one.
    tools.retain(|t| !["list_libraries", "create_library", "import_library", "sync_searches"].contains(&t.name));
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
            // The words written are for the manuscript, not for the line that says a section is being written.
            ("text", Value::String(text)) if name == "edit_section" => numbers.push((key, format!("全文 {} 字", text.trim().chars().count()))),
            (_, Value::String(text)) if !text.trim().is_empty() => words.push((key, text.split_whitespace().collect::<Vec<_>>().join(" "))),
            (_, Value::Number(n)) => numbers.push((key, format!("{key} {n}"))),
            ("changes", Value::Array(items)) => numbers.push((key, format!("{} 篇", items.len()))),
            ("edits", Value::Array(items)) => numbers.push((key, format!("{} 处", items.len()))),
            ("remarks", Value::Array(items)) => numbers.push((key, format!("{} 条", items.len()))),
            ("sections", Value::Array(items)) => numbers.push((key, format!("{} 节", items.len()))),
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
            } else if let (Some(changed), Some(unchanged)) = (value["changed"].as_u64(), value["unchanged"].as_u64()) {
                if unchanged > 0 { format!("改了 {changed} 篇，{unchanged} 篇不用改") } else { format!("改了 {changed} 篇") }
            } else if let Some(commented) = value["commented"].as_u64() {
                let missed = value["missed"].as_array().map_or(0, Vec::len);
                let name = value["section"]["name"].as_str().unwrap_or("");
                if missed == 0 { format!("在「{name}」留了 {commented} 条批注") } else { format!("在「{name}」留了 {commented} 条批注，{missed} 条的原文没有找到") }
            } else if let Some(edited) = value["edited"].as_u64() {
                let missed = value["missed"].as_array().map_or(0, Vec::len);
                let name = value["section"]["name"].as_str().unwrap_or("");
                match (edited, missed) {
                    (0, 0) => format!("「{name}」不用改"),
                    (_, 0) => format!("已写入「{name}」"),
                    (0, _) => format!("「{name}」的 {missed} 处没有对上原文，没有改"),
                    _ => format!("已写入「{name}」，另有 {missed} 处没有对上原文"),
                }
            } else if let (Some(file), Some(sections)) = (value["file"].as_str(), value["manuscript"]["sections"].as_array()) {
                let title = value["manuscript"]["title"].as_str().unwrap_or("");
                if value["already"] == true { format!("「{file}」已经是稿件《{title}》") } else { format!("已导入「{file}」：{} 节", sections.len()) }
            } else if let (Some(sections), Some(_)) = (value["sections"].as_array(), value["added"].as_array()) {
                format!("大纲现有 {} 节", sections.len())
            } else if let Some(manuscripts) = value["manuscripts"].as_array() {
                format!("{} 份稿件", manuscripts.len())
            } else if let Some(sections) = value["sections"].as_array() {
                format!("读了 {} 节", sections.len())
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
    call_with(root, name, args, fetch, download, None)
}

pub fn call_with(root: &Path, name: &str, args: &Value, fetch: Fetch, download: Download, publish: Option<crate::notes::Publish>) -> Result<Value, String> {
    checked(name, args)?;
    if name == "create_library" { return local::create_library(root, args).map_err(|e| format!("{e:#}")) }
    if name == "import_library" { return local::import_library(root, args).map_err(|e| format!("{e:#}")) }
    if name == "sync_searches" { return sync_searches(root, args, fetch, crate::time::now_millis()).map_err(|e| format!("{e:#}")) }
    if name == "list_libraries" {
        let all: Vec<Value> = libraries::shelved(root).iter().map(|l| json!({"id": l.id, "name": l.name, "topic": l.topic, "papers": l.paper_count, "autoSyncHours": l.auto_sync_hours})).collect();
        return Ok(json!({"libraries": all}));
    }
    let info = find_library(root, args["library"].as_str().unwrap_or(""))?;
    let mut store = Store::open(&info.dir).map_err(|e| format!("{e:#}"))?;
    call_in_with(&mut store, &info, name, args, fetch, download, publish)
}

/// Run a library tool on a library that is already open.
pub fn call_in(store: &mut Store, info: &LibraryInfo, name: &str, args: &Value, fetch: Fetch, download: Download) -> Result<Value, String> {
    call_in_with(store, info, name, args, fetch, download, None)
}

pub fn call_in_with(store: &mut Store, info: &LibraryInfo, name: &str, args: &Value, fetch: Fetch, download: Download, publish: Option<crate::notes::Publish>) -> Result<Value, String> {
    checked(name, args)?;
    if name == "save_note_to_notion" {
        return publish.ok_or("此运行环境没有提供 Notion MCP；请使用桌面应用或 wisp 命令。")?(store, info, args);
    }
    dispatch(store, info, name, args, fetch, download).map_err(|e| format!("{e:#}"))
}

fn dispatch(store: &mut Store, info: &LibraryInfo, name: &str, args: &Value, fetch: Fetch, download: Download) -> Result<Value> {
    match name {
        "list_note_sources" => crate::notes::sources(store, text(args, "paperId")?),
        "prepare_note" => Ok(crate::notes::from_args(store, info, args)?.summary()),
        "export_note" => crate::notes::export(&crate::notes::from_args(store, info, args)?, Path::new(text(args, "path")?)),
        "resolve_note_save" => {
            let saved = args["saved"].as_bool().ok_or_else(|| anyhow!("需要 saved=true 或 false。"))?;
            let page = if saved { Some(text(args, "page")?) } else { None };
            store.resolve_note(text(args, "id")?, page)?;
            Ok(json!({"resolved": true}))
        }
        "export_library" => local::export_library(info, args),
        "import_pdf" => local::import_pdf(store, args),
        "import_references" => local::import_references(store, args),
        "import_zotero" => local::import_zotero(store, args),
        "find_pdf" => find_pdf(store, args, fetch, download),
        "import_url" => web::import_url(store, args, fetch, download),
        "find_citations" => find_citations(store, args),
        "find_passages" => find_passages(store, args),
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
        "suggest_field" => suggest_field(store, args),
        "organise_papers" => Ok(organise::report(&store.organise(&organise::parse(args).map_err(|e| anyhow!(e))?)?)),
        "list_cite_styles" => Ok(cite_styles(&data_root(info))),
        "read_cite_style" => {
            let key = text(args, "style")?;
            if key.is_empty() { bail!("需要 style：样式的 key，见 list_cite_styles。") }
            Ok(json!({"style": key, "csl": crate::csl::xml_of(key, &data_root(info))?}))
        }
        "try_cite_style" => {
            let root = data_root(info);
            let style = match (text(args, "csl")?, text(args, "style")?) {
                ("", "") => bail!("需要 csl（一份 CSL 定义）或 style（已有样式的 key）。"),
                ("", key) => crate::csl::load(key, &root)?,
                (xml, _) => crate::csl::parse(xml, &root)?,
            };
            cite_preview(store, &style, args)
        }
        "save_cite_style" => {
            let (name, xml) = (text(args, "name")?, text(args, "csl")?);
            if name.is_empty() || xml.is_empty() { bail!("需要 name（文件名）和 csl（完整的 CSL 定义）。") }
            let root = data_root(info);
            let saved = crate::csl::save(&root, name, xml)?;
            let mut out = cite_preview(store, &crate::csl::load(&saved.key, &root)?, args)?;
            out["style"] = json!(saved.key);
            out["name"] = json!(saved.name);
            out["next"] = json!("已保存。请告诉用户：在写作页「导出」菜单的引用样式里选它，导出 Word 时就按它写引用。");
            Ok(out)
        }
        "read_docx_citations" => local::read_docx_citations(store, args, fetch),
        "restyle_docx" => local::restyle_docx(store, info, args, fetch),
        "import_docx_references" => local::import_docx_references(store, args, fetch),
        "import_manuscript" => local::import_manuscript(store, args),
        "make_slides" => local::make_slides(store, args),
        "list_manuscripts" => list_manuscripts(store),
        "read_manuscript" => read_manuscript(store, args),
        "comment_manuscript" => comment_manuscript(store, args),
        "edit_section" => crate::compose::edit(store, args),
        "set_outline" => crate::compose::outline(store, args),
        "library_status" => library_status(store, info),
        "offer_action" => crate::offer::offered(args),
        "list_papers" => list_papers(store, info, args),
        "list_figures" => list_figures(store, info, args),
        "list_pdf_annotations" => list_pdf_annotations(store, args),
        "get_paper" => get_paper(store, args),
        "journal_metrics" => journal_metrics(store, args),
        "search_fulltext" => search_fulltext(store, args),
        "read_fulltext" => read_fulltext(store, args),
        _ => Err(anyhow!("没有这个工具：{name}")),
    }
}

/// The library `wanted` names by id or by name; the only one when it is empty. A conversation of the home
/// page not yet made a library (`LibraryInfo::draft`) is found only by its id: it is no library to count or
/// to name, and an agent outside the window has no business there unless it was handed that id.
pub fn find_library(root: &Path, wanted: &str) -> Result<LibraryInfo, String> {
    let wanted = wanted.trim();
    let (drafts, mut all): (Vec<LibraryInfo>, Vec<LibraryInfo>) = libraries::list(root).into_iter().partition(|l| l.draft);
    if let Some(draft) = drafts.into_iter().find(|l| !wanted.is_empty() && l.id == wanted) { return Ok(draft) }
    let names = |all: &[LibraryInfo]| all.iter().map(|l| format!("「{}」", l.name)).collect::<Vec<_>>().join("、");
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
    let fields: Vec<Value> = store.list_fields()?.iter().map(|f| json!({"id": f.id, "name": f.name, "kind": f.kind.as_str(), "options": f.options, "hint": f.hint})).collect();
    Ok(json!({
        "id": info.id,
        "name": info.name,
        "topic": info.topic,
        // A conversation from the home page not yet made a library: the model is not to call it one.
        "draft": info.draft,
        "note": if info.draft { "这是从首页开始的一次对话，还不是文献库：首页上没有它，用户点「建成文献库」后才是。name 是这次对话的名称。" } else { "" },
        // What screening suggestions judge against; empty means by the topic.
        "screeningGoal": info.criteria,
        "folder": info.dir.display().to_string(),
        "papers": papers.len(),
        "inTrash": store.list_trash()?.len(),
        "screening": tally(&papers, &SCREENINGS, |p| &p.screening),
        "reading": tally(&papers, &READINGS, |p| &p.reading),
        "fulltext": tally(&papers, &FULLTEXTS, |p| &p.fulltext),
        "fields": fields,
        // The tags in use with how many papers carry each, so an agent that tags takes one that is there.
        "tags": crate::filter::tag_counts(&papers).into_iter().map(|(tag, count)| json!({"tag": tag, "papers": count})).collect::<Vec<_>>(),
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

fn list_figures(store: &mut Store, info: &LibraryInfo, args: &Value) -> Result<Value> {
    let (words, tag, paper) = (crate::figure::search_words(text(args, "query")?), text(args, "tag")?, text(args, "paper")?);
    let (limit, offset) = (number(args, "limit", 50, 500)?, number(args, "offset", 0, usize::MAX)?);
    let papers = store.list_papers()?;
    let by_id: BTreeMap<&str, &Paper> = papers.iter().map(|p| (p.id.as_str(), p)).collect();
    let figures = store.list_figures()?;
    let found: Vec<&crate::figure::Figure> = figures
        .iter()
        .filter(|f| (tag.is_empty() || f.tags.iter().any(|t| t == tag)) && (paper.is_empty() || f.paper_id == paper) && f.matches(by_id.get(f.paper_id.as_str()).copied(), &words))
        .collect();
    let page: Vec<Value> = found
        .iter()
        .skip(offset)
        .take(limit)
        .map(|f| {
            let source = by_id.get(f.paper_id.as_str()).map(|p| json!({"id": p.id, "title": p.title, "authors": p.short_authors(), "year": p.year}));
            json!({"id": f.id, "label": f.label, "legend": f.legend, "tags": f.tags, "notes": f.notes, "paper": source, "drawn": f.drawn(), "page": (f.page > 0).then_some(f.page),
                "width": f.width, "height": f.height, "file": store.figure_path(f), "addedAt": f.added_at})
        })
        .collect();
    Ok(json!({"library": info.name, "total": found.len(), "offset": offset, "figures": page, "tags": crate::figure::tag_counts(&figures).into_iter().map(|(tag, count)| json!({"tag": tag, "count": count})).collect::<Vec<_>>()}))
}

fn list_pdf_annotations(store: &Store, args: &Value) -> Result<Value> {
    let (paper, resource, kind) = (text(args, "paperId")?, text(args, "resourceId")?, text(args, "kind")?);
    if !["", "underline", "highlight", "box", "note", "cut"].contains(&kind) { bail!("不支持的标注类型：{kind}") }
    let page = number(args, "page", 0, usize::MAX)?;
    if !args["page"].is_null() && page == 0 { bail!("页码从 1 开始。") }
    let words = crate::figure::search_words(text(args, "query")?);
    let annotations: Vec<_> = store.pdf_annotations(paper, resource)?.into_iter().filter(|a| {
        (kind.is_empty() || a.kind.key() == kind) && (page == 0 || a.page == page)
            && words.iter().all(|word| format!("{}\n{}", a.quote, a.note).to_lowercase().contains(word.as_str()))
    }).collect();
    let total = annotations.len();
    let offset = number(args, "offset", 0, usize::MAX)?;
    let limit = number(args, "limit", 50, 500)?;
    let annotations: Vec<_> = annotations.into_iter().skip(offset).take(limit).collect();
    Ok(json!({"total": total, "offset": offset, "annotations": annotations}))
}

/// A journal's metrics from the table the app carries, by its name, its ISSN or a paper of it.
fn journal_metrics(store: &mut Store, args: &Value) -> Result<Value> {
    let asked = args["journal"].as_str().map(str::trim).unwrap_or("");
    let name = match args["paperId"].as_str().filter(|id| asked.is_empty() && !id.is_empty()) {
        Some(id) => store.get_paper(id)?.journal,
        None => asked.to_string(),
    };
    if name.is_empty() { bail!("需要参数 journal（刊名或 ISSN）或 paperId；这篇文献没有期刊名时也查不了。") }
    let table = crate::journals::table();
    let found = table.by_issn(&name).filter(|_| crate::journals::issn_key(&name).len() == 8).or_else(|| table.find(&name));
    Ok(match found {
        Some(journal) => json!({"found": true, "asked": name, "journal": journal, "summary": journal.summary(), "editions": crate::journals::EDITIONS}),
        None => json!({"found": false, "asked": name, "editions": crate::journals::EDITIONS}),
    })
}

fn get_paper(store: &mut Store, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "id")?)?;
    let mut out = serde_json::to_value(&paper)?;
    // Custom fields by the name a person gave them, in the library's order.
    let (mut fields, mut proposed) = (Map::new(), vec![]);
    let proposals = extract::Proposals::new(store.list_proposals()?);
    for field in store.list_fields()? {
        // What an assistant has proposed for a field and the person has yet to look at.
        if let Some(proposal) = proposals.get(&paper.id, &field.id).filter(|proposal| extract::standing(proposal, &field, &paper) == extract::Standing::Open) {
            proposed.push(json!({"field": field.name, "values": proposal.values, "excerpt": proposal.excerpt, "page": proposal.page, "author": proposal.author}));
        }
        if let Some(values) = paper.fields.get(&field.id) { fields.insert(field.name, json!(values)); }
    }
    out["fields"] = Value::Object(fields);
    out["fieldProposals"] = json!(proposed);
    out["link"] = json!(paper.link());
    out["journalMetrics"] = json!(crate::journals::find(&paper.journal));
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
    out["pdfAnnotationCount"] = json!(store.pdf_annotations(&paper.id, "")?.len());
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
    // A table's cells or the note under it are no sentence to write by, whenever they were kept.
    let found: Vec<crate::store::PaperSentence> = found.into_iter().filter(|s| !crate::structure::apparatus(&s.sentence.text)).collect();
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
    fetch_pdf_from(store, paper, None, look_only, fetch, download)
}

/// `fetch_pdf`, with an address to try before the indexes are asked: the PDF a publisher's page names
/// (`import_url`). What came of it is among the reasons when nothing else is had either.
fn fetch_pdf_from(store: &mut Store, paper: &Paper, first: Option<openaccess::Candidate>, look_only: bool, fetch: Fetch, download: Download) -> Result<Value> {
    let mut row = json!({"paperId": paper.id, "title": paper.title});
    let mut failures = vec![];
    if let Some(first) = first.as_ref().filter(|_| !look_only) && try_candidate(store, paper, first, download, &mut row, &mut failures)? { return Ok(row) }
    // A caller with a time to keep (`sources::within`: a pasted address) goes no further once it is up.
    let (mut candidates, notes) = if sources::out_of_time(&mut failures) { (vec![], vec![]) } else { openaccess::candidates(paper, fetch) };
    failures.extend(notes);
    if let Some(first) = &first { candidates.retain(|candidate| candidate.url != first.url) }
    let listed: Vec<Value> = candidates.iter().map(|c| json!({"url": c.url, "source": c.source, "version": c.version})).collect();
    if look_only {
        row["outcome"] = json!(if candidates.is_empty() { "not_found" } else { "candidates" });
        row["candidates"] = json!(listed);
        if !failures.is_empty() { row["reason"] = json!(sources::reasons(&failures)) }
        return Ok(row);
    }
    for candidate in &candidates {
        if sources::out_of_time(&mut failures) { break }
        if try_candidate(store, paper, candidate, download, &mut row, &mut failures)? { return Ok(row) }
    }
    let tried = first.is_some() || !candidates.is_empty();
    row["outcome"] = json!(if tried { "failed" } else { "not_found" });
    // A file is listed and was not handed out: some publishers give it to a browser only.
    if tried { failures.push("拿不到文件时，可以在浏览器里打开文献的链接下载，再把 PDF 拖进窗口".into()) }
    if !tried && failures.is_empty() { failures.push("没有找到开放获取的全文。可以打开文献的链接自己下载，再把 PDF 拖进窗口：它会附到这一条上。".into()) }
    row["reason"] = json!(sources::reasons(&failures));
    Ok(row)
}

/// Download one candidate and keep it with the paper if it is its full text; `false`, with the reason among
/// `failures`, when it is not.
fn try_candidate(store: &mut Store, paper: &Paper, candidate: &openaccess::Candidate, download: Download, row: &mut Value, failures: &mut Vec<String>) -> Result<bool> {
    match download(&candidate.url) {
        Ok(bytes) if openaccess::is_pdf(&bytes) => {
            // Read before it is kept: a repository lists a paper's supplement among its files, and that is not its full text.
            let pages = fulltext::pdf_pages(&bytes);
            if pages.as_ref().is_ok_and(|pages| openaccess::supplement(pages)) {
                failures.push(format!("{}：给出的文件是补充材料（附图、附表），不是正文，没有保存", candidate.source));
                return Ok(false);
            }
            let file = store.import_fetched(&paper.id, &file_name(paper), &bytes, "fulltext", &candidate.url, &candidate.source, candidate.version)?;
            // Saved is not yet readable: what came of reading its text is kept and said.
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
            Ok(true)
        }
        Ok(_) => {
            failures.push(format!("{}：返回的不是 PDF，多半是登录页或验证页", candidate.source));
            Ok(false)
        }
        Err(err) => {
            failures.push(format!("{}：{err}", candidate.source));
            Ok(false)
        }
    }
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

/// The papers without readable text that a search names; the rest are counted.
const UNREADABLE_LISTED: usize = 20;

fn find_citations(store: &mut Store, args: &Value) -> Result<Value> {
    let text = text(args, "text")?;
    if text.is_empty() { bail!("请提供要配引用的文字。") }
    let per_sentence = number(args, "perSentence", 3, 10)?.max(1);
    // Search words for the sentences, in their order: what finds papers written in another language than the text.
    let extra: Vec<String> = match &args["queries"] {
        Value::Null => vec![],
        Value::Array(items) => items.iter().map(|item| item.as_str().unwrap_or("").trim().to_string()).collect(),
        _ => bail!("queries 应是一组字符串，第 n 个是第 n 句的检索词。"),
    };
    // What each sentence does, in their order: it decides what of a paper is shown for it.
    let intents: Vec<&str> = match &args["intents"] {
        Value::Null => vec![],
        Value::Array(items) => items.iter().map(|item| crate::citefind::intent_of(item.as_str().unwrap_or(""))).collect(),
        _ => bail!("intents 应是一组字符串，第 n 个说明第 n 句在做什么。"),
    };
    extract_missing(store, None)?;
    let papers: Vec<Paper> = store.list_papers()?.into_iter().filter(|p| p.screening != "irrelevant").collect();
    let known: std::collections::HashSet<&str> = papers.iter().map(|p| p.id.as_str()).collect();
    // What a paper says of its own: the abstract its record has, and every sentence kept of its full text.
    let mut held: Vec<crate::store::PaperSentence> = papers
        .iter()
        .flat_map(|p| crate::structure::split_sentences(&p.abstract_text).into_iter().filter(|s| crate::structure::usable(s)).enumerate().map(|(position, text)| crate::store::PaperSentence { paper_id: p.id.clone(), sentence: crate::structure::Sentence { section: "abstract".into(), position, text, ..Default::default() } }).collect::<Vec<_>>())
        .collect();
    let stored: Vec<crate::store::PaperSentence> = store.section_sentences("", "")?.into_iter().filter(|s| known.contains(s.paper_id.as_str()) && s.sentence.section != "abstract" && !crate::structure::apparatus(&s.sentence.text)).collect();
    let with_text: std::collections::HashSet<&str> = stored.iter().map(|s| s.paper_id.as_str()).collect();
    let without_text = papers.iter().filter(|p| !with_text.contains(p.id.as_str())).count();
    held.extend(stored);
    let all = crate::citefind::sentences(text);
    let asked: Vec<&str> = all.iter().take(crate::citefind::MAX_SENTENCES).map(|range| &text[range.clone()]).collect();
    let queries: Vec<Vec<String>> = asked
        .iter()
        .enumerate()
        .map(|(n, sentence)| if crate::citefind::worth(sentence) { std::iter::once(sentence.to_string()).chain(extra.get(n).filter(|words| !words.is_empty()).cloned()).collect() } else { vec![] })
        .collect();
    let found = crate::citefind::search(&papers, &held, &queries, &intents, per_sentence);
    let out: Vec<Value> = asked
        .iter()
        .zip(&found)
        .enumerate()
        .map(|(n, (sentence, hits))| {
            let candidates: Vec<Value> = hits
                .iter()
                .map(|hit| {
                    let paper = &papers[hit.paper];
                    let mut candidate = json!({"paperId": paper.id, "title": paper.title, "year": paper.year, "cite": paper.citation(), "matched": hit.matched, "of": hit.of, "in": "record"});
                    // The sentence of the paper's own that says most of it, and where it stands.
                    if let Some(at) = hit.sentence {
                        let own = &held[at].sentence;
                        candidate["in"] = json!(own.section);
                        candidate["snippet"] = json!(own.text);
                        if own.page > 0 { candidate["page"] = json!(own.page) }
                        // The works that sentence names: where it only tells what others found, they are the source, and the library may hold them.
                        let names: Vec<Value> = crate::citefind::named(&own.text)
                            .iter()
                            .map(|named| {
                                let mut name = json!({"text": named.text});
                                if let Some(other) = crate::citefind::holds(&papers, named).filter(|other| *other != hit.paper).map(|other| &papers[other]) {
                                    (name["paperId"], name["title"], name["year"], name["cite"]) = (json!(other.id), json!(other.title), json!(other.year), json!(other.citation()));
                                }
                                name
                            })
                            .collect();
                        if !names.is_empty() { candidate["names"] = json!(names) }
                    }
                    if hit.review { candidate["review"] = json!(true) }
                    candidate
                })
                .collect();
            let mut row = json!({"sentence": sentence, "candidates": candidates});
            if let Some(intent) = intents.get(n).filter(|intent| !intent.is_empty()) { row["intent"] = json!(intent) }
            row
        })
        .collect();
    Ok(json!({
        "sentences": out,
        "sentencesSkipped": all.len().saturating_sub(crate::citefind::MAX_SENTENCES),
        "papersSearched": papers.len(),
        "papersWithoutText": without_text,
        // What search words are to be written in, for a text in another language.
        "language": crate::citefind::library_language(&papers),
    }))
}

/// The sentences of one paper closest to sentences of a manuscript that cite it (`passage`).
fn find_passages(store: &mut Store, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "paperId")?)?;
    let strings = |key: &str, what: &str| -> Result<Vec<String>> {
        match &args[key] {
            Value::Null => Ok(vec![]),
            Value::Array(items) => Ok(items.iter().map(|item| item.as_str().unwrap_or("").trim().to_string()).collect()),
            _ => bail!("{key} 应是一组字符串：{what}。"),
        }
    };
    let texts = strings("texts", "稿件里引用这篇文献的句子")?;
    if texts.iter().all(String::is_empty) { bail!("请在 texts 里提供稿件里引用这篇文献的句子。") }
    let extra = strings("queries", "第 n 个是第 n 句的检索词")?;
    let limit = number(args, "limit", 5, 10)?.max(1);
    extract_missing(store, Some(&paper.id))?;
    let held = crate::passage::sentences_of(&paper, &store.section_sentences("", &paper.id)?);
    let results: Vec<Value> = texts
        .iter()
        .enumerate()
        .map(|(n, text)| {
            let queries: Vec<String> = std::iter::once(text.clone()).chain(extra.get(n).cloned()).filter(|query| !query.is_empty()).collect();
            let candidates: Vec<Value> = crate::passage::nearest(&held, &queries, limit)
                .into_iter()
                .map(|(at, fair)| {
                    let own = &held[at];
                    let mut candidate = json!({"text": own.text, "in": own.part, "fair": fair});
                    if own.page > 0 { candidate["page"] = json!(own.page) }
                    candidate
                })
                .collect();
            json!({"text": text, "candidates": candidates})
        })
        .collect();
    Ok(json!({"paperId": paper.id, "title": paper.title, "year": paper.year, "sentences": held.len(), "results": results}))
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
    if found.asked.iter().all(|asked| !asked.error.is_empty()) { bail!("{}", sources::reasons(&found.asked.iter().map(|asked| format!("{}：{}", asked.source, asked.error)).collect::<Vec<_>>())) }
    let placeholder = complete::placeholder_title(&paper, &files);
    let mut candidates = vec![];
    for candidate in &found.candidates {
        let record = &candidate.paper;
        let held = store.existing_paper(record)?.filter(|id| *id != paper.id);
        candidates.push(json!({
            "title": record.title, "authors": record.authors, "year": record.year, "journal": record.journal, "volume": record.volume, "issue": record.issue, "pages": record.pages, "status": record.publication_status, "doi": record.doi, "pmid": record.pmid,
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
    let trashed: std::collections::HashSet<String> = store.list_trash()?.into_iter().map(|entry| entry.paper.id).collect();
    for mut paper in found.papers {
        paper.normalize();
        // What the library already holds for this record, and what its owner decided about it.
        let existing = match store.existing_paper(&paper)? {
            Some(id) => {
                known += 1;
                let held = store.get_paper(&id)?;
                json!({"id": held.id, "screening": held.screening, "inTrash": trashed.contains(&held.id)})
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
/// that fails is recorded too, so the search shows what went wrong; one
/// that stopped after some pages imports those and says so (`stopped`).
/// Once a run has gone through the newest records the search asks for, the
/// next ones fetch only what the library does not hold (`sources::Known`).
fn run_search(store: &mut Store, id: &str, fetch: Fetch) -> Result<Value> {
    let search = store.list_searches()?.into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("检索式不存在：{id}"))?;
    let kind_name = sources::KINDS.iter().find(|(key, _)| *key == search.kind).map_or(search.kind.as_str(), |(_, name)| name);
    let label = format!("{kind_name}：{}", search.query);
    // The last run stands for the list as it is in the library when it went this deep with this query and nothing of it was taken back.
    let since = search.last_run.as_ref().filter(|run| run.error.is_empty() && run.undone_at.is_empty() && run.label == label && run.depth >= search.max_results).map(|run| run.remote_total.unwrap_or(u64::MAX));
    let holds = |paper: &Paper| {
        let mut paper = paper.clone();
        paper.normalize();
        store.existing_paper(&paper).is_ok_and(|held| held.is_some())
    };
    let found = match sources::search_new(&search.kind, &search.query, search.max_results, fetch, Some(&sources::Known { holds: &holds, since })) {
        Ok(found) => found,
        Err(error) => {
            store.record_failed_run(&search.id, &label, &error)?;
            bail!(error)
        }
    };
    let stopped = found.stopped.clone();
    let (batch, result) = store.import_batch(&search.id, &label, found, search.max_results)?;
    let added: Vec<Value> = result.added.iter().take(50).map(|p| json!({"id": p.id, "title": p.title, "year": p.year})).collect();
    let mut run = json!({"batchId": batch.id, "search": search.name, "total": batch.remote_total, "fetched": batch.fetched, "added": batch.added, "skipped": batch.skipped, "addedPapers": added});
    if let Some(why) = stopped { run["stopped"] = json!(format!("取回 {} 篇后中断，这些已经导入；再运行一次会接着取回其余的。原因：{why}", batch.fetched)) }
    Ok(run)
}

fn undo_import(store: &mut Store, args: &Value) -> Result<Value> {
    let undone = store.undo_batch(text(args, "batchId")?)?;
    let mut result = json!({"removed": undone.removed, "kept": undone.kept.iter().map(|(id, title)| json!({"id": id, "title": title})).collect::<Vec<_>>()});
    if !undone.set_aside.is_empty() { result["setAside"] = json!(store.root().join(&undone.set_aside)) }
    Ok(result)
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
    let all = if wanted.is_empty() { libraries::shelved(root) } else { vec![find_library(root, wanted).map_err(|e| anyhow!(e))?] };
    let mut runs = vec![];
    let (mut added, mut failed) = (0, 0);
    for info in all.iter().filter(|info| force || info.auto_sync_hours > 0) {
        // A library that cannot be opened (written by a newer build, damaged) does not stop the others either.
        let (mut store, searches) = match Store::open(&info.dir).and_then(|store| store.list_searches().map(|searches| (store, searches))) {
            Ok(opened) => opened,
            Err(error) => {
                failed += 1;
                runs.push(json!({"library": info.name, "libraryId": info.id, "error": format!("{error:#}")}));
                continue;
            }
        };
        let due: Vec<String> = if force { searches.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect() } else { due_searches(&searches, info.auto_sync_hours, now_ms).iter().map(|s| s.id.clone()).collect() };
        for id in due {
            // One search failing does not stop the others.
            match run_search(&mut store, &id, fetch) {
                Ok(run) => {
                    added += run["added"].as_u64().unwrap_or(0);
                    // A run that stopped short counts as failed: it is due again within the hour.
                    if run["stopped"].is_string() { failed += 1 }
                    let mut entry = json!({"library": info.name, "libraryId": info.id, "search": run["search"], "added": run["added"], "skipped": run["skipped"], "batchId": run["batchId"]});
                    if run["stopped"].is_string() { entry["stopped"] = run["stopped"].clone() }
                    runs.push(entry);
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

// ---- extraction

/// What a call would do, a line per thing it changes, for the person who
/// is asked whether it may run; an error when it cannot run as written.
/// A saved search is named by its id in the call: the card says what it asks of which database.
pub fn preview_call(store: &Store, name: &str, args: &Value) -> Result<Vec<String>, String> {
    match name {
        "run_search" | "remove_search" => {
            let id = text(args, "id").map_err(|e| e.to_string())?;
            let search = store.list_searches().map_err(|e| format!("{e:#}"))?.into_iter().find(|s| s.id == id).ok_or_else(|| format!("检索式不存在：{id}"))?;
            let source = sources::KINDS.iter().find(|(key, _)| *key == search.kind).map_or(search.kind.as_str(), |(_, name)| name);
            let title = if search.name.trim().is_empty() || search.name.trim() == source { source.to_string() } else { format!("{} · {source}", search.name.trim()) };
            let detail = if name == "run_search" { format!("{}（最多取最新的 {} 条）", search.query, search.max_results) } else { search.query.clone() };
            Ok(vec![format!("《{title}》：{detail}")])
        }
        "export_note" | "save_note_to_notion" => {
            let info = std::fs::read_to_string(store.root().join(libraries::INFO_FILE)).ok().and_then(|s| serde_json::from_str::<LibraryInfo>(&s).ok()).unwrap_or_default();
            let doc = crate::notes::from_args(store, &info, args).map_err(|e| e.to_string())?;
            let destination = if name == "save_note_to_notion" { format!("Notion 父页面：{}", text(args, "parent").map_err(|e| e.to_string())?) } else { format!("本地文件：{}", text(args, "path").map_err(|e| e.to_string())?) };
            Ok(vec![destination, format!("标题：{}", doc.title), format!("正文 {} 字，{} 张图片；原始笔记不变。", doc.markdown.chars().count(), doc.used_attachments().count()), doc.markdown.chars().take(2000).collect()])
        }
        "resolve_note_save" => Ok(vec![format!("仅在已手动核对 Notion 后确认：{}。", if args["saved"] == true { "笔记已成功写入" } else { "完全没有写入（不是部分写入）" }), format!("记录：{}；页面：{}", args["id"].as_str().unwrap_or(""), args["page"].as_str().unwrap_or(""))]),
        "edit_section" | "set_outline" => crate::compose::preview(store, name, args).map_err(|e| format!("{e:#}")),
        "import_manuscript" => local::preview_manuscript(store, args).map_err(|e| format!("{e:#}")),
        _ => Ok(vec![]),
    }
}

/// The folder that holds the libraries: the citation styles the person added are kept beside them (`csl::FOLDER`).
fn data_root(info: &LibraryInfo) -> std::path::PathBuf {
    info.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| info.dir.clone())
}

fn cite_styles(root: &Path) -> Value {
    let listed = |styles: Vec<crate::csl::Listed>| styles.into_iter().map(|style| json!({"key": style.key, "name": style.name})).collect::<Vec<_>>();
    json!({"common": listed(crate::csl::common()), "added": listed(crate::csl::added(root)), "builtin": listed(crate::csl::bundled()), "simple": [{"key": "author-year", "name": "作者-年份"}, {"key": "numeric", "name": "编号"}]})
}

/// A style tried on papers of the library: one cited alone, several together, and their entries in the list.
fn cite_preview(store: &mut Store, style: &crate::csl::Loaded, args: &Value) -> Result<Value> {
    let asked: Vec<&str> = args["paperIds"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut papers = store.list_papers()?;
    if asked.is_empty() {
        // The latest few that have what an entry is made of.
        papers.retain(|paper| !paper.authors.trim().is_empty() && paper.year.is_some());
        papers.sort_by(|a, b| b.added_at.cmp(&a.added_at));
        papers.truncate(4);
    } else {
        papers.retain(|paper| asked.contains(&paper.id.as_str()));
    }
    if papers.is_empty() { bail!("文献库里没有可以用来试排的文献（需要有作者和年份）。") }
    let all: Vec<&Paper> = papers.iter().collect();
    let groups: Vec<Vec<&Paper>> = [vec![all[0]], all.clone()].into_iter().take(if all.len() > 1 { 2 } else { 1 }).collect();
    let (cited, list) = crate::csl::render(style, &groups, match text(args, "language")? { "" => "en", language => language });
    let shown = |rich: &crate::cite::Rich| {
        let mut out = json!({"text": rich.text});
        for (name, has) in [("italic", (|look: &crate::docx::Look| look.italic) as fn(&crate::docx::Look) -> bool), ("bold", |look| look.bold), ("superscript", |look| look.sup), ("subscript", |look| look.sub), ("smallCaps", |look| look.small_caps), ("underline", |look| look.underline)] {
            let words: Vec<&str> = rich.looks.iter().filter(|(_, look)| has(look)).map(|(range, _)| &rich.text[range.clone()]).collect();
            if !words.is_empty() { out[name] = json!(words) }
        }
        out
    };
    Ok(json!({
        "citations": cited.iter().zip(&groups).map(|(rich, group)| { let mut out = shown(rich); out["papers"] = json!(group.len()); out }).collect::<Vec<_>>(),
        "references": list.iter().map(|(_, rich)| shown(rich)).collect::<Vec<_>>(),
    }))
}

fn list_manuscripts(store: &mut Store) -> Result<Value> {
    let manuscripts = store.manuscripts()?;
    Ok(json!({
        "manuscripts": manuscripts.iter().map(|m| json!({
            "id": m.id,
            "title": m.shown_title(),
            "language": m.language,
            "updatedAt": m.updated_at,
            "sections": m.sections.iter().map(|s| json!({"id": s.id, "name": s.name, "kind": s.kind, "characters": s.body.chars().count()})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

/// The manuscript `args` names by id or title; the only one when it names none.
fn named_manuscript<'a>(manuscripts: &'a [crate::manuscript::Manuscript], args: &Value) -> Result<&'a crate::manuscript::Manuscript> {
    let wanted = text(args, "manuscript")?;
    Ok(match wanted {
        "" if manuscripts.len() == 1 => &manuscripts[0],
        "" if manuscripts.is_empty() => bail!("写作页还没有稿件。"),
        "" => bail!("有 {} 份稿件，用 manuscript 指定一份：{}", manuscripts.len(), manuscripts.iter().map(|m| format!("「{}」", m.shown_title())).collect::<Vec<_>>().join("、")),
        _ => manuscripts.iter().find(|m| m.id == wanted || m.shown_title() == wanted).ok_or_else(|| anyhow!("没有这份稿件：{wanted}。现有：{}", manuscripts.iter().map(|m| format!("「{}」", m.shown_title())).collect::<Vec<_>>().join("、")))?,
    })
}

/// Comments of the assistant's on words of a section, as a review in Word leaves them: the text is not
/// changed, each proposal waits for the person (`comment.rs`). A quotation the section does not hold
/// leaves no comment and is said back, so the model can quote again.
fn comment_manuscript(store: &mut Store, args: &Value) -> Result<Value> {
    let manuscripts = store.manuscripts()?;
    let manuscript = named_manuscript(&manuscripts, args)?;
    let wanted = text(args, "section")?;
    let Some(section) = manuscript.sections.iter().find(|s| s.id == wanted || s.name == wanted) else {
        bail!("这份稿件没有名为「{wanted}」的节。现有：{}", manuscript.sections.iter().map(|s| format!("「{}」", s.name)).collect::<Vec<_>>().join("、"))
    };
    let Some(items) = args["remarks"].as_array().filter(|items| !items.is_empty()) else { bail!("需要 remarks：至少一条批注。") };
    let (remarks, missed) = crate::comment::remarks_in(items, &section.body);
    let made = store.add_remarks(&manuscript.id, &section.id, &remarks)?;
    Ok(json!({
        "commented": made.len(),
        "missed": missed,
        "manuscript": manuscript.id,
        "section": {"id": section.id, "name": section.name},
        "next": "批注已留在稿件上，用户在窗口里逐条采纳或忽略；导出 Word 时写成 Word 的批注。不要把同样的意见再贴进对话。",
    }))
}

fn read_manuscript(store: &mut Store, args: &Value) -> Result<Value> {
    let manuscripts = store.manuscripts()?;
    let manuscript = named_manuscript(&manuscripts, args)?;
    let section = text(args, "section")?;
    // The comments that still wait: those a Word file came with, the person's and the assistant's.
    let comments = store.comments(&manuscript.id)?;
    // Where its citations are from: the passages kept for them, by the sentence that cites.
    let passages = store.passages(&manuscript.id)?;
    let sections: Vec<_> = if section.is_empty() {
        manuscript.sections.iter().filter(|s| !s.body.trim().is_empty()).collect()
    } else {
        let found: Vec<_> = manuscript.sections.iter().filter(|s| s.id == section || s.name == section).collect();
        if found.is_empty() { bail!("这份稿件没有名为「{section}」的节。现有：{}", manuscript.sections.iter().map(|s| format!("「{}」", s.name)).collect::<Vec<_>>().join("、")) }
        found
    };
    Ok(json!({
        "id": manuscript.id,
        "title": manuscript.shown_title(),
        "language": manuscript.language,
        "sections": sections.iter().map(|s| json!({"id": s.id, "name": s.name, "kind": s.kind, "body": s.body, "comments": comments.iter().filter(|c| c.section_id == s.id && c.state != crate::comment::DONE).map(|c| json!({"quote": c.quote, "comment": c.body, "by": c.author, "from": c.origin, "proposes": c.suggestion})).collect::<Vec<_>>(), "passages": passages.iter().filter(|p| p.section_id == s.id && p.found()).map(|p| json!({"paperId": p.paper_id, "sentence": p.claim, "passage": p.passage, "in": p.part, "page": p.page, "from": p.origin})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

/// Record what an agent thinks a custom field of a paper should hold. The
/// field itself is the person's to write and is not touched here; a
/// quotation the paper's text does not hold is not kept as evidence, and a
/// Select field is proposed one of its options or nothing.
fn suggest_field(store: &mut Store, args: &Value) -> Result<Value> {
    let paper = store.get_paper(text(args, "id")?)?;
    let wanted = text(args, "field")?;
    let fields = store.list_fields()?;
    let Some(field) = fields.iter().find(|f| f.id == wanted || f.name == wanted) else {
        if fields.is_empty() { bail!("这个文献库还没有自定义词条。") }
        bail!("没有名为「{wanted}」的词条。现有：{}", fields.iter().map(|f| format!("「{}」", f.name)).collect::<Vec<_>>().join("、"))
    };
    let values = extract::coerce(field, &[text(args, "value")?.to_string()]);
    if !values.is_empty() && !extract::fits(field, &values) { bail!("「{}」是单选词条，取值应为 {} 之一；文中没有合适的就把 value 留空。", field.name, field.options.join("、")) }
    extract_missing(store, Some(&paper.id))?;
    let pages = store.paper_pages(&paper.id)?;
    let quote = text(args, "excerpt")?;
    let found = if quote.is_empty() || values.is_empty() { None } else { crate::ai::locate_quote(quote, &paper, pages.as_deref()) };
    let author = text(args, "author")?;
    let mut proposal = Proposal {
        paper_id: paper.id.clone(),
        field_id: field.id.clone(),
        values,
        excerpt: if found.is_some() { quote.into() } else { String::new() },
        page: found.flatten(),
        basis: if pages.is_some() { "fulltext" } else if paper.abstract_text.is_empty() { "none" } else { "abstract" }.into(),
        author: if author.is_empty() { "agent".into() } else { author.into() },
        asked: extract::asked_as(field),
        ..Default::default()
    };
    store.save_proposal(&mut proposal)?;
    Ok(json!({
        "saved": true,
        "paperId": paper.id,
        "field": field.name,
        "values": proposal.values,
        "excerptKept": quote.is_empty() || found.is_some(),
        "page": proposal.page,
        // Said every time: an agent that reads this knows the field is as it was.
        "current": paper.fields.get(&field.id).cloned().unwrap_or_default(),
        "note": "建议已保存，词条没有改动，由用户在提取页确认。",
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
            // A list is written as JSON, or as one value for a list of one.
            "array" | "object" => serde_json::from_str::<Value>(&value).ok().filter(|parsed| parsed.is_array() || parsed.is_object()).unwrap_or_else(|| json!([value])),
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
    fn pdf_annotations_are_readable_searchable_and_scoped_through_tools_and_mcp() {
        use crate::annotation::Kind;
        let (root, paper) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let source = root.join("annotated.pdf");
        std::fs::write(&source, b"%PDF-1.4 test").unwrap();
        let file = store.import_file(&paper, &source, "fulltext").unwrap();
        let first = store.add_pdf_annotation(&paper, &file.id, 1, Kind::Underline, "Root growth", &[[10., 20., 100., 30.]]).unwrap();
        store.update_pdf_annotation_note(&file.id, &first.id, "Compare WOX5 expression").unwrap();
        store.add_pdf_annotation(&paper, &file.id, 2, Kind::Highlight, "Cell identity", &[[10., 40., 100., 50.]]).unwrap();
        let all = call(&root, "list_pdf_annotations", &json!({"paperId": paper})).unwrap();
        assert_eq!(all["total"], 2);
        assert_eq!(call(&root, "get_paper", &json!({"id": paper})).unwrap()["pdfAnnotationCount"], 2);
        let found = call(&root, "list_pdf_annotations", &json!({"query": "growth wox5", "resourceId": file.id})).unwrap();
        assert_eq!(found["total"], 1);
        assert_eq!(found["annotations"][0]["note"], "Compare WOX5 expression");
        assert_eq!(found["annotations"][0]["quote"], "Root growth");
        assert_eq!(found["annotations"][0]["boxes"], json!([[10., 20., 100., 30.]]));
        assert_eq!(found["annotations"][0]["color"], "");
        let highlight = store.pdf_annotations(&paper, &file.id).unwrap().into_iter().find(|a| a.kind == Kind::Highlight).unwrap();
        store.recolor_pdf_annotation(&file.id, &highlight.id, "blue").unwrap();
        let highlights = call(&root, "list_pdf_annotations", &json!({"kind": "highlight", "page": 2})).unwrap();
        assert_eq!((&highlights["total"], &highlights["annotations"][0]["color"]), (&json!(1), &json!("blue")));
        assert_eq!(call(&root, "list_pdf_annotations", &json!({"offset": 1, "limit": 1})).unwrap()["annotations"][0]["page"], 2);
        for args in [json!({"paperId": "missing"}), json!({"resourceId": "missing"}), json!({"page": 3})] {
            assert_eq!(call(&root, "list_pdf_annotations", &args).unwrap()["total"], 0);
        }
        assert!(call(&root, "list_pdf_annotations", &json!({"page": 0})).is_err());
        assert!(call(&root, "list_pdf_annotations", &json!({"kind": "wrong"})).is_err());
        // The same tool is available to the built-in assistant and a read-only MCP client.
        assert!(library_catalog().iter().any(|t| t.name == "list_pdf_annotations" && !t.writes));
        let server = crate::mcp::Server { fetch: &offline, download: &no_download, publish: None, read_only: true };
        let response = crate::mcp::handle(&root, &json!({"id": 1, "method": "tools/call", "params": {"name": "list_pdf_annotations", "arguments": {"paperId": paper}}}), &server).unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["structuredContent"]["total"], 2);
        store.trash_paper(&paper).unwrap();
        assert_eq!(call(&root, "list_pdf_annotations", &json!({})).unwrap()["total"], 0);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn figures_are_listed_with_their_source() {
        let (root, atlas) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x02\x80\x00\x00\x01\xe0\x08\x02\x00\x00\x00";
        let mut niche = crate::figure::Figure { paper_id: atlas.clone(), page: 2, label: "Fig. 1".into(), legend: "The quiescent centre under the confocal microscope.".into(), tags: vec!["confocal".into()], ..Default::default() };
        store.add_figure(&mut niche, png).unwrap();
        let mut blot = crate::figure::Figure { paper_id: atlas.clone(), label: "Fig. 3".into(), legend: "Western blot.".into(), tags: vec!["western".into(), "confocal".into()], ..Default::default() };
        store.add_figure(&mut blot, png).unwrap();
        drop(store);
        let all = call(&root, "list_figures", &json!({})).unwrap();
        assert_eq!((all["total"].as_u64(), all["tags"][0]["tag"].as_str(), all["tags"][0]["count"].as_u64()), (Some(2), Some("confocal"), Some(2)));
        // By its own words, by its paper's, by tag; with the page and the paper it leads back to.
        let found = call(&root, "list_figures", &json!({"query": "quiescent zhang"})).unwrap();
        let figure = &found["figures"][0];
        assert_eq!((found["total"].as_u64(), figure["id"].as_str(), figure["page"].as_u64(), figure["paper"]["title"].as_str(), figure["width"].as_u64()), (Some(1), Some(niche.id.as_str()), Some(2), Some("Root stem cell niche"), Some(640)));
        assert!(Path::new(figure["file"].as_str().unwrap()).is_file());
        let western = call(&root, "list_figures", &json!({"tag": "western"})).unwrap();
        assert_eq!((western["total"].as_u64(), western["figures"][0]["page"].is_null()), (Some(1), true));
        assert_eq!(call(&root, "list_figures", &json!({"paper": "nobody"})).unwrap()["total"], 0);
        assert_eq!(call(&root, "list_figures", &json!({"limit": 1, "offset": 1})).unwrap()["figures"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(root).ok();
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
    fn a_conversation_not_yet_made_a_library_is_seen_only_by_its_id() {
        let (root, _) = sample();
        let draft = libraries::create_draft(&root, "根毛").unwrap();
        // Not listed, not counted: the one library on the shelf still needs no name.
        let listed = call(&root, "list_libraries", &json!({})).unwrap();
        assert_eq!(listed["libraries"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect::<Vec<_>>(), ["根"]);
        assert_eq!(call(&root, "library_status", &json!({})).unwrap()["papers"], 2);
        // Its name does not find it, and is not offered; its id does.
        let refused = call(&root, "library_status", &json!({"library": "根毛"})).unwrap_err();
        assert!(refused.ends_with("现有：「根」"), "{refused}");
        let status = call(&root, "library_status", &json!({"library": draft.id})).unwrap();
        assert_eq!((status["papers"].as_u64(), status["draft"].as_bool()), (Some(0), Some(true)));
        assert!(status["note"].as_str().unwrap().contains("还不是文献库"));
        let shelved = call(&root, "library_status", &json!({})).unwrap();
        assert_eq!((shelved["draft"].as_bool(), shelved["note"].as_str()), (Some(false), Some("")));
        // A data folder holding only conversations has no library.
        let alone = std::env::temp_dir().join(format!("wisp-tools-{}", new_id("T")));
        libraries::create_draft(&alone, "x").unwrap();
        assert!(find_library(&alone, "").unwrap_err().contains("还没有文献库"));
        assert!(call(&alone, "list_libraries", &json!({})).unwrap()["libraries"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(alone).ok();
    }

    #[test]
    fn remarks_on_a_manuscript_are_kept_where_their_words_are_found() {
        let (root, _) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let manuscript = store.create_manuscript("Root hairs", "en").unwrap();
        let section = manuscript.sections[1].clone();
        store.save_section_body(&section.id, "Root hairs take up water. They grow fast.\n\nEthylene makes them longer.").unwrap();
        drop(store);
        let args = json!({"section": section.name, "remarks": [
            {"quote": "They grow fast.", "comment": "多快？给出数据和出处。", "replacement": "They grow about 1 µm per minute."},
            {"quote": "Ethylene makes them longer", "comment": "需要引用。"},
            {"quote": "Auxin is absent", "comment": "原文里没有这句。"},
        ]});
        let done = call(&root, "comment_manuscript", &args).unwrap();
        assert_eq!((done["commented"].as_u64(), done["missed"].clone()), (Some(2), json!(["Auxin is absent"])));
        assert_eq!(describe_result(&Ok(done)), format!("在「{}」留了 2 条批注，1 条的原文没有找到", section.name));
        let store = Store::open(&info.dir).unwrap();
        let comments = store.comments(&manuscript.id).unwrap();
        assert_eq!(comments.iter().map(|c| (c.quote.as_str(), c.origin.as_str(), c.proposes())).collect::<Vec<_>>(), [("They grow fast.", crate::comment::ASSISTANT, true), ("Ethylene makes them longer", crate::comment::ASSISTANT, false)]);
        // The text itself is the person's to change.
        assert_eq!(store.manuscripts().unwrap()[0].section(&section.id).unwrap().body, "Root hairs take up water. They grow fast.\n\nEthylene makes them longer.");
        assert!(call(&root, "comment_manuscript", &json!({"section": "Nowhere", "remarks": [{"quote": "x", "comment": "y"}]})).unwrap_err().contains("没有名为"));
        assert!(call(&root, "comment_manuscript", &json!({"section": section.id, "remarks": []})).is_err());
        assert!(catalog().iter().any(|tool| tool.name == "comment_manuscript" && tool.writes && !tool.confirm));
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
        // Reasons that end in a full stop are joined without doubling it.
        let refused = |_: &str| -> Result<Vec<u8>, String> { Err("link.springer.com 返回 HTTP 403。".into()) };
        let failed = super::call(&root, "find_pdf", &json!({"paperId": third.id}), &open, &refused).unwrap();
        assert_eq!(failed["papers"][0]["reason"], "Springer：link.springer.com 返回 HTTP 403；拿不到文件时，可以在浏览器里打开文献的链接下载，再把 PDF 拖进窗口。");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_supplement_is_not_kept_as_the_full_text() {
        let (root, _) = sample();
        let mut store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let paper = store.add_paper(Paper { title: "Drought in the mesophyll".into(), doi: "10.1186/drought-1".into(), ..Default::default() }).unwrap();
        drop(store);
        // The repository's file is the paper's supplementary figures; the publisher has the article.
        let index = |url: &str| -> Result<String, String> {
            if url.contains("openalex") { Ok(json!({"open_access": {"is_oa": true}, "best_oa_location": {"is_oa": true, "pdf_url": "https://repo.example/file.pdf", "version": "publishedVersion"}, "locations": []}).to_string()) } else { Ok(json!({"resultList": {"result": []}}).to_string()) }
        };
        let supplement = crate::fulltext::text_pdf(&["Supp. Figure 1. Gene expression changes upon cell wall digestion.\nVolcano plot", "Cluster 1 Cluster 2\nSupp. Figure 2. Functional analysis.", "Supp. Figure 3. Comparison of the profiles."]);
        let article = crate::fulltext::text_pdf(&["Drought in the mesophyll\nAbstract\nLeaves respond to drought.", "Introduction\nSupplementary Figure 1 shows the design."]);
        let only_supplement = |_: &str| -> Result<Vec<u8>, String> { Ok(supplement.clone()) };
        let failed = super::call(&root, "find_pdf", &json!({"paperId": paper.id}), &index, &only_supplement).unwrap();
        assert_eq!((failed["saved"].as_u64(), failed["failed"].as_u64()), (Some(0), Some(1)));
        assert!(failed["papers"][0]["reason"].as_str().unwrap().starts_with("OpenAlex：给出的文件是补充材料（附图、附表），不是正文，没有保存；Springer：给出的文件是补充材料"));
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        assert!(store.paper_files(&paper.id).unwrap().is_empty());
        assert_eq!(store.get_paper(&paper.id).unwrap().fulltext, "linked");
        drop(store);
        // The next address has the article: that one is kept, with its text.
        let files = |url: &str| -> Result<Vec<u8>, String> { Ok(if url.contains("repo.example") { supplement.clone() } else { article.clone() }) };
        let found = super::call(&root, "find_pdf", &json!({"paperId": paper.id}), &index, &files).unwrap();
        assert_eq!((found["papers"][0]["outcome"].as_str(), found["papers"][0]["source"].as_str(), found["papers"][0]["pages"].as_u64()), (Some("saved"), Some("Springer"), Some(2)));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn citations_for_a_text() {
        let (root, id) = sample();
        // A full text long enough to have sentences kept: the methods are on its second page.
        {
            let info = libraries::list(&root).remove(0);
            let mut store = Store::open(&info.dir).unwrap();
            let file = store.paper_files(&id).unwrap().remove(0);
            let pages = vec![
                "Introduction\n\nThe root tip of Arabidopsis holds a small group of stem cells that divide rarely. How this niche is kept through development has been studied for decades.".to_string(),
                "Methods\n\nProtoplasts were isolated from root tips and libraries were prepared with the 10x Genomics Chromium platform. Reads were aligned to the reference genome and counted per gene.".to_string(),
            ];
            store.save_fulltext_pages(&file.id, &Ok(pages)).unwrap();
        }
        let found = call(&root, "find_citations", &json!({"text": "Libraries were prepared with the Chromium platform. WOX5 maintains the quiescent centre of the root. Too short."})).unwrap();
        let sentence = |n: usize| &found["sentences"][n];
        assert_eq!(found["sentences"].as_array().unwrap().len(), 3);
        // Each candidate comes with the sentence of its own that says most of it: here one of the full text, with its page.
        assert_eq!(sentence(0)["candidates"][0]["paperId"].as_str(), Some(id.as_str()));
        assert!(sentence(0)["candidates"][0]["snippet"].as_str().unwrap().contains("libraries were prepared"));
        assert_eq!((sentence(0)["candidates"][0]["in"].as_str(), sentence(0)["candidates"][0]["page"].as_u64()), (Some("methods"), Some(2)));
        // The second is what the abstract says.
        assert_eq!((sentence(1)["candidates"][0]["paperId"].as_str(), sentence(1)["candidates"][0]["page"].as_u64()), (Some(id.as_str()), None));
        assert_eq!(sentence(2)["candidates"], json!([]));
        // The paper marked irrelevant is never a candidate.
        assert_eq!((found["papersSearched"].as_u64(), found["papersWithoutText"].as_u64(), found["language"].as_str()), (Some(1), Some(0), Some("en")));
        assert_eq!(call(&root, "find_citations", &json!({"text": "Unrelated sentence about volcanic eruptions in Iceland."})).unwrap()["sentences"][0]["candidates"], json!([]));
        // A sentence in another language finds the paper by the search words given for it.
        let chinese = "文库用 Chromium 平台构建。";
        assert_eq!(call(&root, "find_citations", &json!({"text": chinese})).unwrap()["sentences"][0]["candidates"], json!([]));
        let helped = call(&root, "find_citations", &json!({"text": chinese, "queries": ["libraries prepared Chromium platform"]})).unwrap();
        assert_eq!(helped["sentences"][0]["candidates"][0]["paperId"].as_str(), Some(id.as_str()));
        assert!(call(&root, "find_citations", &json!({"text": chinese, "queries": "x"})).is_err());
        // What a sentence does is said back; the authors' own words are not searched for, and an intent nobody knows is none.
        let by_intent = call(&root, "find_citations", &json!({"text": "Libraries were prepared with the Chromium platform. Libraries were prepared with the Chromium platform.", "intents": ["method", "own"]})).unwrap();
        assert_eq!((by_intent["sentences"][0]["intent"].as_str(), by_intent["sentences"][0]["candidates"][0]["paperId"].as_str()), (Some("method"), Some(id.as_str())));
        assert_eq!((by_intent["sentences"][1]["intent"].as_str(), &by_intent["sentences"][1]["candidates"]), (Some("own"), &json!([])));
        assert!(call(&root, "find_citations", &json!({"text": chinese, "intents": ["whatever"]})).unwrap()["sentences"][0]["intent"].is_null());
        assert!(call(&root, "find_citations", &json!({"text": chinese, "intents": "x"})).is_err());
        // In one paper: the sentences of its own closest to a sentence that cites it, the full text's with their page.
        let within = call(&root, "find_passages", &json!({"paperId": id, "texts": ["Libraries were built on the Chromium platform from protoplasts.", chinese], "queries": ["", "libraries prepared Chromium platform"]})).unwrap();
        for n in 0..2 {
            let first = &within["results"][n]["candidates"][0];
            assert!(first["text"].as_str().unwrap().starts_with("Protoplasts were isolated"), "{within}");
            assert_eq!((first["in"].as_str(), first["page"].as_u64(), first["fair"].as_bool()), (Some("methods"), Some(2), Some(true)));
        }
        assert!(within["sentences"].as_u64().unwrap() >= 3);
        assert!(call(&root, "find_passages", &json!({"paperId": id, "texts": []})).is_err());
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
        // Like PubMed, only the records asked for.
        let asked: Vec<&str> = url.rsplit("id=").next().unwrap_or("").split(',').collect();
        let records = [
            ("1", "<PubmedArticle><MedlineCitation><PMID>1</PMID><Article><Journal><JournalIssue><PubDate><Year>2024</Year></PubDate></JournalIssue><Title>Plant Cell</Title></Journal><ArticleTitle>Root stem cell niche.</ArticleTitle></Article></MedlineCitation></PubmedArticle>"),
            ("2", "<PubmedArticle><MedlineCitation><PMID>2</PMID><Article><Journal><JournalIssue><PubDate><Year>2025</Year></PubDate></JournalIssue><Title>Nature Plants</Title></Journal><ArticleTitle>A new root atlas</ArticleTitle><Abstract><AbstractText>Cells of the root.</AbstractText></Abstract></Article></MedlineCitation></PubmedArticle>"),
        ];
        Ok(format!("<PubmedArticleSet>{}</PubmedArticleSet>", records.iter().filter(|(id, _)| asked.contains(id)).map(|(_, record)| *record).collect::<String>()))
    }

    #[test]
    fn a_run_that_stops_short_imports_what_came() {
        let (root, _) = sample();
        // Crossref sends the first page and fails at the second.
        let crossref = |url: &str| -> Result<String, String> {
            if !url.contains("offset=0&") { return Err("网络请求失败".into()) }
            let items: Vec<String> = (0..sources::PAGE).map(|n| format!(r#"{{"title":["Paper {n}"],"DOI":"10.1000/{n}"}}"#)).collect();
            Ok(format!(r#"{{"message":{{"total-results":900,"items":[{}]}}}}"#, items.join(",")))
        };
        let online = |name: &str, args: Value| super::call(&root, name, &args, &crossref, &no_download);
        assert!(online("save_search", json!({"source": "crossref", "query": "root", "maxResults": sources::MAX_RESULTS + 1})).is_err());
        let saved = online("save_search", json!({"source": "crossref", "query": "root", "maxResults": 600})).unwrap();
        // The card that asks names the database and the query, not the id the model gave.
        let info = find_library(&root, "").unwrap();
        let card = preview_call(&Store::open(&info.dir).unwrap(), "run_search", &json!({"id": saved["id"]})).unwrap();
        assert_eq!(card, vec!["《Crossref》：root（最多取最新的 600 条）"]);
        assert!(preview_call(&Store::open(&info.dir).unwrap(), "remove_search", &json!({"id": "S-none"})).unwrap_err().contains("检索式不存在"));
        let run = online("run_search", json!({"id": saved["id"]})).unwrap();
        assert_eq!((run["total"].as_u64(), run["fetched"].as_u64(), run["added"].as_u64()), (Some(900), Some(200), Some(200)));
        assert!(run["stopped"].as_str().unwrap().contains("网络请求失败"));
        // The record of the run holds both what came and why no more did, and the batch can be undone.
        let last = call(&root, "list_searches", &json!({})).unwrap()["searches"][0]["lastRun"].clone();
        assert_eq!((last["fetched"].as_u64(), last["error"].as_str()), (Some(200), Some("网络请求失败")));
        assert_eq!(call(&root, "undo_import", &json!({"batchId": run["batchId"]})).unwrap()["removed"], 200);
        // A scheduled look counts it among the failed, so its turn comes again soon.
        let synced = online("sync_searches", json!({"force": true})).unwrap();
        assert_eq!((synced["added"].as_u64(), synced["failed"].as_u64()), (Some(200), Some(1)));
        assert!(synced["runs"][0]["stopped"].is_string());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_later_run_fetches_only_what_is_new() {
        let (root, _) = sample();
        // Crossref holds `total` records and counts what it is asked. Number 1000 is new and stands at place 300.
        let (total, requests) = (std::cell::Cell::new(450), std::cell::Cell::new(0));
        let crossref = |url: &str| -> Result<String, String> {
            requests.set(requests.get() + 1);
            let number = |name: &str| -> usize { url.split(['?', '&']).find_map(|part| part.strip_prefix(name)?.strip_prefix('=')?.parse().ok()).unwrap() };
            let mut list: Vec<usize> = (0..450).collect();
            if total.get() > 450 { list.insert(300, 1000) }
            let items: Vec<String> = list.iter().skip(number("offset")).take(number("rows")).map(|n| format!(r#"{{"title":["Paper {n}"],"DOI":"10.1000/{n}"}}"#)).collect();
            Ok(format!(r#"{{"message":{{"total-results":{},"items":[{}]}}}}"#, total.get(), items.join(",")))
        };
        let online = |name: &str, args: Value| {
            requests.set(0);
            let result = super::call(&root, name, &args, &crossref, &no_download).unwrap();
            (result["fetched"].as_u64().unwrap_or(0), result["added"].as_u64().unwrap_or(0), requests.get(), result)
        };
        let id = online("save_search", json!({"source": "crossref", "query": "root", "maxResults": 1000})).3["id"].clone();
        // The first run takes everything, three pages of it.
        let (fetched, added, asked, first) = online("run_search", json!({"id": id}));
        assert_eq!((fetched, added, asked), (450, 450, 3));
        // The next one finds nothing new on the first page and leaves it at that.
        let (fetched, added, asked, _) = online("run_search", json!({"id": id}));
        assert_eq!((fetched, added, asked), (200, 0, 1));
        // A new record further down shows in the count, and is looked for.
        total.set(451);
        let (fetched, added, asked, _) = online("run_search", json!({"id": id}));
        assert_eq!((fetched, added, asked), (451, 1, 3));
        assert_eq!(online("run_search", json!({"id": id})).2, 1);
        // Asked for more than any run went through, or after an import was taken back: everything again.
        online("save_search", json!({"id": id, "maxResults": 1200}));
        assert_eq!(online("run_search", json!({"id": id})).2, 3);
        assert_eq!(online("run_search", json!({"id": id})).2, 1);
        online("undo_import", json!({"batchId": first["batchId"]}));
        let (fetched, added, asked, _) = online("run_search", json!({"id": id}));
        assert_eq!((fetched, added, asked), (451, 450, 3));
        std::fs::remove_dir_all(root).ok();
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
    fn proposals_are_not_values() {
        let (root, id) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        store.save_field(&mut FieldDef { name: "方法".into(), kind: crate::model::FieldKind::Select, options: vec!["scRNA-seq".into(), "snRNA-seq".into()], hint: "测序方法".into(), ..Default::default() }).unwrap();
        drop(store);
        let status = call(&root, "library_status", &json!({})).unwrap();
        assert_eq!((status["fields"][1]["name"].as_str(), status["fields"][1]["hint"].as_str(), status["fields"][1]["options"][1].as_str()), (Some("方法"), Some("测序方法"), Some("snRNA-seq")));
        // An option in another spelling is taken as that option; the quotation is found on its page.
        let said = call(&root, "suggest_field", &json!({"id": id, "field": "方法", "value": "ScRNA seq", "excerpt": "Libraries were prepared with 10x Genomics Chromium", "author": "codex"})).unwrap();
        assert_eq!((said["values"][0].as_str(), said["excerptKept"].as_bool(), said["page"].as_u64(), said["current"].as_array().map(Vec::len)), (Some("scRNA-seq"), Some(true), Some(2), Some(0)));
        // The field is as it was; the proposal waits beside it.
        let paper = call(&root, "get_paper", &json!({"id": id})).unwrap();
        assert!(paper["fields"]["方法"].is_null());
        assert_eq!((paper["fieldProposals"][0]["field"].as_str(), paper["fieldProposals"][0]["values"][0].as_str(), paper["fieldProposals"][0]["author"].as_str()), (Some("方法"), Some("scRNA-seq"), Some("codex")));
        // What the field holds already does not wait; an invented quotation is dropped; nothing found is kept as that.
        let held = call(&root, "suggest_field", &json!({"id": id, "field": "物种", "value": "Arabidopsis", "excerpt": "This paper is about leaves only."})).unwrap();
        assert_eq!((held["excerptKept"].as_bool(), held["current"][0].as_str()), (Some(false), Some("Arabidopsis")));
        assert_eq!(call(&root, "get_paper", &json!({"id": id})).unwrap()["fieldProposals"].as_array().map(Vec::len), Some(1));
        assert_eq!(call(&root, "suggest_field", &json!({"id": id, "field": "方法"})).unwrap()["values"], json!([]));
        assert_eq!(call(&root, "get_paper", &json!({"id": id})).unwrap()["fieldProposals"], json!([]));
        for (args, message) in [(json!({"id": id, "field": "方法", "value": "Smart-seq2"}), "单选词条"), (json!({"id": id, "field": "产地", "value": "x"}), "没有名为「产地」的词条"), (json!({"id": id, "value": "x"}), "需要参数 field"), (json!({"id": "P-none", "field": "物种", "value": "x"}), "文献不存在")] {
            let error = call(&root, "suggest_field", &args).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let schema = &catalog().into_iter().find(|t| t.name == "suggest_field").unwrap().input_schema;
        assert_eq!(cli_args(schema, &["P-1".into(), "物种".into(), "--value".into(), "拟南芥；水稻".into()]).unwrap(), json!({"id": "P-1", "field": "物种", "value": "拟南芥；水稻"}));
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
        assert!(inside.iter().all(|t| t.name != "create_library" && t.name != "import_library"));
        assert_eq!(inside.len(), catalog().len() - 4);
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
    fn papers_are_organised_and_manuscripts_read() {
        let (root, atlas) = sample();
        let done = call(&root, "organise_papers", &json!({"changes": [{"id": atlas, "addTags": ["综述"], "starred": true, "note": "再读一遍方法"}]})).unwrap();
        assert_eq!((done["changed"].as_u64(), done["papers"][0]["changed"].as_array().map(Vec::len)), (Some(1), Some(3)));
        let paper = call(&root, "get_paper", &json!({"id": atlas})).unwrap();
        assert!(paper.to_string().contains("综述") && paper.to_string().contains("助手所记"));
        // The decision stays the person's, whatever the call says.
        assert!(call(&root, "organise_papers", &json!({"changes": [{"id": atlas, "screening": "included"}]})).unwrap_err().contains("suggest_screening"));
        assert_eq!(describe_call("organise_papers", &json!({"changes": [{"id": "a"}, {"id": "b"}]})), "整理文献：2 篇");
        assert_eq!(describe_result(&Ok(done)), "改了 1 篇");
        // The command line writes the list as JSON.
        let tool = catalog().into_iter().find(|tool| tool.name == "organise_papers").unwrap();
        let args = cli_args(&tool.input_schema, &["[{\"id\":\"a\",\"starred\":true}]".to_string()]).unwrap();
        assert_eq!(args, json!({"changes": [{"id": "a", "starred": true}]}));

        assert!(call(&root, "read_manuscript", &json!({})).unwrap_err().contains("还没有稿件"));
        let info = libraries::list(&root).remove(0);
        let mut store = Store::open(&info.dir).unwrap();
        let made = store.create_manuscript("根的综述", "zh").unwrap();
        store.save_section_body(&made.sections[1].id, "根尖的干细胞 [@Zhang2021]。").unwrap();
        drop(store);
        let listed = call(&root, "list_manuscripts", &json!({})).unwrap();
        assert_eq!((listed["manuscripts"][0]["title"].as_str(), listed["manuscripts"][0]["sections"].as_array().map(Vec::len)), (Some("根的综述"), Some(5)));
        // Without a section, only the parts that are written.
        let read = call(&root, "read_manuscript", &json!({})).unwrap();
        assert_eq!((read["sections"].as_array().map(Vec::len), read["sections"][0]["body"].as_str()), (Some(1), Some("根尖的干细胞 [@Zhang2021]。")));
        let name = made.sections[0].name.clone();
        assert_eq!(call(&root, "read_manuscript", &json!({"manuscript": "根的综述", "section": name})).unwrap()["sections"][0]["body"], "");
        assert!(call(&root, "read_manuscript", &json!({"section": "没有这一节"})).unwrap_err().contains("没有名为"));
        // An agent writes a section and sets the outline; how each reads in a conversation.
        let wrote = call(&root, "edit_section", &json!({"section": name, "text": format!("根尖的干细胞 [@{atlas}]。")})).unwrap();
        assert_eq!(describe_result(&Ok(wrote)), format!("已写入「{name}」"));
        assert!(call(&root, "read_manuscript", &json!({"section": name})).unwrap()["sections"][0]["body"].as_str().unwrap().starts_with("根尖的干细胞 [@"));
        assert_eq!(describe_call("edit_section", &json!({"section": "导言", "text": "一二三"})), "写稿件：导言 · 全文 3 字");
        assert_eq!(describe_call("edit_section", &json!({"section": "导言", "edits": [{"old": "a", "new": "b"}]})), "写稿件：导言 · 1 处");
        let outlined = call(&root, "set_outline", &json!({"sections": [{"name": name}, {"name": "展望"}]})).unwrap();
        assert_eq!((describe_result(&Ok(outlined.clone())), outlined["sections"].as_array().map(Vec::len)), ("大纲现有 3 节".to_string(), Some(3)));
        assert_eq!(outlined["kept"].as_array().map(Vec::len), Some(1));
        assert_eq!(preview_call(&Store::open(&info.dir).unwrap(), "set_outline", &json!({"sections": [{"name": "甲"}]})).unwrap(), vec!["《根的综述》的大纲改为：甲"]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_library_moves_through_its_file() {
        let (root, _) = sample();
        let path = root.join("根.wispstudy");
        assert!(call(&root, "export_library", &json!({"path": root.join("根.zip")})).unwrap_err().contains(".wispstudy"));
        let exported = call(&root, "export_library", &json!({"path": path, "attachments": false})).unwrap();
        assert_eq!((exported["papers"].as_u64(), exported["attachments"].as_bool()), (Some(2), Some(false)));
        assert!(call(&root, "export_library", &json!({"path": path})).unwrap_err().contains("不会覆盖"));
        let imported = call(&root, "import_library", &json!({"path": path})).unwrap();
        assert_eq!((imported["name"].as_str(), imported["papers"].as_u64()), (Some("根 (2)"), Some(2)));
        let (here, there) = (call(&root, "list_papers", &json!({"library": "根"})).unwrap(), call(&root, "list_papers", &json!({"library": imported["id"]})).unwrap());
        assert_eq!((here["total"].as_u64(), here["papers"][0]["title"].as_str()), (there["total"].as_u64(), there["papers"][0]["title"].as_str()));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slides_are_written_to_a_new_file_with_the_figures_of_the_library() {
        let (root, atlas) = sample();
        let info = find_library(&root, "").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x02\x80\x00\x00\x01\xe0\x08\x02\x00\x00\x00";
        let mut niche = crate::figure::Figure { paper_id: atlas.clone(), page: 2, label: "Fig. 1".into(), legend: "The quiescent centre under the confocal microscope.".into(), ..Default::default() };
        store.add_figure(&mut niche, png).unwrap();
        let mut drawn = crate::figure::Figure { legend: "A drawing".into(), ..Default::default() };
        store.add_figure(&mut drawn, b"RIFF\x00\x00\x00\x00WEBPVP8 ").unwrap();
        let cited = store.get_paper(&atlas).unwrap().citation();
        drop(store);
        let output = root.join("talk.pptx");
        let slides = json!([
            {"title": "背景"},
            {"title": "静止中心", "bullets": ["- WOX5 只在静止中心表达", "  - 敲除后干细胞分化"], "figure": niche.id, "notes": "先讲这张图。"},
            {"title": "模型画的图", "bullets": "一条\n两条", "figure": drawn.id, "source": "自绘"},
        ]);
        let done = call(&root, "make_slides", &json!({"paperId": atlas, "slides": slides, "output": output})).unwrap();
        // The title page and three slides; the WebP is left out and said, the slide is still made.
        assert_eq!((done["slides"].as_u64(), done["pictures"].as_u64(), done["title"].as_str(), done["output"].as_str()), (Some(4), Some(1), Some("Root stem cell niche"), output.to_str()));
        assert!(done["notes"][0].as_str().unwrap().contains("第 3 页没有放图") && done["notes"][0].as_str().unwrap().contains("WebP"));
        let zip = crate::docx::Zip::read(&std::fs::read(&output).unwrap()).unwrap();
        // What the paper and the figure library know is on the slides without being asked for.
        assert!(cited.ends_with("et al., 2021") && zip.text("ppt/slides/slide1.xml").unwrap().contains(&format!("<a:t>{cited}</a:t>")));
        let (opening, shown) = (zip.text("ppt/slides/slide2.xml").unwrap(), zip.text("ppt/slides/slide3.xml").unwrap());
        assert!(opening.contains(&format!("<a:t>{cited}</a:t>")) && shown.contains(&format!("<a:t>{cited} · Fig. 1 · p. 2</a:t>")) && shown.contains("<a:t>Fig. 1 | The quiescent centre under the confocal microscope.</a:t>"));
        assert!(shown.contains("lvl=\"1\"") && shown.contains("<a:t>敲除后干细胞分化</a:t>") && zip.has("ppt/media/image1.png") && zip.text("ppt/notesSlides/notesSlide3.xml").unwrap().contains("先讲这张图。"));
        let last = zip.text("ppt/slides/slide4.xml").unwrap();
        assert!(last.contains("<a:t>一条</a:t>") && last.contains("<a:t>两条</a:t>") && last.contains("<a:t>自绘</a:t>") && !last.contains("<p:pic>"));
        // No file is written over, and a call that cannot be carried out says which slide is at fault.
        assert!(call(&root, "make_slides", &json!({"title": "T", "slides": slides, "output": output})).unwrap_err().contains("不会覆盖"));
        assert!(call(&root, "make_slides", &json!({"title": "T", "slides": slides, "output": root.join("talk.ppt")})).unwrap_err().contains(".pptx"));
        let wrong = |slides: Value| call(&root, "make_slides", &json!({"title": "T", "slides": slides, "output": root.join("other.pptx")})).unwrap_err();
        assert!(wrong(json!([{"title": "a"}, {"figure": "G-none"}])).contains("第 2 页：图片库里没有 G-none"));
        assert!(wrong(json!([{"notes": "只有备注"}])).contains("第 1 页是空的") && wrong(json!([])).contains("需要 slides") && wrong(json!([{"title": "a", "bullets": [1]}])).contains("bullets"));
        assert!(wrong(json!([{"figure": niche.id, "image": "/tmp/a.png"}])).contains("只给一个") && !root.join("other.pptx").exists());
        assert!(call(&root, "make_slides", &json!({"slides": [{"title": "a"}], "output": root.join("other.pptx")})).unwrap_err().contains("需要 title"));
        // A picture file of the machine goes on a slide too; the command line reads the slides as JSON.
        let picture = root.join("plot.png");
        std::fs::write(&picture, png).unwrap();
        let tool = catalog().into_iter().find(|tool| tool.name == "make_slides").unwrap();
        let words = [format!("[{{\"title\": \"Plot\", \"image\": {:?}}}]", picture.to_str().unwrap()), "--title".into(), "Lab meeting".into(), "--output".into(), root.join("plot.pptx").display().to_string()];
        let made = call(&root, "make_slides", &cli_args(&tool.input_schema, &words).unwrap()).unwrap();
        assert_eq!((made["slides"].as_u64(), made["pictures"].as_u64(), made["notes"].as_array().map(Vec::len)), (Some(2), Some(1), Some(0)));
        assert!(tool.writes && tool.confirm);
        std::fs::remove_dir_all(root).ok();
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
        assert_eq!(library_catalog().iter().filter(|tool| tool.confirm).map(|tool| tool.name).collect::<Vec<_>>().len(), 19);
        // The tools that write a manuscript are asked about outside the conversation that writes it.
        assert!(MANUSCRIPT.iter().all(|name| catalog().iter().any(|tool| tool.name == *name && tool.writes && tool.confirm)));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn journal_metrics_by_name_issn_or_paper() {
        let (root, _) = sample();
        let id = Store::open(&crate::libraries::list(&root)[0].dir).unwrap().add_paper(Paper { title: "Hairs".into(), journal: "Nat Commun".into(), ..Default::default() }).unwrap().id;
        let by_paper = call(&root, "journal_metrics", &json!({"paperId": id})).unwrap();
        assert_eq!((by_paper["found"].as_bool(), by_paper["journal"]["quartile"].as_str()), (Some(true), Some("Q1")));
        assert_eq!(call(&root, "journal_metrics", &json!({"journal": "2041-1723"})).unwrap()["journal"], by_paper["journal"]);
        assert_eq!(call(&root, "get_paper", &json!({"id": id})).unwrap()["journalMetrics"], by_paper["journal"]);
        assert_eq!(call(&root, "journal_metrics", &json!({"journal": "Journal of Nothing At All Whatsoever"})).unwrap()["found"], false);
        assert!(call(&root, "journal_metrics", &json!({})).unwrap_err().contains("需要参数 journal"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_word_file_written_elsewhere_is_read_and_cited_anew_in_a_new_file() {
        let (root, _) = sample();
        Store::open(&crate::libraries::list(&root)[0].dir).unwrap().add_paper(Paper { title: "Root hairs take up water".into(), doi: "10.1000/hairs".into(), authors: "Zhang, Wei; Li, Na; Wang, Fang".into(), year: Some(2021), journal: "Plant Cell".into(), ..Default::default() }).unwrap();
        let p = |text: &str| format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>");
        let source = crate::docx::tests::file(&format!("{}{}{}{}", p("Stem cells stay put [1], whatever else is said [2]."), p("References"), p("[1] Zhang W, Li N, Wang F. Root hairs take up water. Plant Cell. 2021;33(2):10-19. doi:10.1000/hairs"), p("[2] A note nobody can read.")), "");
        let path = root.join("draft.docx");
        std::fs::write(&path, &source).unwrap();
        let read = call(&root, "read_docx_citations", &json!({"path": path})).unwrap();
        assert_eq!((read["source"].as_str(), read["entries"][0]["resolved"].as_str(), read["entries"][0]["sure"].as_str(), read["unresolved"].clone()), (Some("text"), Some("library"), Some("sure"), json!([2])));
        assert_eq!(read["citations"]["keptAsWritten"][0]["why"], "第 2 条参考文献没能识别");
        // A new file beside the old one; neither it nor the file given is ever written over.
        let done = call(&root, "restyle_docx", &json!({"path": path, "style": "csl:apa"})).unwrap();
        let output = PathBuf::from(done["output"].as_str().unwrap());
        assert_eq!((output.file_name().and_then(|name| name.to_str()), done["citations"]["rewritten"].as_u64(), done["references"]["leftAsWritten"].as_u64()), (Some("draft-restyled.docx"), Some(1), Some(1)));
        let said: Vec<String> = crate::docx::read(&std::fs::read(&output).unwrap()).unwrap().paragraphs.into_iter().map(|para| para.text).collect();
        assert_eq!(said[0], "Stem cells stay put (Zhang et al., 2021), whatever else is said [2].");
        assert!(said[2].starts_with("Zhang, W., Li, N., & Wang, F. (2021). Root hairs take up water. Plant Cell, 33(2), 10–19."), "{}", said[2]);
        assert_eq!(std::fs::read(&path).unwrap(), source);
        assert!(call(&root, "restyle_docx", &json!({"path": path, "style": "csl:apa"})).unwrap_err().contains("不会覆盖"));
        assert!(call(&root, "restyle_docx", &json!({"path": path, "style": "csl:apa", "output": path})).unwrap_err().contains("不会覆盖"));
        // A style changed for this file alone, the entry the rules could not read given by whoever asks, the changes tracked.
        let mine = call(&root, "read_cite_style", &json!({"style": "csl:nature"})).unwrap()["csl"].as_str().unwrap().replace("vertical-align=\"sup\"", "prefix=\"[\" suffix=\"]\"");
        let done = call(&root, "restyle_docx", &json!({"path": path, "csl": mine, "output": root.join("out.docx"), "track": true, "entries": [{"n": 2, "csl": {"type": "article-journal", "title": "A note", "author": [{"family": "Nobody"}], "issued": {"date-parts": [[1999]]}}}]})).unwrap();
        assert_eq!((done["citations"]["rewritten"].as_u64(), done["unresolved"].clone(), done["tracked"].as_bool()), (Some(2), json!([]), Some(true)));
        assert_eq!(done["citations"]["sample"][1], json!({"before": "[2]", "after": "[2]"}));
        assert!(call(&root, "restyle_docx", &json!({"path": path, "style": "numeric"})).unwrap_err().contains("csl:nature"));
        assert!(call(&root, "read_docx_citations", &json!({"path": root.join("x.pdf")})).is_err());
        assert!(catalog().iter().any(|tool| tool.name == "restyle_docx" && tool.writes && tool.confirm) && catalog().iter().any(|tool| tool.name == "read_docx_citations" && !tool.writes));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_file_written_elsewhere_becomes_a_manuscript_of_the_writing_page() {
        let (root, _) = sample();
        let dir = crate::libraries::list(&root)[0].dir.clone();
        // The empty manuscript 写作 opens a library with.
        let blank = Store::open(&dir).unwrap().create_manuscript("", crate::manuscript::DEFAULT_LANGUAGE).unwrap();
        let p = |text: &str| format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>");
        let h = |text: &str| format!("<w:p><w:pPr><w:pStyle w:val=\"1\"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>");
        let source = crate::docx::tests::file(&format!("{}{}{}{}{}{}", h("Introduction"), p("Roots grow down [1]."), h("Results"), p("Stem cells stay put."), h("References"), p("[1] Zhang W. Root hairs take up water. Plant Cell. 2021.")), "");
        let path = root.join("Roots v3.docx");
        std::fs::write(&path, &source).unwrap();
        let card = preview_call(&Store::open(&dir).unwrap(), "import_manuscript", &json!({"path": path})).unwrap();
        assert!(card[0].contains("Roots v3.docx") && card[1].starts_with("3 节：Introduction、Results、References"), "{card:?}");
        let done = call(&root, "import_manuscript", &json!({"path": path})).unwrap();
        assert_eq!((done["file"].as_str(), done["manuscript"]["sections"].as_array().map(Vec::len), done["retiredBlank"].as_u64(), done["guessedHeadings"].clone()), (Some("Roots v3.docx"), Some(3), Some(1), json!([])));
        assert_eq!(describe_result(&Ok(done.clone())), "已导入「Roots v3.docx」：3 节");
        // It is a manuscript like any other from then on, the blank one gave way, and the file is as it was.
        let listed = call(&root, "list_manuscripts", &json!({})).unwrap();
        assert_eq!((listed["manuscripts"].as_array().map(Vec::len), listed["manuscripts"][0]["id"].clone()), (Some(1), done["manuscript"]["id"].clone()));
        assert!(listed["manuscripts"].as_array().unwrap().iter().all(|m| m["id"] != json!(blank.id)));
        assert_eq!(call(&root, "read_manuscript", &json!({"section": "Introduction"})).unwrap()["sections"][0]["body"], "Roots grow down [1].");
        assert_eq!(std::fs::read(&path).unwrap(), source);
        // The same file is not taken in twice, under whatever name.
        let copy = root.join("copy.docx");
        std::fs::write(&copy, &source).unwrap();
        let again = call(&root, "import_manuscript", &json!({"path": copy})).unwrap();
        assert_eq!((again["already"].as_bool(), again["manuscript"]["id"].clone()), (Some(true), done["manuscript"]["id"].clone()));
        assert!(preview_call(&Store::open(&dir).unwrap(), "import_manuscript", &json!({"path": copy})).unwrap()[0].contains("已经是写作页的稿件"));
        assert_eq!(call(&root, "list_manuscripts", &json!({})).unwrap()["manuscripts"].as_array().map(Vec::len), Some(1));
        // Markdown and plain text too; a file without heading styles is cut at what looks like headings, and said so.
        let notes = root.join("notes.md");
        std::fs::write(&notes, "# 根的综述\n\n## 引言\n\n根向下生长，吸收水分。\n\n## 讨论\n\n还有很多不清楚的地方。\n").unwrap();
        let taken = call(&root, "import_manuscript", &json!({"path": notes})).unwrap();
        assert_eq!((taken["manuscript"]["title"].as_str(), taken["manuscript"]["sections"].as_array().map(Vec::len)), (Some("根的综述"), Some(2)));
        assert_eq!(call(&root, "import_manuscript", &json!({"path": notes})).unwrap()["already"], true);
        let plain = crate::docx::tests::file(&format!("{}{}{}{}{}", p("根尖干细胞的研究进展"), p("一、引言"), p("根向下生长，吸收水分，这是一段足够长的正文，用来和标题区分开。"), p("二、结果"), p("干细胞留在原处，这也是一段足够长的正文，用来和标题区分开。")), "");
        let unstyled = root.join("无样式.docx");
        std::fs::write(&unstyled, &plain).unwrap();
        let guessed = call(&root, "import_manuscript", &json!({"path": unstyled})).unwrap();
        assert_eq!(guessed["guessedHeadings"], json!(["一、引言", "二、结果"]));
        assert!(guessed["notes"].as_array().unwrap().iter().any(|note| note.as_str().unwrap().contains("没有用标题样式")));
        // What is no manuscript file is said, an old .doc with what to do about it.
        std::fs::write(root.join("old.doc"), b"x").unwrap();
        assert!(call(&root, "import_manuscript", &json!({"path": root.join("old.doc")})).unwrap_err().contains("另存为 .docx"));
        std::fs::write(root.join("x.pdf"), b"x").unwrap();
        assert!(call(&root, "import_manuscript", &json!({"path": root.join("x.pdf")})).unwrap_err().contains(".docx"));
        assert!(catalog().iter().any(|tool| tool.name == "import_manuscript" && tool.writes && tool.confirm) && !MANUSCRIPT.contains(&"import_manuscript"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_references_of_a_word_file_become_the_librarys_papers() {
        let (root, _) = sample();
        let dir = crate::libraries::list(&root)[0].dir.clone();
        Store::open(&dir).unwrap().add_paper(Paper { title: "Root hairs take up water".into(), doi: "10.1000/hairs".into(), year: Some(2021), ..Default::default() }).unwrap();
        let p = |text: &str| format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>");
        let source = crate::docx::tests::file(&format!("{}{}{}{}{}", p("Roots [1], branches [2] and more [3]."), p("References"), p("[1] Zhang W. Root hairs take up water. Plant Cell. 2021;33(2):10-19. doi:10.1000/hairs"), p("[2] Wang F. Lateral roots branch out. Nature, 2019, 570: 1-5."), p("[3] A note nobody can read.")), "");
        let path = root.join("thesis.docx");
        std::fs::write(&path, &source).unwrap();
        // Without the network: the one the library has is skipped, the one read by rule comes in to be checked, the one unread is said.
        let done = call(&root, "import_docx_references", &json!({"path": path, "lookup": false})).unwrap();
        assert_eq!((done["entries"].as_u64(), done["added"].as_u64(), done["skipped"].as_u64()), (Some(3), Some(1), Some(1)));
        assert_eq!((done["checkAgainst"][0]["n"].as_u64(), done["unresolved"][0]["n"].as_u64(), done["unresolved"][0]["text"].as_str()), (Some(2), Some(3), Some("[3] A note nobody can read.")));
        let papers = Store::open(&dir).unwrap().list_papers().unwrap();
        let taken = papers.iter().find(|paper| paper.title == "Lateral roots branch out").unwrap();
        assert_eq!((taken.authors.as_str(), taken.year, taken.journal.as_str(), taken.source.as_str()), ("Wang, F", Some(2019), "Nature", "导入 · thesis.docx"));
        // An entry read by the caller is taken alone; again, nothing comes in twice.
        let given = call(&root, "import_docx_references", &json!({"path": path, "entries": [{"n": 3, "csl": {"type": "article-journal", "title": "Notes on roots", "author": [{"family": "Nobody"}], "issued": {"date-parts": [[2020]]}}}]})).unwrap();
        assert_eq!((given["added"].as_u64(), given["unresolved"].as_array().map(Vec::len)), (Some(1), Some(0)));
        assert_eq!(call(&root, "import_docx_references", &json!({"path": path, "lookup": false})).unwrap()["added"].as_u64(), Some(0));
        assert_eq!(Store::open(&dir).unwrap().list_papers().unwrap().len(), papers.len() + 1);
        assert!(call(&root, "import_docx_references", &json!({"path": root.join("x.pdf")})).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_cite_style_is_read_tried_and_saved_for_the_person_to_choose() {
        let (root, _) = sample();
        let listed = call(&root, "list_cite_styles", &json!({})).unwrap();
        assert!(listed["common"].as_array().unwrap().iter().any(|style| style["key"] == "csl:nature") && listed["added"].as_array().unwrap().iter().all(|style| !style["key"].as_str().unwrap().starts_with("file:")));
        // A built-in style is written out as the XML a changed one starts from.
        let xml = call(&root, "read_cite_style", &json!({"style": "csl:nature"})).unwrap()["csl"].as_str().unwrap().to_string();
        assert!(xml.contains("<citation") && xml.contains("vertical-align=\"sup\""), "{}", &xml[..xml.len().min(600)]);
        let tried = call(&root, "try_cite_style", &json!({"style": "csl:nature"})).unwrap();
        assert_eq!((tried["citations"][0]["text"].as_str(), tried["citations"][0]["superscript"][0].as_str()), (Some("1"), Some("1")));
        assert!(tried["references"][0]["text"].as_str().unwrap().starts_with("1. "), "{tried}");
        // Changed: brackets on the line instead of a raised number. Tried without being kept, then saved.
        let mine = xml.replace("vertical-align=\"sup\"", "prefix=\"[\" suffix=\"]\"").replace("<title>Nature</title>", "<title>Nature, in brackets</title>");
        assert_ne!(mine, xml);
        let tried = call(&root, "try_cite_style", &json!({"csl": mine})).unwrap();
        assert_eq!((tried["citations"][0]["text"].as_str(), tried["citations"][0].get("superscript")), (Some("[1]"), None), "{tried}");
        assert!(crate::csl::added(&root).iter().all(|style| !style.key.starts_with("file:")));
        assert!(call(&root, "try_cite_style", &json!({"csl": "<style>"})).unwrap_err().contains("CSL"));
        let saved = call(&root, "save_cite_style", &json!({"name": "nature-brackets", "csl": mine})).unwrap();
        assert_eq!((saved["style"].as_str(), saved["name"].as_str(), saved["citations"][0]["text"].as_str()), (Some("file:nature-brackets.csl"), Some("Nature, in brackets"), Some("[1]")));
        assert!(crate::csl::added(&root).iter().any(|style| style.key == "file:nature-brackets.csl"));
        // Saved again saying something else: what it said before is kept.
        call(&root, "save_cite_style", &json!({"name": "nature-brackets.csl", "csl": mine.replace("in brackets", "bracketed")})).unwrap();
        assert_eq!(std::fs::read_dir(root.join("styles/removed")).unwrap().count(), 1);
        assert!(call(&root, "save_cite_style", &json!({"name": "../x", "csl": mine})).unwrap_err().contains("文件名"));
        assert!(catalog().iter().any(|tool| tool.name == "save_cite_style" && tool.writes && !tool.confirm));
        std::fs::remove_dir_all(root).ok();
    }
}
