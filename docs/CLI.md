# 命令行与 MCP

`wisp` 是不开窗口的入口，读写和应用同一个数据目录，应用开着时也能用。它提供的每个工具既是一个子命令，也是一个 MCP 工具，应用内的助手用的也是同一张工具表。

## 命令行

```sh
cargo build --release -p wisp_cli       # 生成 target/release/wisp

wisp help                               # 命令一览
wisp list_libraries
wisp list_papers -l 大语言模型 --query attention --limit 10
wisp get_paper <文献 ID>
wisp search_fulltext "10x Genomics Chromium"
wisp read_fulltext <文献 ID> --page 3
wisp search_fulltext --help             # 一个工具的参数
```

输出是 JSON；出错时信息写到 stderr，退出码为 1。只有一个文献库时可以省略 `-l`；`--data-dir 文件夹` 指定另一个数据目录。

## 工具

| 工具 | 用途 |
|---|---|
| `list_libraries` | 所有文献库的 ID、名称、主题和文献数量 |
| `library_status` | 一个库的概况：各状态的数量、自定义词条、状态值的显示名称 |
| `list_papers` | 搜索、筛选、翻页；规则与界面上的列表相同 |
| `get_paper` | 一篇文献的完整记录、词条和附件 |
| `search_fulltext` | 在已保存的全文里找短语或关键词，返回页码和逐字片段 |
| `read_fulltext` | 按页读取全文，一次最多 8 页 |
| `find_citations` | 为一段文字逐句找出库里可能支持它的文献（按词语匹配的候选） |
| `find_sentences` | 写作参考：库里各篇文献在同一部分（摘要、导言、方法、结果、讨论）的句子；给 `--query` 时按相近程度排，不给时返回各篇这一部分的开头 |
| `search_online` | 在 PubMed、Europe PMC、Crossref 或 arXiv 试运行一个检索式，不写入；标出每条结果是否已在库中 |
| `find_metadata` | 为缺 DOI 或题目的文献按题目到 Crossref、Europe PMC 和 arXiv 查找可能对应的记录，不写入；返回各来源的情况和候选（相似度、一致与不一致之处、是否已是库里的另一篇）。`--paper-id` 必填，`--title`、`--author`、`--year` 可选。写入由人在窗口里核对后确认 |
| `list_searches` / `save_search` / `remove_search` | 查看、保存、删除检索式（后两个会改动文献库） |
| `run_search` | 运行一个保存的检索式并导入，已有的文献跳过，返回批次号（会改动文献库） |
| `list_imports` / `undo_import` | 导入记录；整批撤销一次导入，处理过的文献保留（撤销会改动文献库） |
| `sync_searches` | 运行到期的检索式（所有开启了自动更新的文献库，或指定的一个）；`--force` 不看间隔（会改动文献库） |
| `suggest_screening` | 为一篇文献提出收录建议（结论、理由、逐字引文、把握）；不改变收录判断，由用户在筛选页确认 |
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
