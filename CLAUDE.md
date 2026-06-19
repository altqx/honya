# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

honya (本屋) is a Ratatui terminal app for AI-assisted **Japanese → Thai** light-novel translation. It imports an EPUB, pre-processes it (spine-ordered chapters, relocated illustrations, HTML→Markdown cleanse), and runs a three-agent LLM pipeline (Orchestrator · Translator · Reviewer) over an OpenRouter-compatible API. Binary name is `honya`; Rust edition 2024.

## Commands

```sh
cargo run --release          # launch the TUI in the current working directory (= the "shelf")
cargo test                   # full suite (cleanse rules, EPUB parse, UI render smoke, mock e2e)
cargo test <name>            # single test, e.g. cargo test reference_ctx_scopes_to_chunk
cargo clippy --all-targets --locked -- -D warnings   # CI lints with warnings-as-errors; match this locally
```

Running the app (not the tests) requires an OpenRouter API key, resolved in order: `HONYA_API_KEY` → `OPENROUTER_API_KEY` env → `~/.config/honya/config.json` → interactive startup prompt. The test suite uses a mock client and needs no key.

Version is the single source of truth in `Cargo.toml`; CI auto-tags on version change, so a release is cut by bumping `version` there. A version bump also publishes the crate to crates.io (`cargo install honya`) via `.github/workflows/publish.yml` — this needs a `CRATES_IO_TOKEN` repo secret, and the publish step is idempotent (skips versions already on crates.io).

## Comments — write few, keep them short

Prefer self-explanatory code (clear names, small functions) over comments. **Default to no comment.** Don't narrate what the code already says, restate a name, or label obvious blocks. Add a comment only when the *why* is genuinely non-obvious — a subtle invariant, a non-local consequence, a workaround, or a deliberate trade-off — and when you do, keep it to a line or two. When in doubt, leave it out.

## Website + changelog live in the private `honya-relay` repo

The homepage and the `/app` remote dashboard (a TanStack Start app prerendered to
static HTML for Cloudflare Pages) used to live here under `web/`. They were **moved
to the private `altqx/honya-relay` repo** (`web/` there) so the site/dashboard
source stays private — this repo now ships only the Rust app/crate. The install
scripts served at `honya.altqx.com/install.sh` / `install.ps1` live there too
(`web/public/`); keep the release asset names below in sync with them.

When I ask you to **bump the version**:
- Bump `version` in `Cargo.toml` here (the single source of truth; CI auto-tags on
  the change, which also publishes to crates.io).
- Update the changelog in the **honya-relay** repo: prepend a new release object to
  `web/src/data/changelog.ts`, move `badge: 'latest'`, bump `VERSION` in
  `web/src/data/site.ts`. See that repo's `CLAUDE.md` for the entry format (Thai,
  `add`/`chg`/`fix` tags). Do this only on an explicit bump.

## Architecture

### Event loop & concurrency contract (the most important thing to understand)

`main.rs` runs one `tokio::select!` loop that fans in three sources: a 100ms animation ticker (`app.frame`), terminal key input via crossterm's `EventStream`, and background `AppEvent`s over an **unbounded mpsc channel**. The terminal is always restored before any error is printed.

The hard rule: **background tasks never touch `App` state directly.** Long-running work (the translation pipeline, EPUB import) is `tokio::spawn`ed and communicates back *only* by sending `AppEvent`s through a cloned `EventTx`. The UI thread folds those events into state in `App::on_app_event`. `model.rs` defines `AppEvent` — it is the shared vocabulary between workers and the UI; adding a worker→UI signal means adding a variant there and handling it in `on_app_event`.

### App state machine (`src/app/`)

`App` owns everything: the five screens (`Shelf`/`Project`/`Translate`/`Reader`/`Lexicon`), the current `Overlay`, config, theme, and the active project. Key handling is a strict three-step funnel:

1. `App::on_key` → `route_key` decides what a key *means* given overlay/screen/capture state and returns an **`Action`** (it generally must not mutate state itself). Overlays get first refusal; a focused text field (`screen_is_capturing`) swallows single-letter globals; then global keys (`1`-`5`, Tab, `?`, `:`, `` ` ``, `q`) and toast dismissal (`Esc`/`Backspace`); then the active screen. `l` opens the activity log except on the Project tab, where it is the screen-local expand/focus key.
2. `apply(action)` is the **single mutation funnel** — every state change and every spawn of background work goes through here.

Each screen module (`shelf.rs`, `project.rs`, `translate.rs`, `reader.rs`, `lexicon.rs`) owns its own sub-state, `handle_key` (returns an `Action`), `render`, and `hints`. `Screen` enum variant **order is load-bearing** (`ui::chrome` and digit routing depend on it). The `TranslateScreen` observes *every* `AppEvent` so its live panel stays current even when off-tab; its queue panel is only a mirror of `App.run_queue`.

### The pipeline (`src/agents/pipeline.rs`)

`run_pipeline` drives a per-chapter / per-chunk state machine, emitting the full `AppEvent` sequence the UI renders. Per chunk: **Translator → Reviewer**, retrying up to `cfg.max_attempts`; the reviewer's feedback is routed back into the next translator attempt.

Two design rules that look surprising but are deliberate:
- **"Everything uses tools" — except the final append.** When the reviewer approves, the Thai is appended **deterministically, app-side** (`workspace::translation::append_chunk`), *not* via an LLM tool. Only *metadata* mutation (new characters/terms/recap) goes through the Orchestrator's tool loop afterward.
- **Reference context is scoped per chunk.** `build_reference_ctx` injects only the glossary terms and characters whose JP form actually appears in the chunk text (capped at 80 / 40), re-read every chunk. This stops the injected context from ballooning with the whole accumulated roster as a volume progresses. Continuity = the previous chunk's last N Thai sentences, seeded from the previous *chapter's* tail at a chapter boundary.

Image-only chapters skip the agents entirely. `RunControl` is a cloneable `AtomicU8` (0 run / 1 pause / 2 stop) the UI toggles and the pipeline polls **between chunks** (pause/stop take effect after the current chunk finishes).

The live run queue is `ChapterQueue`, shared between the UI and pipeline like `RunControl`. It stores `(vol, chapter)` identities because chapter numbers repeat across volumes. The active chapter lives in a separate `running` slot, so UI mutations only touch pending items: enqueue, move up/down, sort, and remove. A single-volume run drains one workspace and rejects cross-volume enqueues; a whole-project run drains by volume and then sweeps any live-added volumes the original plan did not cover. Whenever the UI adds/removes chapters, `App` resyncs the recovery checkpoint so crash resume follows the live queue.

The three agents (`translator.rs`, `reviewer.rs`, plus the Orchestrator metadata turn) — and the Refine agent — each pick their own **provider + model + reasoning effort** (`ModelSet` of `AgentModel { provider, model, effort }`); prompts live in `prompts.rs`. `AgentModel` deserializes a bare model-id string from legacy configs (→ OpenRouter, no effort). The effort, when set, is sent as the request's `reasoning: {"effort": …}` param.

### LLM layer (`src/llm/`)

`LlmClient` is a `dyn`-compatible async trait; `OpenRouterClient` is the live impl and `mock.rs` (test-only) returns canned responses for the offline e2e suite. **OpenRouter and Tokenrouter are the same OpenAI-compatible `/chat/completions` wire format** — they share `OpenRouterClient`, differing only in base URL (`ClientConfig::for_endpoint`) + key (Tokenrouter key resolved from `HONYA_TOKENROUTER_API_KEY`/`TOKENROUTER_API_KEY`/config). `ClientSet` holds the per-provider clients built once per run; an agent routes to its provider via `ClientSet::for_agent` (the pipeline resolves it per call, failing fast with a clear message if that provider has no key). **Codex** (`Provider::Codex`) signs in with ChatGPT (PKCE OAuth in `src/codex/`, auto-importing `~/.codex/auth.json`) and talks to the ChatGPT-backend **Responses API** via `llm::codex::CodexClient` — which translates honya's chat/completions-shaped `ChatRequest` into Responses (`instructions` + typed `input` items + flat tools + `text.format` + `reasoning.effort`) and folds the `response.*` SSE stream back into a `ChatResponse`. `tool_loop::run_tool_loop` drives multi-turn tool calling against any `ToolExecutor` (the pipeline's executor is `agents::tools::WorkspaceTools`). `structured::chat_structured` handles strict-JSON-schema outputs (Translator/Reviewer return typed structs). Wire-format subtleties that are easy to break: `Message.content` must serialize as JSON `null` (not skipped) on a tool-call turn, and `FunctionCall.arguments` is a JSON *string* decoded again via `parse_args`.

### Remote control & GitHub accounts (`src/remote/`)

Optional feature: sign in with GitHub (OAuth **Device Flow** — no browser redirect in the terminal) to link this app instance to an account on the Cloudflare relay backend (the **separate private `honya-relay` repo** — a Worker + Durable Object + D1, not in this tree), then live-monitor and control a translation session from the web dashboard (the `web/` `/app` route, which also lives in the `honya-relay` repo). Two `tokio::spawn`'d background tasks, both modeled on `update.rs` (own a short-lived client, never touch `App`, report only via `EventTx`):

- `auth.rs` — device-flow sign-in → `POST {RELAY_BASE}/device/register` → a long-lived `device_token` persisted in `AppConfig.account` (a secret, hence config.json's 0600 mode matters).
- `relay.rs` — persistent `wss://…/relay` link: pushes serialized state OUTbound, receives commands INbound. Auto-reconnects with capped backoff; disabled by dropping the outbound sender + flipping a shared `Arc<AtomicBool>` (same shape as `RunControl`).

The contract stays intact two ways: (1) **outbound** — `App.on_app_event` folds each event into state as usual, then `project_and_send_remote` pushes a *serializable projection* (`protocol::RemoteSnapshot`/`RemoteDelta`) down an `Option<UnboundedSender<RemoteOutbound>>` on `App`; the relay task only serializes and ships it. (2) **inbound** — a browser command arrives as `AppEvent::RemoteCommand`, and `map_remote_command` turns it into an **existing** `Action` (`PauseRun`, `EnqueueChapters`, …) routed through the same `apply()` funnel as a keystroke — so a remote command adds zero new mutation logic. `protocol.rs` is the pure-serde wire contract shared with the `honya-relay` backend and its `web/` dashboard; keep all three in lockstep (the source-of-truth `PROTOCOL.md` lives in the `honya-relay` repo). The Settings overlay grows an "Account / Remote" section (Ctrl-A sign in · Ctrl-R toggle · Ctrl-O sign out); the header shows a `⇄` glyph + watcher count when connected. `GITHUB_CLIENT_ID`/`RELAY_BASE` are baked at build time via `option_env!` (like `HONYA_BUILD_COMMIT`).

### Workspace & the data-block convention (`src/workspace/`)

A `Workspace` binds a project root to one active volume (`Vol_NN`) and resolves every path honya touches. Project metadata lives in human-readable Markdown files (`CHARACTERS.md`, `GLOSSARY.md`, `STYLE.md`, `PROJECT.md`, per-volume `VOLUME.md`), but the **source of truth is a `<!-- honya:data … honya:data -->` JSON block** embedded in each file (`data_block.rs`). The visible tables are *re-rendered* from that JSON on every write — never hand-parse or treat the tables as authoritative. `scan.rs` rebuilds in-memory `Project` state by reading these from disk; re-opening a project always re-scans (otherwise a stale snapshot would revert completed chapters). Writes are atomic.

Layout per project: `PROJECT.md`/`CHARACTERS.md`/`GLOSSARY.md`/`STYLE.md` + `images/` at the root, and `Vol_NN/{VOLUME.md, raw/ch_NNN.md, translated/ch_NNN.md}` per volume.

### EPUB import & cleanse (`src/epub/`, `src/cleanse.rs`)

Import reads true **spine order** from the OPF (not filename order), relocates every illustration into `images/` (dedup-safe), detects image-only pages (rendered straight to `translated/`, skipping agents), and cleanses XHTML → Markdown with fixed rules (`<ruby>` → `Base (Furigana)`, 「」/『』 → "/' quotes, `<img>`/SVG `<image>` → markdown image links). XML is parsed with `roxmltree` (namespace-aware), HTML with `scraper`.

### Text rendering for CJK + Thai (`src/ui/text.rs`)

Terminal layout is computed in **display columns, never bytes or chars** — use `col_width` / `truncate_cols` / `pad_to_cols`, never `String::len()`, for any width math. Thai text is run through `thai_display_safe` (decomposes SARA AM and related clusters) before display to stop terminal cell drift.

## Dependency pins are intentional

`Cargo.toml` carries comments explaining several deliberate version/feature choices — do **not** "upgrade" or "fix" these without reason: exactly one `crossterm` (0.29, re-exported via ratatui — only added directly for `EventStream`) and one `zip` (8.6, not 9.x prerelease) must be in the lockfile; `reqwest`'s TLS feature is `rustls` (not `rustls-tls`); `tokio-tungstenite` (WebSocket client for `src/remote`) uses `rustls-tls-webpki-roots` to share reqwest's rustls + bundled CA roots — **not** `native-tls` or `rustls-tls-native-roots` (a single `rustls` ends up in the tree; only the `webpki-roots` *data* crate has a benign duplicate); `ego-tree` is a direct dep because `scraper` doesn't re-export it; `quick-xml` is intentionally omitted (roxmltree covers all XML).


<!-- headroom:rtk-instructions -->
# RTK (Rust Token Killer) - Token-Optimized Commands

When running shell commands, **always prefix with `rtk`**. This reduces context
usage by 60-90% with zero behavior change. If rtk has no filter for a command,
it passes through unchanged — so it is always safe to use.

## Key Commands
```bash
# Git (59-80% savings)
rtk git status          rtk git diff            rtk git log

# Files & Search (60-75% savings)
rtk ls <path>           rtk read <file>         rtk grep <pattern>
rtk find <pattern>      rtk diff <file>

# Test (90-99% savings) — shows failures only
rtk pytest tests/       rtk cargo test          rtk test <cmd>

# Build & Lint (80-90% savings) — shows errors only
rtk tsc                 rtk lint                rtk cargo build
rtk prettier --check    rtk mypy                rtk ruff check

# Analysis (70-90% savings)
rtk err <cmd>           rtk log <file>          rtk json <file>
rtk summary <cmd>       rtk deps                rtk env

# GitHub (26-87% savings)
rtk gh pr view <n>      rtk gh run list         rtk gh issue list

# Infrastructure (85% savings)
rtk docker ps           rtk kubectl get         rtk docker logs <c>

# Package managers (70-90% savings)
rtk pip list            rtk pnpm install        rtk npm run <script>
```

## Rules
- In command chains, prefix each segment: `rtk git add . && rtk git commit -m "msg"`
- For debugging, use raw command without rtk prefix
- `rtk proxy <cmd>` runs command without filtering but tracks usage
<!-- /headroom:rtk-instructions -->
