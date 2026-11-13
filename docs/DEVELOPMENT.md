# 开发

## 构建

需要 Rust（版本见 `rust-toolchain.toml`，rustup 会自动安装）。Windows 上需要 MSVC 构建工具。

```sh
cargo run --release                 # 运行
cargo test                          # 核心引擎、命令行和网络层的测试
cargo test -- --ignored lookup      # 访问 Crossref / Europe PMC 的真实查询测试
cargo clippy --all-targets          # 应当没有警告
```

代码是手工排版的，仓库里没有 rustfmt 配置，请不要运行 `cargo fmt`，照着周围的风格写。

生成一个带示例数据的目录来试用：

```sh
cargo run -p wisp_core --example seed -- /tmp/wisp-demo some.pdf
WISP_PAPER_HOME=/tmp/wisp-demo cargo run
```

手头没有 API Key 时，`cargo run -p wisp_core --example mock_model -- 8787` 是一个假的 Claude 接口；把示例目录 `settings.json` 里的一个模型服务指向 `http://127.0.0.1:8787` 即可。端口后面再写一个毫秒数（`-- 8787 1500`），每个回答就晚这么久，可以看清批量筛选的进度；它会打印每个请求的模型和思考参数，用来核对各个任务用的是哪个模型。提问里带「试试撤销」时，它改为调用 `undo_import`，用来看助手改动文献库前的确认。

## 调试用的环境变量

| 变量 | 作用 |
|---|---|
| `WISP_PAPER_HOME` | 把数据目录和 `settings.json` 一起指到一个文件夹，手动试用时不碰真正的文献库 |
| `WISP_PAPER_DEV` | 仅调试构建：让窗口启动后直接进入某个状态，如 `open:0,select:0,tab:pdf`、`settings:ai`、`theme:dark`（步骤见 `src/app.rs` 末尾的 `dev_start`） |
| `WISP_PAPER_UPDATE_URL` | 把检查更新指向别的地址（测试用） |

## 界面布局检查

修改文案或布局后，分别用中文和英文检查 900×560 与 1360×860 窗口；详情面板打开时也要检查主区域。长说明应换行，操作栏允许换行，较长页面和弹窗应能滚动到所有控件。文献标题、文件名等列表摘要可以省略，但按钮不能越界或遮住相邻内容。写作区不足 680 像素时，编辑器和候选句上下排列。

覆盖首页、文献列表与详情、库内及单篇提问、历史与综述记录、检索与建库向导、三种筛选视图、写作及历史版本、图片库、全部设置页，以及新建/编辑文献库、添加/编辑文献、自定义词条、检索式、Zotero、导入进度、证据和确认弹窗。PDF 另检查长文件名、缩放菜单、截图收藏、图片编辑和悬浮提问。

`WISP_PAPER_DEV` 可用 `size:900;560` 调整窗口，`language:en` 切换语言，`ui:clear` 关闭弹窗与设置；`ui:new-library|edit-library|add|manual|edit-paper|fields|new-search|zotero|delete-paper|guide|guide-search` 打开对应界面（每次选一个动作）。同一时刻有多个依赖步骤时，用 `do:ui:clear|page:papers|select:0|tab:info` 顺序执行，再用 `2@shot:/tmp/layout.png` 保存离屏渲染供检查。始终使用单独的 `WISP_PAPER_HOME`，模型交互使用本地 mock。

## 代码结构

界面支持 `zh` / `en`，在「设置 › 通用」即时切换；`settings.json` 的 `language` 保存选择，旧配置及未知语言默认中文。静态文案使用 `src/i18n.rs` 的 `t`，英文维护在 `locales/en.json`；带变量的文案使用 `tf!("中文格式", "English format", ...)`，两个格式都由 Rust 检查。共享状态表在显示处翻译，输入框在绘制时翻译占位文字。文献内容、笔记、稿件和模型提示不随界面语言改写。调试脚本支持 `language:en` / `language:zh`。

仓库是一个 Cargo workspace，分四个 crate：core 不依赖其他三个，net 只依赖 core，应用和命令行依赖这两个。

### `crates/core`（`wisp_core`）

文献库引擎，不依赖 GPUI，可以单独编译和测试。

| 模块 | 内容 |
|---|---|
| `model` | 文献、自定义词条、附件；DOI / PMID 规范化 |
| `libraries` | 数据目录下的各个文献库 |
| `store` | 一个文献库：SQLite 数据库和 `papers/` 里的附件 |
| `filter` | 搜索、筛选、排序、侧栏计数 |
| `import` / `export` | BibTeX、RIS 读入；BibTeX、CSV 写出 |
| `zotero` | 只读 Zotero 本地 SQLite 快照，读取元数据、标签、笔记与分类，逐条去重并复制 PDF；单条失败保留在结果中 |
| `lookup` | Crossref / Europe PMC 返回值 → 文献 |
| `sources` | 文献数据库检索：要请求的地址，以及从返回内容读出的记录 |
| `fulltext` | PDF 逐页文字提取；引文与页面字形的比对 |
| `markdown` | 回答里的 Markdown → 要画的块（标题、列表、表格、代码块和行内标记） |
| `ai` | 提问的上下文组装、三种协议的请求体（含工具定义）、流式返回和工具调用的解析、斜杠命令 |
| `settings` | settings.json，以及它和数据目录的位置 |
| `tools` | agent 能对文献库做的事：命令行、MCP 和应用内助手共用的一张工具表 |
| `mcp` | 这些工具的 MCP（JSON-RPC）应答 |
| `update` | 最新发布的版本号，是否比当前版本新 |

### `crates/net`（`wisp_net`）

HTTP 请求（阻塞的 ureq）：元数据查询、文献检索、流式对话、检查更新。

### `crates/cli`（`wisp_cli`）

可执行文件 `wisp`。把命令行参数按工具的 schema 转成调用，`wisp mcp` 在标准输入输出上收发 MCP 消息。

### 根 crate（`wisp_paper`）

GPUI 应用。

| 模块 | 内容 |
|---|---|
| `app` | `Workspace`：全部状态；每次点击是一个 `Cmd`，由 `run` 处理 |
| `home` / `library` / `detail` / `chat` / `dialogs` | 各部分界面的绘制 |
| `markdown` | 把 `wisp_core::markdown` 读出的块画出来：助手的回答 |
| `settings` | 设置页：通用、外观、快捷键、AI、网络、关于；检查更新 |
| `shortcuts` | 快捷键的默认值、改键规则和显示 |
| `input` | 文本输入框（单行 / 多行，支持输入法） |
| `pdf` | PDF 预览：后台线程渲染，虚拟化列表显示；在页面的字形里定位引文并高亮 |
| `guide` | 建库向导：从哪里开始，助手拟的检索式和它们的试检索结果 |
| `searches` | 「检索与导入」页面：检索式、试检索结果、导入记录 |
| `screen` | 「筛选」页面：待判断的队列、助手这一轮的进度、按建议分组、确认按钮 |
| `theme` / `ui` | 浅色 / 深色两套颜色与通用控件；窗口标题栏 |

### `vendor/`

`gpui`、`taffy` 和 `xattr` 带着本地补丁放在仓库里，通过 `[patch.crates-io]` 接入。改过的地方有 `quiet.md:` 注释，并列在 `vendor/PATCHES.md`。

## 打包与发布

```sh
scripts/package-macos.sh                    # dist/Wisp Paper.app 和 DMG，本机架构，ad hoc 签名
scripts/package-macos.sh --arch universal   # Apple 芯片 + Intel（需要两个 rustup target）
scripts/package-macos.sh --arch arm64       # 只有 Apple 芯片；--arch x64 只有 Intel（发布时这两个各出一个 DMG）
pwsh -File scripts/package-windows.ps1      # dist/WispPaper-v<版本>-windows-x64.zip，不签名
```

没有签名身份时，macOS 的包只能在本机运行，不适合分发。本机 `cargo` 用不了固定的工具链时，用 `CARGO="cargo +stable"` 指定；Xcode 没装 Metal Toolchain 时脚本会自己改用 `gpui/runtime_shaders`。

发布一个版本：

1. 把四个 `Cargo.toml` 的 `version` 改成新版本，写 `.github/release-notes/v<版本>.md`，合并到 `main`。
2. 在 `main` 上打标签 `v<版本>` 并推送。`.github/workflows/release.yml` 会核对标签和版本一致，然后在三个系统上编译、打包、签名，全部成功才创建 Release。

签名沿用 BiblioCell 的方案，需要在仓库的 Actions secrets 里放十个值（名字和 BiblioCell 相同，可以用同一套证书）：

| 用途 | Secrets |
|---|---|
| macOS：Developer ID 证书（.p12 的 base64）和它的密码 | `MAC_CERT_P12_BASE64`、`MAC_CERT_PASSWORD` |
| macOS：Apple 公证 | `APPLE_ID`、`APPLE_TEAM_ID`、`APPLE_APP_PASSWORD` |
| Windows：SSL.com eSigner | `ESIGNER_CREDENTIAL_ID`、`ESIGNER_USERNAME`、`ESIGNER_PASSWORD`、`ESIGNER_TOTP_SECRET`、`ESIGNER_PIN` |

缺任何一个，工作流在开头就失败，不会发布没有签名的包。改动打包脚本或工作流的 pull request 只做一次不签名的预演，包留在这次运行的 artifacts 里。想先看签名对不对再发布，在 Actions 页手动运行 Release，填已有的标签，不勾选 publish。
