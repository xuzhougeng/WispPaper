---
name: wisp-paper
description: 使用 Wisp Paper 的 wisp CLI 或 MCP 新建文献库、导入本地 PDF / BibTeX / RIS / Zotero、在线检索并导入文献、获取开放全文、阅读和检索 PDF 文本、查找引用与提交筛选建议。用户提到 Wisp Paper、文献库、批量收集论文或基于本地论文回答问题时使用。
---

# Wisp Paper

Wisp Paper 是本地优先的文献管理软件。每个文献库是独立文件夹，包含书目信息、PDF、笔记和全文索引。这里的“建库”指新建文献库，不是创建 Git 仓库。

本说明对应当前源码的工具接口，旧发布包可能尚未包含新增的建库和本地导入命令。运行前用 `wisp tools` 和 `wisp <工具名> --help` 核对当前安装版本；缺少命令时使用包含该功能的版本或从源码构建。CLI 不打开窗口，与桌面应用使用同一份文献数据。

## 先判断任务是否支持

| 任务 | CLI / MCP 支持情况 | 操作入口 |
|---|---|---|
| 查看已有文献库、查询文献 | 支持 | `list_libraries`、`library_status`、`list_papers`、`get_paper` |
| 新建文献库 | 支持 | `create_library` |
| 从在线数据库批量导入书目信息和摘要 | 支持 | `search_online` → `save_search` → `run_search` |
| 按 DOI / PMID 直接添加条目 | 暂无专用命令 | 桌面「添加文献」 |
| 导入本地 PDF、PDF 文件夹 | 支持 | `import_pdf`；可用 `--paper-id` 将单文件附到已有条目 |
| 导入 BibTeX / RIS | 支持 | `import_references`，按内容自动识别格式 |
| 从 Zotero 本地数据库迁移 | 支持 | `import_zotero` |
| 为已有条目下载开放获取 PDF | 支持 | `find_pdf` |
| 全文检索、按页阅读、候选引用与句子 | 支持 | `search_fulltext`、`read_fulltext`、`find_citations`、`find_sentences` |
| 提出收录建议 | 支持；最终判断在桌面确认 | `suggest_screening` |
| 整理已有文献：标签、星标、阅读状态、追加笔记 | 支持 | `organise_papers` |
| 为自定义词条提出取值 | 支持；写入在桌面确认 | `suggest_field` |
| 查看稿件、为引用找出处 | 支持 | `list_manuscripts`、`read_manuscript`、`find_passages` |
| 修改题录字段，导出 BibTeX / CSV | 暂无对应命令 | 桌面编辑与导出 |

通过工具接口操作，不直接修改 SQLite 或拼装 `library.json`。BibTeX 和 RIS 共用 `import_references`，没有单独的 `import_bibtex`、`import_ris` 或通用 `import` 命令。

## 找到可执行文件并选择文献库

发布包附带独立的 `wisp`：Windows 是主程序旁的 `wisp.exe`，Linux 是主程序旁的 `wisp`，macOS 是 `Wisp Paper.app/Contents/MacOS/wisp`。它与 GUI 可执行文件不同。

以下示例使用 PowerShell。将 `$Wisp` 替换为安装位置；如果 `wisp` 已在 PATH 中，可以直接使用命令名。

```powershell
$Wisp = 'C:\Apps\WispPaper\wisp.exe'
& $Wisp help
& $Wisp tools
& $Wisp list_libraries
```

在源码仓库中也可以构建后使用：

```powershell
cargo build -p wisp_cli
$Wisp = (Resolve-Path '.\target\debug\wisp.exe').Path
& $Wisp list_libraries
```

macOS / Linux 的等价调用示例：

```sh
"/Applications/Wisp Paper.app/Contents/MacOS/wisp" list_libraries
# 或在源码目录中运行
cargo run -p wisp_cli -- list_libraries
```

从 `list_libraries` 的 `libraries` 数组中选择用户指定的库，后续优先使用其真实 ID。以下示例中的 ID、路径、检索式都需要替换为任务的实际值：

```powershell
$Library = 'L-实际文献库ID'
& $Wisp library_status -l $Library
```

- `-l` 等价于 `--library`，接受 ID 或完整名称；只有一个库时可以省略。同名库必须用 ID，不根据列表顺序猜测目标。
- 默认数据目录来自应用设置，未自定义时为系统「文档」目录下的 `WispPaper`。
- `--data-dir 'D:\Literature\WispPaper'` 指向包含多个文献库的**数据根目录**，不是某个 `L-…` 子目录；指定目录不会自动建库。使用自定义目录时，每次调用都带上该参数。
- `--data-dir` 只选择文献数据，仍读取当前设置。隔离测试可用 `WISP_PAPER_HOME` 将设置文件和默认数据目录放到另一处；其中已有 `settings.json` 的 `dataDir` 仍会生效。
- 工具成功时向 stdout 输出 JSON；`help` 输出文字。失败时错误写到 stderr，退出码为 1。PowerShell 调用后检查 `$LASTEXITCODE`，不要把错误当成空结果继续执行。
- CLI 参数使用 `--paper-id`、`--max-results` 等形式；MCP 参数使用 schema 中的 `paperId`、`maxResults` 等名称。
- 布尔参数开启用 `--force`、`--look-only`；关闭用 `--enabled=false`，不要写成 `--enabled false`。数组和对象参数写成一段 JSON（例如 `--queries '["root hair","auxin"]'`）；只有一项时可以直接写这一项。

## 从零开始或导入已有资料

用户需要新库时，直接创建并保存返回的真实 ID。每次 `create_library` 都会新建一个库；重跑任务前先查 `list_libraries`，不要无意创建同名副本。`save_search --name` 创建的是检索式，不是文献库。

```powershell
$CreatedJson = & $Wisp create_library --name '植物单细胞转录组' --topic '植物细胞图谱与发育轨迹' --color green
if ($LASTEXITCODE -ne 0) { throw '创建文献库失败，请查看 stderr。' }
$Created = $CreatedJson | ConvertFrom-Json
$Library = $Created.id

& $Wisp import_references -l $Library --path 'D:\Literature\references.bib'
& $Wisp import_references -l $Library --path 'D:\Literature\references.ris'
& $Wisp import_pdf -l $Library --path 'D:\Literature\paper.pdf'
& $Wisp import_pdf -l $Library --path 'D:\Literature\PDF collection'
& $Wisp import_zotero -l $Library --path 'C:\Users\YourName\Zotero\zotero.sqlite'
& $Wisp library_status -l $Library
```

这些是按来源选择执行的示例，每条命令执行后先检查退出码及 JSON 结果，再继续下一步。

- **建库**：`--name` 必填，最多 60 字；`--topic` 最多 2000 字；`--color` 默认为 `blue`，还可选 `green`、`amber`、`rose`、`violet`、`teal`、`slate`。返回 `id`、`name`、`topic`、`color`、`directory`、`papers`。
- **BibTeX / RIS**：按文件内容识别格式，只导入参考文献记录，不自动带入 PDF。返回 `total`、`added`、`skipped`、`addedPapers`、`skippedPapers`。重复记录保留原内容；`skippedPapers` 带 `existingId` 和 `inTrash`。
- **PDF / 文件夹**：离线读取前两页中的 DOI，题目暂用文件名；不自动联网补全元数据。按 DOI、PMID、题目去重；首个 PDF 是全文，后续不同内容的 PDF 是补充附件，同一条目的相同文件内容只存一次。文件夹递归扫描 PDF，各层子文件夹名称转为标签；隐藏项和链接目录跳过，读取失败会报告。
- **指定附件归属**：对缺 DOI 的 PDF 或扫描件，先确定文献 ID，再运行 `import_pdf -l $Library --path 'D:\Literature\paper.pdf' --paper-id $PaperId`。不能把 `--paper-id` 与文件夹一起使用。已有元数据和笔记保留，回收站中的目标跳过。
- **Zotero**：建议先退出 Zotero，再导入本机 `zotero.sqlite`；`storage` 目录应在数据库旁边。读取个人库和群组库，迁移支持的书目信息、标签、笔记及可访问的本地 PDF；只读源库、复制附件。相对链接附件和云端未下载 PDF 的限制见 [Zotero 教程](docs/ZOTERO.md)。全文在后续阅读或检索时提取。

PDF 和 Zotero 返回 `added`、`skipped`、`inTrash`、`filesAdded`、`filesExisting`、`failed`、`results`；Zotero 另有 `otherAttachments`（未迁移的非 PDF 附件数）。逐项检查 `results[].problems`，以及真实 `paperId`、`fileIds`。`skipped` 包括保留已有记录但仍可能补入 PDF 的条目；`failed` 是存在问题的项数，可能与 `added` 重叠，例如已添加记录但缺失附件。部分失败时仍返回 JSON、退出码可能为 0，不代表全部成功。

PDF 返回的 `textless` 和逐项 `textError` 表示文件已保存但未读出文字，不能当作导入失败，也不能宣称已经读过全文。没有 DOI 的同一论文若文件名不同，可能无法自动去重；优先明确 `--paper-id`。本地导入不会生成 `batchId`，不能用 `undo_import` 撤销；该命令只适用于在线检索批次。

导入后用 `library_status`、`list_papers` 和 `get_paper` 核对记录、附件和未完成项。上述本地导入不联网，不移动源文件。

## 在线检索并导入

支持的来源值为 `pubmed`、`europepmc`、`crossref`、`arxiv`。PubMed / Europe PMC 支持各自的布尔和字段语法，arXiv 使用 `ti:`、`abs:`、`cat:` 等字段，Crossref 按词语匹配。`list_searches` 返回的 `syntax` 可用于核对来源语法。

先用少量结果判断检索范围；`search_online` 不导入，也需要已有目标库：

```powershell
$Query = 'Arabidopsis AND single cell'
& $Wisp search_online -l $Library --source pubmed --query $Query --limit 10
& $Wisp list_searches -l $Library
```

用户已要求导入时，沿用已有匹配检索式，或保存新检索式并立即使用返回的 `id`：

```powershell
$SavedJson = & $Wisp save_search -l $Library --source pubmed --query $Query --name '拟南芥单细胞' --max-results 50
if ($LASTEXITCODE -ne 0) { throw '保存检索式失败，请查看 stderr。' }
$Saved = $SavedJson | ConvertFrom-Json
$SearchId = $Saved.id

$ImportedJson = & $Wisp run_search -l $Library --id $SearchId
if ($LASTEXITCODE -ne 0) { throw '导入失败，请查看 stderr 和 list_imports。' }
$Imported = $ImportedJson | ConvertFrom-Json
$Imported | Select-Object batchId, total, fetched, added, skipped

& $Wisp list_imports -l $Library --limit 10
& $Wisp library_status -l $Library
```

`save_search` 只保存，不执行检索。`search_online --limit` 最大 100；`save_search --max-results` 默认 50、最大 200，每次取回最新的有限条记录，不代表下载全部命中。`total` 是数据库命中总数，`fetched` 是取回数，`added` 是新增数，`skipped` 是跳过数。`addedPapers` 最多列出 50 篇，不能当作本批次完整清单。

导入按 DOI、PMID、题目去重，回收站内已有条目也会阻止重复导入；新文献的收录判断为 `pending`。这一步主要导入元数据和摘要，PDF 另用 `find_pdf` 获取。

保存 `batchId`，用户要求撤销这次在线导入时使用：

```powershell
& $Wisp undo_import -l $Library --batch-id $Imported.batchId
```

撤销会删除该批次新增且尚未处理的记录；已判断、阅读、收藏、写笔记、加标签或附件的记录保留。报告返回的 `removed` 和 `kept`，不要宣称一定能完全回滚，也不要把它当作撤销桌面文件导入的接口。

需要更新已保存的检索式时：

```powershell
& $Wisp sync_searches -l $Library
# 用户要求立即运行目标库中所有已启用的检索式时
& $Wisp sync_searches -l $Library --force
```

普通同步只执行开启自动更新且已到期的检索式；自动更新间隔在桌面设置。`--force` 忽略间隔和库的自动更新开关。省略 `-l` 会涉及所有文献库。`sync_searches` 即使部分失败也可能退出 0，必须检查 JSON 的 `failed` 和 `runs[].error`。

## 查询、获取全文和核对证据

```powershell
& $Wisp list_papers -l $Library --query 'Arabidopsis' --limit 20 --offset 0
& $Wisp list_papers -l $Library --screening pending --sort year-desc --limit 20
$PaperId = 'P-从查询结果取得的实际ID'
& $Wisp get_paper -l $Library --id $PaperId

# 仅列出开放全文候选，不下载
& $Wisp find_pdf -l $Library --paper-id $PaperId --look-only
# 用户需要获取全文时，下载并附到已有条目
& $Wisp find_pdf -l $Library --paper-id $PaperId

& $Wisp search_fulltext -l $Library --query 'root stem cell' --limit 10
& $Wisp read_fulltext -l $Library --paper-id $PaperId --page 1 --pages 4
& $Wisp find_citations -l $Library --text 'Single-cell profiling reveals root cell types.' --per-sentence 3
& $Wisp find_sentences -l $Library --section methods --query 'protoplast isolation' --limit 10
& $Wisp list_figures -l $Library --paper $PaperId --limit 20
& $Wisp list_pdf_annotations -l $Library --paper-id $PaperId --limit 50
```

`list_papers` 默认不返回回收站条目和判断为 `irrelevant` 的条目；需要查看后者时显式传 `--screening irrelevant`。分页使用 `--offset`，单页上限 500。

用户提到下划线、高亮、标注或阅读重点时，用 `list_pdf_annotations` 读取，支持 `--query` 搜索原文与笔记，`--resource-id`、`--page`、`--kind` 筛选以及分页。`quote` 是选中的原文，`note` 是用户笔记，不能把笔记当成论文结论。页码从 1 开始，属于 `resourceId` 指定的 PDF 附件；`boxes` 是以页面左上角为原点、单位为 PDF 点的各行矩形。标注作为独立 metadata 保存，原文件没有改写。此工具只读；新增、编辑和删除在桌面阅读器里操作。

`find_pdf` 查找开放获取版本，不登录或绕过付费墙。省略 `--paper-id` 会批量处理没有 PDF 的文献，默认 10 篇、最多 50 篇。检查每篇结果，区分已下载、没有候选和失败。

全文工具首次使用时可能提取并缓存文本；`read_fulltext` 每次最多 8 页。扫描件或无可提取文字的 PDF 需要在桌面处理；没有读到全文就明确说明仅依据摘要。`find_metadata --paper-id` 可在线查找元数据候选，但写入需在桌面核对确认。

回答时记录实际文献 ID、标题、DOI、页码及对应原句。`find_citations` 和 `find_sentences` 是词语匹配得到的候选，读上下文后才能判断是否支持结论，不能把排序当成证据。

## 提交筛选建议

先读 `library_status` 的 `screeningGoal`（为空则用 `topic`），再读 `get_paper` 和可用全文。`suggest_screening` 的 `--id`、`--verdict`、`--reason` 必填；可补 `--excerpt`、`--confidence`、`--author`。理由用文中实际信息，摘录必须逐字来自摘要或全文。

`verdict` 可取 `included`（收录）、`excluded`（界面显示“相关”，值得备查）、`irrelevant`（不相关）、`uncertain`（信息不足）。调用后核对 `excerptKept`；提供的引文未保存时，不把它报告为已验证证据。同一文献的新建议会覆盖旧建议，最终收录判断仍由用户在桌面筛选页确认。

## 通过 MCP 使用

支持 MCP 的客户端可以启动 `wisp mcp`，工具与 CLI 相同。下面是使用绝对路径和指定数据目录的配置示例，具体配置位置取决于客户端：

```json
{
  "mcpServers": {
    "wisp-paper": {
      "command": "C:\\Apps\\WispPaper\\wisp.exe",
      "args": ["--data-dir", "D:\\Literature\\WispPaper", "mcp"]
    }
  }
}
```

仅查询时可在 `args` 中增加 `--read-only`，服务会隐藏写入工具；全文读取仍可能生成缓存。MCP 中用 `library` 指定目标库，参数名称和类型以工具 schema 为准。MCP 的 `isError` 以及结构化结果中的逐项失败都需要检查。

## 完成任务时报告

交代目标库名称和 ID、资料来源或检索式、实际新增与跳过数量、批次号、PDF 获取情况，以及失败和未完成项。区分“检索命中”“已导入记录”“已保存 PDF”“已读到全文”和“已提交建议”。

联网操作使用应用「设置 → 网络」的代理，留空时跟随代理环境变量。在线检索会发送检索式，全文查找会发送 DOI / PMID。CLI 查询和导入本身不要求配置 AI 模型；桌面 AI 功能另需模型服务。

更多说明：[命令行与 MCP](docs/CLI.md)、[桌面入门教程](docs/TUTORIAL.md)、[Zotero 迁移](docs/ZOTERO.md)、[数据与隐私](docs/DATA.md)。若仅分发此技能文件，配套文档可从 [Wisp Paper 项目](https://github.com/xuzhougeng/WispPaper) 获取。
