# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A small Rust CLI (package/binary `bot-notification`) that sends markdown content to a messaging-bot webhook — **WeCom** (Enterprise WeChat, `markdown_v2`) or **Feishu/Lark** (`interactive` card; markdown tables become real `table` elements). Platform is chosen with `--platform`.

## Module layout

- `main.rs` — CLI (`Args`, `Platform`), per-platform size constants, and the `main()` pipeline.
- `chunk.rs` — `split_content`: byte-bounded chunking on line boundaries, re-prepending table headers across splits.
- `content.rs` — `translate` (osv-scanner English→Chinese) and `git_header` (repo summary).
- `webhook.rs` — `build_url` / `build_wechat_payload` / `build_feishu_payload` / `feishu_sign` / `is_success`.
- `feishu.rs` — `parse_blocks` (Text/Table) + `pack_feishu_cards` (greedy multi-card packing).

Each module owns its own `#[cfg(test)] mod tests`. Cross-module calls are `pub(crate)`; `Platform` lives in `main.rs`.

## Commands

- **Build:** `cargo build --release` (release profile uses `lto`, `strip`, `opt-level = "z"` for a minimal binary)
- **Test:** `cargo test --release` (CI runs tests in release mode)
- **Single test:** `cargo test --release <test_name>` (e.g. `cargo test --release table_header`)
- **Run:** `cargo run --release -- --key <KEY> [-f path/to/file.md] [-g path/to/repo] [--no-translate] [-P wechat|feishu] [--secret <SECRET>]`
  - `--platform` defaults to `wechat`. For `feishu`, `--key` accepts either a full webhook URL or a bare hook token.
  - Without `-f`, reads markdown from stdin: `echo "hello" | cargo run --release -- --key <KEY>`
- **Release:** pushing a `v*` git tag triggers `.github/workflows/release.yml`, which cross-compiles for `x86_64-unknown-linux-musl`, `x86_64-apple-darwin`, and `aarch64-apple-darwin` and attaches the binaries to a GitHub Release. Commits/PRs (non-tag) trigger `.github/workflows/ci.yml`.

## Architecture

The program is a linear pipeline over the input content:

**read → translate → prepend git header → chunk → POST each chunk**

Each stage, in `main()`:

1. **Read** — from `-f <file>` or stdin.
2. **Translate** (`translate`, on by default) — regex-replaces osv-scanner-style English vulnerability summary lines with Chinese. The patterns are specific (package count, severity breakdown, "N vulnerabilities can be fixed"). If you touch osv-scanner output handling, update both the regex and its corresponding test (`translate_osv_summary`).
3. **Git header** (`git_header`, only with `-g`) — shells out to `git` (`git -C <repo> ...`) to pull branch/commit/author/date and formats a Chinese-labeled header (`**仓库**`, `**分支**`, ...). Failures here degrade gracefully (warns and continues).
4. **Chunk / pack** — WeCom: `split_content` (`chunk.rs`) into ≤`WECHAT_MAX_BYTES` (4096) text chunks. Feishu: `parse_blocks` + `pack_feishu_cards` (`feishu.rs`) into ≤`FEISHU_MAX_BYTES` (18000-byte) interactive cards, each holding ≤5 `table` elements.
5. **POST** (`ureq`) — sends each chunk; success is platform-specific (`is_success`: WeCom checks `errcode == 0`, Feishu checks `code == 0`). Exits non-zero on the first failure.

### Chunking is the subtle part

`split_content` (in `chunk.rs`) chunks text to a byte limit (`WECHAT_MAX_BYTES` = 4096 for WeCom), preferring line boundaries. Two things make it non-obvious:

- **Table-aware continuation** — if a split lands inside a markdown table, the continuation chunk gets the table's `header\n| --- |\n` lines re-prepended so it still renders as a valid table. `annotate_lines` walks the content, detects `header row + separator` pairs, and tags every subsequent `|`-prefixed body row with the index of its header (stored in `headers`). `split_content` consults that tag when starting a new chunk.
- **UTF-8 safety** — `hard_split` backs up to a UTF-8 char boundary when a single line exceeds the limit, so chunks never slice a multibyte char.

There are tests (`#[cfg(test)] mod tests`) covering each of these edge cases; keep them green when changing the splitter.

### Platform abstraction (`webhook.rs`)

`build_url`, `build_wechat_payload`, `build_feishu_payload`, and `is_success` branch on `Platform` (a clap `ValueEnum` defined in `main.rs`). `--secret`, when given with `--platform feishu`, produces a one-time Feishu signing pair (`feishu_sign`: `Base64(HmacSHA256("<ts>\n<secret>", ""))`) reused for every card; with `wechat` it is ignored.

- **WeCom** payload: `{"msgtype":"markdown_v2","markdown_v2":{"content":"..."}}` → `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=<KEY>`.
- **Feishu** payload: `{"msg_type":"interactive","card":{"elements":[...]}}` (+ optional top-level `timestamp`/`sign`) → the webhook URL passed as `--key` (or `https://open.feishu.cn/open-apis/bot/v2/hook/<token>` when a bare token is passed). `elements` is a mix of `{"tag":"markdown"}` and `{"tag":"table"}` items produced by `feishu.rs`.

### Feishu table rendering (`feishu.rs`)

Feishu's markdown element does **not** support `| ... |` table syntax, so `parse_blocks` detects markdown tables and `pack_feishu_cards` emits them as Feishu `table` elements (`data_type: "text"`, `page_size: 10`); non-table text becomes `markdown` elements. Constraints enforced during packing: ≤5 `table` elements per card, ≤20KB request body, ≤50 columns — large tables auto-split across rows/cards. Needs Feishu client ≥ V7.4.

### Feishu caveats

- Feishu rate-limits a bot to 100 msg/min, 5 msg/s. Packing doesn't throttle; a large report split into many cards can hit `code 11232`.
- Feishu markdown special chars (`<` `>` `*` …) are officially expected HTML-escaped; this tool does not escape (kept on par with WeCom).
