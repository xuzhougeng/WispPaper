# 第三方组件与素材

Wisp Paper 本体以 MIT 协议发布（见 [LICENSE](LICENSE)）。下面是随仓库一起分发的第三方代码和素材。Cargo 依赖各自的许可见 `cargo metadata`，这里不逐项列出。

## vendor/

`vendor/gpui`、`vendor/taffy`、`vendor/xattr` 取自 [quiet-md](https://github.com/s1dashu/quiet-md)（MIT，Copyright (c) 2026 s1dashu）的 `vendor/` 目录：分别是 crates.io 上的 gpui 0.2.2、taffy 0.9.0、xattr 0.2.3 加上 quiet-md 的本地补丁（Windows 输入法、中文避头尾换行等），补丁清单见 [vendor/PATCHES.md](vendor/PATCHES.md)。本项目没有再改动它们。

| 目录 | 上游 | 许可 |
|---|---|---|
| `vendor/gpui` | [gpui](https://crates.io/crates/gpui) 0.2.2，Zed Industries | Apache-2.0（`vendor/gpui/LICENSE-APACHE`） |
| `vendor/taffy` | [taffy](https://crates.io/crates/taffy) 0.9.0 | MIT（`vendor/taffy/LICENSE.md`） |
| `vendor/xattr` | [xattr](https://crates.io/crates/xattr) 0.2.3 | MIT / Apache-2.0 |

## 源码

- `src/input.rs` 的文本输入框参考了 gpui 的 `examples/input.rs`（Apache-2.0）和 quiet-md 的 `src/input.rs`（MIT）。
- `build.rs`、`src/assets.rs` 的资源嵌入方式来自 quiet-md（MIT）。
- `src/settings.rs` 设置页的布局和控件（分组卡片、开关、分段选择、快捷键录制）参照 quiet-md 的 `src/settings.rs`、`src/shortcuts.rs`（MIT）。
- `crates/core/src/markdown.rs` 读 Markdown 的规则（块的划分、列表嵌套、表格、行内标记）移植自 quiet-md 的 `crates/core/src/paste.rs`（MIT）。
- `scripts/sign-windows-esigner.ps1` 和发布工作流里的签名步骤来自 [BiblioCell](https://github.com/xuzhougeng/BiblioCell) 的 `scripts/` 和 `.github/workflows/release.yml`。
- 文献模型、去重规则、BibTeX / CSV 导出、筛选排序移植自 [BiblioCell](https://github.com/xuzhougeng/BiblioCell)。

## 图标

`assets/icons/*.svg` 除 `provider-*.svg` 外全部来自 [Lucide](https://lucide.dev/)（lucide-static 0.469.0，ISC 许可，见 `assets/icons/LICENSE-lucide`），文件名按用途改过。

`assets/icons/provider-{deepseek,kimi,glm,claude,openai}.svg` 是各模型服务的标志，取自 [lobe-icons](https://github.com/lobehub/lobe-icons)（@lobehub/icons-static-svg 1.100.0，MIT 许可），只用来在 设置 › AI 里认出是哪家的服务；标志本身归各自的公司所有。`provider-custom.svg` 是 Lucide 的 server。

`assets/branding/` 里的应用图标为本项目绘制。
