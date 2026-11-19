# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. It holds what applies to every change; per-feature detail is in `docs/dev/` (see the table below). Read the matching section there before changing a feature, and put new per-feature detail there, not here.

Wisp Paper is a local-first literature library desktop app (collect papers, organise, preview PDFs, ask an LLM about the library), written in Rust with GPUI. Targets Windows and macOS; Linux compiles but is untested. The desktop UI supports Chinese and English (Settings › General). Use `i18n::t` with Chinese keys in `locales/en.json` for static labels, and `tf!` with both Rust format literals for dynamic messages. Translate shared label tables at display sites; keep stored keys, user content and model prompts unchanged. Code comments are in English.

## Commands

```sh
cargo run --release                     # run the app
cargo test                              # all four crates (workspace default-members)
cargo run -p wisp_cli -- help           # the `wisp` command line; `-- mcp` is the MCP server
cargo test -p wisp_core                 # engine only; no GPUI build, much faster
cargo test -p wisp_core store::         # one module's tests
cargo test -p wisp_core <test_name>     # one test
cargo test -- --ignored lookup          # live Crossref / Europe PMC lookups (network)
cargo clippy --all-targets              # expected to be warning-free
```

- The toolchain is pinned in `rust-toolchain.toml` (clippy only). There is no rustfmt component and no `rustfmt.toml`: the code is hand-formatted with long lines and one-line `if x { return }` bodies. Do not run `cargo fmt`; match the surrounding style.
- Windows needs the MSVC build tools.
- CI runs clippy with `-D warnings` and the tests on Windows and macOS, and the three windowless crates on Linux. A release is a pushed tag `v<version>` matching `Cargo.toml`, always signed. A pull request touching `scripts/` or the workflow rehearses the packaging unsigned, so keep `site/` out of `scripts/`. Details: `docs/dev/release-and-site.md`.
- `WISP_PAPER_HOME` moves both the data folder and `settings.json` to one directory; use it for any manual run so the real library in `Documents/WispPaper` is not touched.
- Debugging without the real library, an API key or a screen (demo data with the `seed` example, the `structure` and `docx` examples for files that come out wrong, `mock_model` as a fake model endpoint, `WISP_PAPER_DEV` steps that script a debug window and take `shot:` screenshots): `docs/dev/debugging.md`. A new debug step is added in `dev_start` at the end of `src/app.rs` and listed there.
- In a `#[cfg(test)]` module of a file that has `use gpui::*`, do not write `use super::*`: it brings in gpui's `test` attribute, and `#[test]` then fails with "recursion limit reached". Import the names you need.

## Where the detail is

| Before working on | Read |
|---|---|
| A module of the engine: docx, 批注, 配引用, 出处, 改稿, 助手写, 图片库, screening, 提取, providers / 生图 / 判断, translation, full text and structure, candidates, open full texts, search sources, note saving | `docs/dev/core.md` |
| A feature of the window: the conversations, 写作 and its modes, citations, 综述, 图片库, 提取, 筛选, 自定义 (connectors, skills, OAuth), settings, layout, keyboard and selection, the PDF view, the text input | `docs/dev/app.md` |
| Anything the person sees: layout, a new panel, header, bar or label (the design principles, 安静的版面) | `docs/dev/design.md` |
| Running, scripting and looking at the app | `docs/dev/debugging.md` |
| CI, signing, releases, `site/` | `docs/dev/release-and-site.md` |
| What each step of the roadmap built and left out | `docs/dev/roadmap.md` |
| Vendored patches | `vendor/PATCHES.md` |

User-facing docs (Chinese) are `docs/FEATURES.md`, `TUTORIAL.md`, `CLI.md`, `DATA.md`, `DEVELOPMENT.md`, `ZOTERO.md`.

## Architecture

A Cargo workspace of four crates. Dependencies point one way only: `wisp_net` depends on core, the app and the command line depend on both.

### `crates/core` (`wisp_core`): the engine, no GPUI

Everything that can be decided without a window or a socket lives here, and it is where nearly all tests are. That includes the pure halves of networked features: `lookup` builds Crossref / Europe PMC URLs and turns their JSON into a `Paper`; `ai` builds the context, the request body for the three protocols (Anthropic Messages, OpenAI-compatible chat completions, OpenAI Responses) and parses stream lines into `StreamEvent`s. The actual HTTP (blocking `ureq`) is in `crates/net` (`wisp_net`), shared by the app and `wisp`; its tests serve canned responses from a local port. Code in core that needs the network takes the fetching function as an argument (`sources::Fetch`), so core tests pass a closure.

On-disk layout: the data root holds one folder per library, each with `library.json` (`libraries`), `wisp-paper.sqlite` and `papers/` (`store`). Attachment paths in the database are relative to the library folder so a folder can be copied elsewhere. Removing a library or attachment moves it to `removed/`, and the records an undone import takes out of the library are written there as BibTeX first; nothing is deleted, with one exception: a paper leaving the trash (`Store::purge_trashed`, `Store::empty_trash`) has its files unlinked.

`store.rs` notes:
- The schema is one `CREATE TABLE IF NOT EXISTS` batch guarded by `SCHEMA_VERSION` / `PRAGMA user_version`; a newer database is refused. Bump the version when an older build would misread the data.
- Deduplication is by DOI, PMID (`paper_identifiers`), then `title_key`; a PDF taken in is also known by its bytes (`Store::paper_holding`, the `sha256` of `resources`), so the same file under another name is not a second paper. Trashed papers stay in `papers` (marked by a `trashed_papers` row) so they keep blocking re-import.
- `Store::in_memory` is what tests use.
- `Paper::fulltext` is computed, not stored: `missing`, `linked`, `stored`, or `textless` (a full-text file is there and no text came of any). Why a file gave no text is the sentence kept in `fulltext_text.error`; `fulltext::failure_kind` tells the kinds apart (`locked`, `parser`, `encoding`, `no_text`, `file`, `unsupported`), messages of earlier builds included, and a new reason is a constant there, not a string somewhere else.
- Status values are stored as English keys and shown through the label tables in `model.rs` (`SCREENINGS`, `READINGS`, …). The labels are not translations: the stored key `excluded` is displayed as 「相关」.

`tools.rs` is the one table of what an agent can do with a library; the command line, MCP and (next) the in-app assistant all end in `tools::call`, so a rule belongs there and not in a front end. `ai.rs` holds the model side of the loop: the request with tool definitions for each protocol, `TurnReader` (one streamed turn → text, tool calls, the message to send back), `tool_results`, and the slash commands (`SLASH`: the prompt templates behind `/search`, `/cite`, `/review`, `/compare` and `/write`).
- A new tool is an entry in `catalog()` (name, Chinese description, JSON Schema) and an arm in `call_in`. Nothing else: `wisp <tool>` reads its flags off the schema (`cli_args`), and `mcp::handle` lists and calls whatever the catalog holds. The test `every_tool_is_callable` fails when an entry has no arm.
- Tools take `library` (id or name, optional when there is one library) and return JSON; an error is a Chinese sentence for the caller, person or model.
- `ToolDef::writes` marks the tools that change a library (the `WRITING` list); `wisp mcp --read-only` leaves them out, and the chat reloads the library after one ran. `ToolDef::confirm` (the `CONFIRMING` list: the imports, `run_search`, `undo_import`, `remove_search`, `find_pdf`, `organise_papers`, and the two that write a manuscript, `edit_section` and `set_outline`) marks those the window's assistant runs only after the person agreed. The screening decision stays the person's: an agent may propose, never set `screening`.
- Tables added after the first schema are made beside the library's own (`CREATE TABLE IF NOT EXISTS`, no version bump), columns through `store::add_columns`.
- A model proposes, the person decides: screening status, custom fields, completed metadata and proposals on a manuscript are written only by the window on the person's action, never by a tool or a model. Quotations and excerpts are kept only when found in the text (`ai::locate_quote`, `ai::check_quote`).
- Author names go through `model::author_names`, the one reader that says which word is the family name.

### `crates/cli` (`wisp_cli`): the `wisp` binary

A thin shell over `tools` and `mcp`: argument words in, JSON out; `wisp mcp` is newline-delimited JSON-RPC on stdin/stdout. It is a separate binary because the app is a Windows GUI-subsystem program with no console. It opens the same data folder as the app (`settings::default_root` / `settings_file`), possibly while the app is running.

### Root crate (`wisp_paper`): the GPUI app

- `src/app.rs` holds all state in one root view, `Workspace` (`lib: Option<Library>` is the open library, `None` is the home page). Every click is a `Cmd` enum value, wired with `self.on(Cmd::…, cx)` and handled in `Workspace::run`. Popup menus are built from a `MenuKind` in `open_menu`; dialogs are `Modal` variants, built and submitted in `app.rs`.
- `home.rs`, `library.rs`, `detail.rs`, `chat.rs`, `dialogs.rs` are `impl Workspace` blocks that only render. A new feature usually means: a `Cmd` variant, its arm in `run`, and the element that emits it.
- Store writes go through `Workspace::change`, which runs the closure, calls `Library::reload` (re-reads papers, fields, files, trash and refilters) and shows a toast on failure. `Library::shown` is indices into `papers`, recomputed by `refilter`.
- Background work (PDF text extraction, lookups, chat streaming) runs on `cx.background_executor()` and comes back through `this.update`. Each task captures the library id (chats also a `generation` counter) and stops when the user has since closed the library or started a new conversation; keep that check in new async code.
- A library has three conversations, told apart by `Talk` (`Library`, `Paper`, `Write`); every function and command of the chat takes one, and `chat_of!(lib, talk)` borrows the one meant.
- `shortcuts.rs` owns every key binding. `shortcuts::rebuild` clears and re-binds all of them (the text field's too) from the defaults plus `Settings::shortcuts`; call it after any change. While 设置 › 快捷键 waits for a key, all bindings are cleared and `Workspace::record_key` receives the key. A new shortcut needs an entry in `SHORTCUTS`, an action, its binding in `rebuild`, an `.on_action` in `home.rs` and an arm in `Workspace::shortcut`.
- Motion is `with_animation` through the helpers in `ui.rs` (`spinner`, `fade_in`). An animation's state lives under its element id, so give animated rows an id made from what they show (a paper id), not their position: the screening queue is sorted anew with every suggestion.
- Colours come only from `theme.rs`, which has a light and a dark set (`theme::dark()`); `Workspace::apply_appearance` picks one from the setting or the system. Do not hard-code colours elsewhere; text on an accent fill is `Theme::on_accent()`, a library colour is `Accent::strong()` / `soft()`.
- An element only hears the pointer moving over itself: while something is dragged (an edge, a floating window, a selection) a layer over the whole window takes the moves (`home.rs`).
- Text a reader may want to copy goes through `Workspace::selectable`; an owner's change to a text field is `TextInput::replace_range` / `replace_apart`, one step of its Undo.
- A row of a `uniform_list` needs `w_full()`, and a text it shows on one line goes through `ui::one_line`: `truncate` keeps hard line breaks, which draw over the next row. A `Div` built with `flex_1()` keeps its basis of 0 under a later `flex_none().w(…)`: give it a parent of that width. A wrapping `TextInput` under ancestors whose widths are all percentages is measured at 240px: give its wrapper a pixel width.
- Never query the database from render: cache in `Library::reload` or once a frame before drawing.
- `build.rs` embeds every file under `assets/` (of `branding/`, only `app-icon.png`) into the binary; icons are loaded by name through `ui::icon`. On Windows it also compiles the icon resource.

### `vendor/`

`gpui` 0.2.2, `taffy` 0.9.0 and `xattr` 0.2.3 are vendored with local patches and wired in through `[patch.crates-io]`. Patched spots carry a `quiet.md:` comment and are listed in `vendor/PATCHES.md`. When changing vendored code, add a row there and keep the comment; do not bump these versions without re-applying the patches. GPUI APIs here are 0.2.2 plus those patches (for example `grid_col_widths`, `ListState::estimated_scroll_metrics`), so check `vendor/gpui/src` rather than upstream docs.

## Scope

Ported from BiblioCell, without its datasets and web version. The roadmap (tools with CLI and MCP, the agent chat, search and import, screening, 写作, 图片库, 批注, 改稿, 生图, 判断, 出处, 助手写) and what each step left out are in `docs/dev/roadmap.md`. API keys are stored in plain text in `settings.json`; when empty, `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` are read from the environment.
