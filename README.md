<p align="center">
  <img src="assets/branding/app-icon.png" width="96" alt="Wisp Paper 的应用图标">
</p>

<h1 align="center">Wisp Paper</h1>

<p align="center">
  <b>本地优先的文献库：收集、筛选、阅读，向你的文献提问。</b><br>
  用 Rust + GPUI 原生绘制。文献和 PDF 留在你的电脑上，每个文献库就是一个文件夹。
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-lightgrey" alt="Windows 和 macOS">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20GPUI-dea584?logo=rust" alt="用 Rust 和 GPUI 构建">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/xuzhougeng/WispPaper?color=green" alt="MIT 许可"></a>
  <img src="https://img.shields.io/badge/status-alpha-orange" alt="Alpha">
</p>

<p align="center">
  <a href="https://xuzhougeng.github.io/WispPaper/"><b>主页</b></a> ·
  <a href="#安装"><b>安装</b></a> ·
  <a href="#功能">功能</a> ·
  <a href="docs/TUTORIAL.md">教程</a> ·
  <a href="docs/ZOTERO.md">Zotero 导入</a> ·
  <a href="docs/CLI.md">命令行与 MCP</a> ·
  <a href="docs/DATA.md">数据与隐私</a> ·
  <a href="docs/DEVELOPMENT.md">开发</a>
</p>

<p align="center">
  <img src="docs/images/library.png" width="100%" alt="Wisp Paper 的文献库页面：左侧是视图和标签，中间是文献列表，右侧是所选文献的详情和笔记。">
</p>

> [!NOTE]
> Wisp Paper 还在早期，变化很快。界面只有中文。第一次用，可以跟着[教程](docs/TUTORIAL.md)从零建一个文献库。遇到问题或有想法，欢迎[提 issue](https://github.com/xuzhougeng/WispPaper/issues)，更欢迎直接发 pull request。

## 为什么是 Wisp Paper

**🔒 本地优先，文献是你自己的。** 不需要账号，没有云端。每个文献库是数据目录下的一个独立文件夹，数据库、PDF、自定义词条都在里面，可以单独备份或整个拷走。删除只是移动到 `removed/`，什么都不会真的丢。哪些数据会离开本机，逐条写在[数据与隐私](docs/DATA.md)里。

**🐋 先广泛导入，再让助手帮你筛。** 为 PubMed、Europe PMC、Crossref、arXiv 保存检索式，成批导入；助手逐篇阅读，给出建议、理由和一句逐字引自原文的依据。建议只是建议：每一篇收不收，由你确认。

**💬 回答带出处，出处能点开。** 向整个文献库或单篇文献提问，材料不够时助手自己去检索全文、按页翻阅。回答里的 `[P3 p.5]` 可以点击，打开 PDF 跳到那一页，所引的原句在页面上高亮。

**⚡ 原生界面，不是网页。** 整个界面由 Rust 和 [GPUI](https://www.gpui.rs/) 在 GPU 上绘制，没有 WebView，没有 Electron。PDF 用纯 Rust 的 [hayro](https://github.com/LaurenzV/hayro) 渲染，不依赖系统 PDF 库。

## 安装

从[发布页](https://github.com/xuzhougeng/WispPaper/releases/latest)下载：

| 平台 | 文件 | 说明 |
|---|---|---|
| **macOS**，Apple 芯片 | `WispPaper-v<版本>-macos-arm64.dmg` | Developer ID 签名并经 Apple 公证。打开后把 Wisp Paper 拖进「应用程序」 |
| **macOS**，Intel | `WispPaper-v<版本>-macos-x64.dmg` | 同上 |
| **Windows** 10 / 11（x64） | `WispPaper-v<版本>-windows-x64.zip` | 解压后运行 `WispPaper.exe`，带 Authenticode 签名 |
| **Linux**（x64） | `WispPaper-v<版本>-linux-x64.tar.gz` | 解压后运行 `wisp-paper`；没有签名，也没有在真机上测试过 |

命令行工具 `wisp` 随包附带：Windows 和 Linux 在主程序旁边，macOS 在 `Wisp Paper.app/Contents/MacOS/wisp`。

从源码构建需要 Rust（版本见 `rust-toolchain.toml`，rustup 会自动安装）；Windows 上还需要 MSVC 构建工具。

```sh
git clone https://github.com/xuzhougeng/WispPaper
cd WispPaper
cargo run --release
```

想先看看效果，可以生成一个带示例数据的目录，不碰你真正的文献库：

```sh
cargo run -p wisp_core --example seed -- /tmp/wisp-demo some.pdf
WISP_PAPER_HOME=/tmp/wisp-demo cargo run
```

提问、建库向导、筛选、翻译和写作页的模型分节需要模型服务，在「设置 › AI」里配置：Claude API、OpenAI 兼容的 chat/completions，或 OpenAI 的 Responses 接口。可以放进多个模型，提问、批量筛选、翻译、整理全文各用哪一个、各自思考到什么程度分别指定。

## 功能

- **文献库**：多个独立的文献库，各有名称、主题描述和标识色。
- **收集**：粘贴 DOI 或 PMID，拖入 PDF，导入 BibTeX / RIS，或手动录入；也可以[从 Zotero 本地数据库导入](docs/ZOTERO.md)文献信息、标签、笔记及已下载的 PDF。按 DOI、PMID、题目去重。成批导入有进度和结果汇总。
- **整理**：搜索、筛选、排序；阅读状态、收录判断、收藏、标签、笔记；每个库自己定义的词条；回收站；导出 BibTeX 和 CSV。
- **检索与导入**：保存检索式，先试检索再导入，每次导入可以整批撤销；可以定时自动更新。
- **建库向导**：写下主题，助手为各个数据库拟几条检索式并逐条试检索，你挑靠谱的。
- **筛选**：助手几篇同时读，给出收录建议和原文依据，你来确认；也可以按建议整组采纳。
- **翻译**：题目和摘要一键译成中文，译过的保存在文献库里。
- **写作**：分部分写稿（摘要、导言、方法、结果、讨论），旁边列出库里文献在同一部分的句子：还没动笔时是各篇的开头，写起来之后是与正在写的这一段最相近的句子，中文也能找。句子可以插入引用、复制，或在 PDF 里定位到原句。入库的 PDF 按版面读出文字、按标题分节；没有标题可依的（如 Nature 的导言）可以让模型判断，扫描件可以让模型读页面。
- **提问**：对文献库或单篇文献提问，回答按 Markdown 排版；助手引的原句会和原文逐字比对，结果标在出处上，点击出处在 PDF 里高亮。单篇提问时对话和 PDF 并排，PDF 上选中的原文或框出的截图可以直接带进问题；回答里的文字可以选中、复制、引用。输入 `/` 选择命令（检索全文、配引用、分阶段写综述、比较文献、逐条核对），可以粘贴图片，对话保存为历史、可以接着问，回答可以存为这篇文献的笔记。
- **PDF**：在详情面板里直接看，面板宽度可以拖动；缩放、适应宽度或整页；选字、复制、截图提问。读不出文字的 PDF 会说明原因，可以让能看图的模型读页面。
- **查找开放全文**：为没有 PDF 的文献批量查找开放获取的全文，下载后附到原条目；不绕过付费墙。
- **命令行与 MCP**：`wisp` 不开窗口就能读写同一个数据目录，`wisp mcp` 把同一组工具交给外部的 AI 客户端。见 [docs/CLI.md](docs/CLI.md)。
- **设置**：浅色 / 深色，可改键的快捷键，全局代理和每个模型服务单独的代理。改动即时保存。

每一项的细节在 [docs/FEATURES.md](docs/FEATURES.md)。

## 文档

| | |
|---|---|
| [教程](docs/TUTORIAL.md) | 从零建一个「植物单细胞转录组」文献库的完整记录，带截图 |
| [功能详解](docs/FEATURES.md) | 每个页面能做什么 |
| [数据与隐私](docs/DATA.md) | 文件放在哪里，哪些数据会离开本机 |
| [命令行与 MCP](docs/CLI.md) | `wisp` 的用法、工具一览、MCP 配置 |
| [开发](docs/DEVELOPMENT.md) | 构建、测试、代码结构 |

## 致谢

文献部分移植自 [BiblioCell](https://github.com/xuzhougeng/BiblioCell)（它的数据集和网页版没有移植），应用结构和回答里的 Markdown 读法参考 [quiet-md](https://github.com/s1dashu/quiet-md)。

## 许可

代码以 [MIT](LICENSE) 许可发布。随附的第三方代码和素材见 [THIRD_PARTY.md](THIRD_PARTY.md)。
