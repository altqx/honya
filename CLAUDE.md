# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

honya (本屋) is a Ratatui terminal app for AI-assisted **Japanese → Thai** light-novel translation. It imports an EPUB, pre-processes it (spine-ordered chapters, relocated illustrations, HTML→Markdown cleanse), and runs a three-agent LLM pipeline (Orchestrator · Translator · Reviewer) over an OpenRouter-compatible API. Binary name is `honya`; Rust edition 2024.

## Commands

```sh
cargo run --release          # launch the TUI in the current working directory (= the "shelf")
cargo test                   # full suite (~57 tests: cleanse rules, EPUB parse, UI render smoke, mock e2e)
cargo test <name>            # single test, e.g. cargo test reference_ctx_scopes_to_chunk
cargo clippy --all-targets --locked -- -D warnings   # CI lints with warnings-as-errors; match this locally
```

Running the app (not the tests) requires an OpenRouter API key, resolved in order: `HONYA_API_KEY` → `OPENROUTER_API_KEY` env → `~/.config/honya/config.json` → interactive startup prompt. The test suite uses a mock client and needs no key.

Version is the single source of truth in `Cargo.toml`; CI auto-tags on version change, so a release is cut by bumping `version` there.

## Changelog — keep it current (mandatory)

**Every** new feature, bug fix, or otherwise user-noticeable change MUST be recorded in the web changelog at `web/public/changelog.html` as part of the same change — it is not optional and not a follow-up. The changelog page is the user-facing history shown at `https://honya.altqx.com/changelog`.

How to add an entry:
- Find (or add) the `<article class="release">` block for the **current `Cargo.toml` version** (entries are newest-first; the topmost release is the latest). When bumping `version` for a release, add a new `<article>` at the top of the timeline and move its `rel-badge` "ล่าสุด" marker there (drop it from the previous latest).
- Add one `<li class="change">` per change, using the right tag: `add` (เพิ่ม) for features, `chg` (ปรับปรุง) for changes/improvements, `fix` (แก้ไข) for bug fixes.
- Write entries in **Thai** to match the Thai-localized site, but keep code identifiers, key names, file formats, agent names, and commands as-is in `<code>` (same translate-vs-keep rules as the rest of `web/public/`). Keep them concise and user-facing — describe the behavior, not the implementation.
- Also update the "เวอร์ชันล่าสุด" pill near the top of the page when the latest version changes.

## Architecture

### Event loop & concurrency contract (the most important thing to understand)

`main.rs` runs one `tokio::select!` loop that fans in three sources: a 100ms animation ticker (`app.frame`), terminal key input via crossterm's `EventStream`, and background `AppEvent`s over an **unbounded mpsc channel**. The terminal is always restored before any error is printed.

The hard rule: **background tasks never touch `App` state directly.** Long-running work (the translation pipeline, EPUB import) is `tokio::spawn`ed and communicates back *only* by sending `AppEvent`s through a cloned `EventTx`. The UI thread folds those events into state in `App::on_app_event`. `model.rs` defines `AppEvent` — it is the shared vocabulary between workers and the UI; adding a worker→UI signal means adding a variant there and handling it in `on_app_event`.

### App state machine (`src/app/`)

`App` owns everything: the five screens (`Shelf`/`Project`/`Translate`/`Reader`/`Lexicon`), the current `Overlay`, config, theme, and the active project. Key handling is a strict three-step funnel:

1. `App::on_key` → `route_key` decides what a key *means* given overlay/screen/capture state and returns an **`Action`** (it generally must not mutate state itself). Overlays get first refusal; a focused text field (`screen_is_capturing`) swallows single-letter globals; then global keys (`1`-`5`, Tab, `?`, `:`, `` ` ``, `q`) and toast dismissal (`Esc`/`Backspace`); then the active screen. `l` opens the activity log except on the Project tab, where it is the screen-local expand/focus key.
2. `apply(action)` is the **single mutation funnel** — every state change and every spawn of background work goes through here.

Each screen module (`shelf.rs`, `project.rs`, `translate.rs`, `reader.rs`, `lexicon.rs`) owns its own sub-state, `handle_key` (returns an `Action`), `render`, and `hints`. `Screen` enum variant **order is load-bearing** (`ui::chrome` and digit routing depend on it). The `TranslateScreen` observes *every* `AppEvent` so its live panel stays current even when off-tab.

### The pipeline (`src/agents/pipeline.rs`)

`run_pipeline` drives a per-chapter / per-chunk state machine, emitting the full `AppEvent` sequence the UI renders. Per chunk: **Translator → Reviewer**, retrying up to `cfg.max_attempts`; the reviewer's feedback is routed back into the next translator attempt.

Two design rules that look surprising but are deliberate:
- **"Everything uses tools" — except the final append.** When the reviewer approves, the Thai is appended **deterministically, app-side** (`workspace::translation::append_chunk`), *not* via an LLM tool. Only *metadata* mutation (new characters/terms/recap) goes through the Orchestrator's tool loop afterward.
- **Reference context is scoped per chunk.** `build_reference_ctx` injects only the glossary terms and characters whose JP form actually appears in the chunk text (capped at 80 / 40), re-read every chunk. This stops the injected context from ballooning with the whole accumulated roster as a volume progresses. Continuity = the previous chunk's last N Thai sentences, seeded from the previous *chapter's* tail at a chapter boundary.

Image-only chapters skip the agents entirely. `RunControl` is a cloneable `AtomicU8` (0 run / 1 pause / 2 stop) the UI toggles and the pipeline polls **between chunks** (pause/stop take effect after the current chunk finishes).

The three agents (`translator.rs`, `reviewer.rs`, plus the Orchestrator metadata turn) have independently-configurable models (`ModelSet`); prompts live in `prompts.rs`.

### LLM layer (`src/llm/`)

`LlmClient` is a `dyn`-compatible async trait; `OpenRouterClient` is the live impl and `mock.rs` (test-only) returns canned responses for the offline e2e suite. `tool_loop::run_tool_loop` drives multi-turn tool calling against any `ToolExecutor` (the pipeline's executor is `agents::tools::WorkspaceTools`). `structured::chat_structured` handles strict-JSON-schema outputs (Translator/Reviewer return typed structs). Wire-format subtleties that are easy to break: `Message.content` must serialize as JSON `null` (not skipped) on a tool-call turn, and `FunctionCall.arguments` is a JSON *string* decoded again via `parse_args`.

### Workspace & the data-block convention (`src/workspace/`)

A `Workspace` binds a project root to one active volume (`Vol_NN`) and resolves every path honya touches. Project metadata lives in human-readable Markdown files (`CHARACTERS.md`, `GLOSSARY.md`, `STYLE.md`, `PROJECT.md`, per-volume `VOLUME.md`), but the **source of truth is a `<!-- honya:data … honya:data -->` JSON block** embedded in each file (`data_block.rs`). The visible tables are *re-rendered* from that JSON on every write — never hand-parse or treat the tables as authoritative. `scan.rs` rebuilds in-memory `Project` state by reading these from disk; re-opening a project always re-scans (otherwise a stale snapshot would revert completed chapters). Writes are atomic.

Layout per project: `PROJECT.md`/`CHARACTERS.md`/`GLOSSARY.md`/`STYLE.md` + `images/` at the root, and `Vol_NN/{VOLUME.md, raw/ch_NNN.md, translated/ch_NNN.md}` per volume.

### EPUB import & cleanse (`src/epub/`, `src/cleanse.rs`)

Import reads true **spine order** from the OPF (not filename order), relocates every illustration into `images/` (dedup-safe), detects image-only pages (rendered straight to `translated/`, skipping agents), and cleanses XHTML → Markdown with fixed rules (`<ruby>` → `Base (Furigana)`, 「」/『』 → "/' quotes, `<img>`/SVG `<image>` → markdown image links). XML is parsed with `roxmltree` (namespace-aware), HTML with `scraper`.

### Text rendering for CJK + Thai (`src/ui/text.rs`)

Terminal layout is computed in **display columns, never bytes or chars** — use `col_width` / `truncate_cols` / `pad_to_cols`, never `String::len()`, for any width math. Thai text is run through `thai_display_safe` (decomposes SARA AM and related clusters) before display to stop terminal cell drift.

## Dependency pins are intentional

`Cargo.toml` carries comments explaining several deliberate version/feature choices — do **not** "upgrade" or "fix" these without reason: exactly one `crossterm` (0.29, re-exported via ratatui — only added directly for `EventStream`) and one `zip` (8.6, not 9.x prerelease) must be in the lockfile; `reqwest`'s TLS feature is `rustls` (not `rustls-tls`); `ego-tree` is a direct dep because `scraper` doesn't re-export it; `quick-xml` is intentionally omitted (roxmltree covers all XML).
