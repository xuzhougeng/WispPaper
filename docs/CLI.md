# 命令行与 MCP

`wisp` 是不开窗口的入口，读写和应用同一个数据目录，应用开着时也能用。它提供的每个工具既是一个子命令，也是一个 MCP 工具，应用内的助手用的也是同一张工具表。

供外部智能体使用的完整操作说明见 [SKILL.md](../SKILL.md)，包括能力边界、PowerShell 示例、导入结果核对和 MCP 配置。

## 建库与导入

当前 CLI / MCP 可以向**已有文献库**导入在线检索结果，流程是 `search_online` 试检索、`save_search` 保存检索式、`run_search` 执行导入；PDF 可再用 `find_pdf` 获取开放版本。

也可以完全通过 CLI 从零建库并导入本地资料：

```sh
wisp create_library --name "植物单细胞" --topic "细胞图谱与发育轨迹" --color green
# 用上一步返回的真实 id 替换 L-实际ID
wisp import_references -l L-实际ID --path references.bib
wisp import_references -l L-实际ID --path references.ris
wisp import_pdf -l L-实际ID --path paper.pdf
wisp import_pdf -l L-实际ID --path "PDF collection"
wisp import_zotero -l L-实际ID --path /path/to/Zotero/zotero.sqlite
```

`import_pdf` 支持递归导入文件夹，子文件夹名称转为标签；`--paper-id` 可将单个 PDF 附到已有文献。离线提取 DOI 和全文，未补全的题目用文件名，不自动联网查询元数据。已有记录保留，回收站条目跳过，相同条目的相同附件内容只保存一次。

`import_references` 按内容识别 BibTeX / RIS，不自动携带 PDF。`import_zotero` 复用桌面的只读迁移流程，复制可访问的本地 PDF；建议先退出 Zotero，详见 [Zotero 导入](ZOTERO.md)。这些本地导入不联网、不移动源文件。

PDF / Zotero 的 JSON 含新增、跳过、附件数量和逐项 `results`。必须检查 `failed` 和 `results[].problems`：有些记录已入库，但附件可能缺失，部分失败仍可能退出 0。PDF 的 `textless` / `textError` 表示文件已保存但未提取出文字。Zotero 附件的文本在阅读时提取。本地导入不生成在线检索的批次号，不能用 `undo_import` 撤销。

`--data-dir` 只选择数据根目录，建库用 `create_library`；`save_search --name` 命名的是检索式。直接按 DOI / PMID 添加条目仍使用桌面入口。旧发布包缺少新命令时需更新或从源码构建。

## 命令行

```sh
cargo build --release -p wisp_cli       # 生成 target/release/wisp

wisp help                               # 命令一览
wisp list_libraries
wisp list_papers -l 大语言模型 --query attention --limit 10
wisp get_paper <文献 ID>
wisp search_fulltext "10x Genomics Chromium"
wisp read_fulltext <文献 ID> --page 3
wisp list_pdf_annotations --paper-id <文献 ID> --query "生长"
wisp search_fulltext --help             # 一个工具的参数
```

输出是 JSON；出错时信息写到 stderr，退出码为 1。只有一个文献库时可以省略 `-l`；`--data-dir 文件夹` 指定另一个数据目录。

## 工具

| 工具 | 用途 |
|---|---|
| `list_libraries` | 所有文献库的 ID、名称、主题和文献数量 |
| `list_pdf_annotations` | 只读查询 PDF 下划线、高亮及笔记；可按 `--paper-id`、`--resource-id`、`--page`、`--kind`、`--query` 筛选，用 `--limit`、`--offset` 分页。返回 `quote`（原文）、`note`（用户笔记）、1-based 页码、页面左上角起的点坐标 `boxes`、附件 ID 和时间；`get_paper` 的 `pdfAnnotationCount` 给出总数。内置助手与只读 MCP 也可使用 |
| `create_library` | 新建文献库；`--name` 必填，`--topic`、`--color` 可选，返回新库 ID（会创建文件和数据库） |
| `import_pdf` | 复制本地 PDF 或递归导入文件夹，`--path` 必填；单文件可用 `--paper-id` 附到已有条目（会改动文献库） |
| `import_references` | 导入本地 BibTeX / RIS 文件，`--path` 必填，按内容识别格式（会改动文献库） |
| `import_zotero` | 从本机 `zotero.sqlite` 迁移文献、笔记、标签和 PDF，`--path` 必填（会改动目标文献库） |
| `library_status` | 一个库的概况：各状态的数量、自定义词条、状态值的显示名称 |
| `list_papers` | 搜索、筛选、翻页；规则与界面上的列表相同 |
| `get_paper` | 一篇文献的完整记录、词条和附件 |
| `list_figures` | 图片库：从 PDF 里截下来收藏的图片，各自的图号、图注、标签、备注、出处（文献和页码）和图片文件的路径；可按 `--query`、`--tag`、`--paper` 筛选 |
| `search_fulltext` | 在已保存的全文里找短语或关键词，返回页码和逐字片段 |
| `read_fulltext` | 按页读取全文，一次最多 8 页 |
| `find_citations` | 为一段文字逐句找出库里可能支持它的文献和它自己最贴近的一句原文（按词语找出的候选；文字和文献语言不同时用 `queries` 给各句配检索词） |
| `find_passages` | 在一篇文献里找出最贴近给定句子的原文：稿件里引用它的那句话依据的是哪一句（摘要和全文里的句子，全文里的带页码；语言不同时用 `queries` 给出检索词） |
| `find_sentences` | 写作参考：库里各篇文献在同一部分（摘要、导言、方法、结果、讨论）的句子；给 `--query` 时按相近程度排，不给时返回各篇这一部分的开头 |
| `search_online` | 在 PubMed、Europe PMC、Crossref 或 arXiv 试运行一个检索式，不写入；标出每条结果是否已在库中 |
| `find_metadata` | 为缺 DOI 或题目的文献按题目到 Crossref、Europe PMC 和 arXiv 查找可能对应的记录，不写入；返回各来源的情况和候选（相似度、一致与不一致之处、是否已是库里的另一篇）。`--paper-id` 必填，`--title`、`--author`、`--year` 可选。写入由人在窗口里核对后确认 |
| `list_searches` / `save_search` / `remove_search` | 查看、保存、删除检索式（后两个会改动文献库） |
| `run_search` | 运行一个保存的检索式并导入，已有的文献跳过，返回批次号（会改动文献库） |
| `list_imports` / `undo_import` | 导入记录；整批撤销一次导入，处理过的文献保留（撤销会改动文献库） |
| `sync_searches` | 运行到期的检索式（所有开启了自动更新的文献库，或指定的一个）；`--force` 不看间隔（会改动文献库） |
| `suggest_screening` | 为一篇文献提出收录建议（结论、理由、逐字引文、把握）；不改变收录判断，由用户在筛选页确认 |
| `suggest_field` | 为一篇文献的一个自定义词条提出取值（附逐字引文）；不改动词条，由用户在提取页确认。单选词条只接受它的选项 |
| `organise_papers` | 整理已有的文献：`changes` 是一个 JSON 数组，每项给出文献 `id` 和要改的方面（`addTags`、`removeTags`、`starred`、`reading`、`note`）；全部写入或全部不写。不能改收录判断和自定义词条 |
| `list_manuscripts` | 列出写作页的稿件：题目、语言、各节的名称和字数 |
| `read_manuscript` | 读取一份稿件的正文，可以只读一节；只读 |
| `find_pdf` | 为没有 PDF 的文献查找开放获取的全文（arXiv、OpenAlex 和 Europe PMC 列出的开放获取文件、部分出版商的开放获取文章），下载后附到原条目并读出文字；`--paper-id` 只处理一篇，`--limit` 一次最多几篇，`--look-only` 只列出候选地址不下载。不绕过付费墙或登录；会把 DOI 或 PMID 发给这些服务（会改动文献库） |

全文检索第一次用到某个 PDF 时会提取它的文字并缓存到库里，同时按标题把它分成导言、方法、结果、讨论并切成句子，之后即时返回。

代理用的是应用「设置 › 网络」里的同一个设置。

## MCP

把 `wisp mcp` 配置给 AI 客户端，它就能通过 MCP（标准输入输出）使用同一组工具；`wisp mcp --read-only` 不提供会改动文献库的工具：

```json
{
  "mcpServers": {
    "wisp-paper": { "command": "/path/to/wisp", "args": ["mcp"] }
  }
}
```

## 定时同步

不开窗口时，可以让系统的计划任务（cron、launchd、Windows 任务计划程序）定期执行 `wisp sync_searches`，它只运行已经到期的检索式。
