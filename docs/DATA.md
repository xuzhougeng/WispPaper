# 数据与隐私

## 数据在哪里

| 内容 | 位置 |
|---|---|
| 文献库 | `文档/WispStudy/<文献库 ID>/`，内含 `library.json`、`wisp-paper.sqlite`、`papers/`，用到时还有 `figures/`（图片库）和 `manuscripts/`（导入的 Word 稿件） |
| 文献的书目信息（题目、作者、期刊、卷、期、页码、年份、DOI、PMID、摘要）、标签、笔记和各项状态 | 文献库的 `wisp-paper.sqlite` 中 `papers` 表；卷、期、页码是后来加的列，旧版本打开这个库时不读也不改它们 |
| 历史对话（问题、回答、随问题发出的图片） | 各文献库的 `wisp-paper.sqlite` 里，随文献库文件夹一起走；在「历史」里删除一条对话是真的删除 |
| Notion 笔记保存记录 | 各文献库的 `wisp-paper.sqlite` 中 `note_saves` 表，保存父页面、笔记页面 ID、草稿指纹及写入状态；不保存另一份笔记正文或令牌 |
| PDF 标注（下划线、高亮、原文、页码、坐标、笔记） | 文献库的 `wisp-paper.sqlite` 中 `pdf_annotations` 表；原 PDF 不改写，备份时需连同数据库一起保存；删除标注或移除其附件会删除对应标注记录 |
| 自己添加的引用样式（CSL，Zotero 用的 `.csl` 文件） | `文档/WispStudy/styles/`；被同名文件替换掉的旧内容留在 `styles/removed/` |
| 被移除的文献库、附件、图片 | 同目录下的 `removed/`（只移动，不删除）；唯一的例外是回收站：从回收站「彻底删除」或「清空回收站」的文献，PDF 和图片直接从磁盘删除 |
| 筛选的每一轮（目标、开始时间、当时各状态的篇数） | 文献库的 `wisp-paper.sqlite` 中 `screening_rounds` 表 |
| 撤销一次导入时移出的文献记录 | 文献库文件夹里的 `removed/undone-import-<批次号>.bib`（BibTeX，可以再导入）；处理过的文献不移出 |
| 设置 | 系统配置目录下的 `WispStudy/settings.json` |
| 上次的界面（打开的文献库、页面、选中的文献） | `settings.json` 旁边的 `session.json`；在「设置 › 通用 › 启动」关闭后即删除 |

数据目录可以在「设置 › 通用」里更改。环境变量 `WISP_STUDY_HOME` 可把数据和设置一起指到别处（测试用）。

本应用原名 Wisp Paper。从旧版升级时，如果只有旧的 `文档/WispPaper` 或配置目录下的 `WispPaper/settings.json`，新版直接在原处继续使用，不移动也不复制；数据库文件名 `wisp-paper.sqlite` 保持不变。旧版导出的 `.wisppaper` 文件照样可以导入。

附件在数据库里记的是相对于文献库文件夹的路径，所以整个文件夹可以拷到别处继续用。

导出的 `.wispstudy` 文件是 gzip 压缩的 tar 包：最前面的 `wispstudy.json` 说明来自哪个版本、文献库名称、文献和文件数量，其后的 `library/` 就是文献库文件夹（数据库是导出那一刻的完整副本，`removed/` 不带）。用 `tar xzf` 解开后，`library/` 可以直接放进数据目录。不含 PDF 导出时，`papers/` 不带，数据库副本里也去掉这些附件的记录，以及挂在附件上的全文文字、句子和 PDF 标注；原文献库不受影响。导入不联网，总是新建文献库。

从 Zotero 导入时，只读打开用户选择的本地 `zotero.sqlite`，把文献信息和本地 PDF 复制到当前 Wisp Study 文献库；不会改动 Zotero 记录或移动原附件。这一步不访问网络，也不调用 AI。导入范围和附件限制见 [Zotero 导入教程](ZOTERO.md)。

## 会离开本机的数据

| 操作 | 发出的内容 | 发给谁 |
|---|---|---|
| DOI / PMID 查询 | 编号 | Crossref 和 Europe PMC |
| 试检索、运行检索式（包括自动更新） | 检索式 | 所选的数据库：PubMed、Europe PMC、Crossref 或 arXiv |
| 向导拟检索式 | 文献库的名称、主题和你写的关键词 | 你为提问指定的模型服务 |
| 助手筛选 | 筛选目标和每篇文献的记录、摘要、全文 | 你为批量筛选指定的模型服务 |
| 翻译 | 文献的题目和摘要，以及翻译的要求 | 你为翻译指定的模型服务 |
| 提问 | 问题和随它粘贴的图片、相关文献的记录、笔记、全文页面，以及助手通过工具读取的 PDF 标注和标注笔记 | 你为提问指定的模型服务 |
| 保存到 Notion | 本次草稿的标题、文献元数据、选定笔记／问答和仍在草稿中的提问图片 | 已登录的官方 Notion MCP 及其官方图片上传端点 |
| 检查更新 | 对本项目 `releases/latest` 的一次请求；开着「自动下载更新」时再取这个版本的安装包和 `SHA256SUMS` | GitHub |

检查更新只读取 `releases/latest` 跳转到的版本号，不上传任何内容；可在「设置 › 通用」关闭。开着「自动下载更新」时（macOS 和 Windows，默认开），新版本的安装包在后台下载，与发布页的 SHA-256 校验值核对（macOS 还要求签名与当前应用同属一个开发者），解开后暂存在应用旁边的 `.wisp-study-update` 文件夹里，等你点「重启并更新」才替换当前应用；被替换下来的旧应用在下次启动时清掉。Windows 的安装版和便携版都这样更新。关掉它，新版本只提示，需要自己到发布页下载安装。

## API Key

API Key 以明文保存在 `settings.json`。留空则读取环境变量：预设的服务读它自己的那个（`DEEPSEEK_API_KEY`、`MOONSHOT_API_KEY`、`ZHIPUAI_API_KEY`、`ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`GEMINI_API_KEY`、`ARK_API_KEY`、`TYPESAFE_API_KEY`），自己填地址的服务按接口读 `ANTHROPIC_API_KEY` 或 `OPENAI_API_KEY`。
