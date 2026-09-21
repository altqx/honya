# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

honya (本屋) is a Ratatui terminal app for AI-assisted **Japanese → Thai or English** light-novel translation. It imports an EPUB, pre-processes it (spine-ordered chapters, relocated illustrations, HTML→Markdown cleanse), and runs a three-agent LLM pipeline (Orchestrator · Translator · Reviewer) over an OpenRouter-compatible API. Binary name is `honya`; Rust edition 2024.

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

1. `App::on_key` → `route_key` decides what a key *means* given overlay/screen/capture state and returns an **`Action`** (it decides; it does not mutate `App` state itself). Overlays get first refusal; a focused text field (`screen_is_capturing`) swallows single-letter globals; then global keys (`1`-`5`, Tab, `?`, `:`, `` ` ``, `q`) and toast dismissal (`Esc`/`Backspace`); then the active screen. `l` opens the activity log except on the Project tab, where it is the screen-local expand/focus key.
2. `apply(action)` is the **single mutation funnel** — every `App`-level state change and every spawn of background work goes through here, from both front ends. `src/gui/` holds no direct `App` field writes: `RescanShelf` and `DismissToast` exist because the GUI (and the key router) used to do those by hand, and a disk scan writing `App.projects` from inside a paint was invisible to `project_and_send_remote`, which only ever sees what `apply` folds. A *screen's own* sub-state is still the screen's to mutate from its handler — that is the one thing this funnel deliberately does not cover.

Each screen module (`shelf.rs`, `project.rs`, `translate.rs`, `reader.rs`, `lexicon.rs`) owns its own sub-state, `handle_key` (returns an `Action`), `render`, and `hints`.

**Forms are generated from declarations, not written twice.** `settings_defs.rs` and `lexicon_defs.rs` each declare their rows once — order, label, help, group, `Kind` — and both front ends render from that declaration: the TUI through `ui::kit::form`, the GUI through `gui/settings.rs::declared_rows` and `gui/lexicon_form.rs`. **Declaration order is the on-screen order.** Values are read through `SettingsState::settings_{toggle,select_label,secret,text,disabled}` and written through `cycle_field` / `text_field_mut_of` / `set_select`, all keyed by `SField`, so neither front end holds a second copy of what a row means. `cycle_field` is the single write path for every choice row and `select_domain` sits beside it, because the two have to agree about what follows what; `set_select` walks the cycle rather than assigning, so there is no second place that knows a domain's order. The GUI previously hand-placed 33 controls and kept its own const arrays of providers, efforts, tiers and gate modes — which is how the Pipeline tab came to silently omit six settings `save_action` still wrote. `Screen` enum variant **order is load-bearing** (`ui::chrome` and digit routing depend on it). The `TranslateScreen` observes *every* `AppEvent` so its live panel stays current even when off-tab; its queue panel is only a mirror of `App.run_queue`.

### The pipeline (`src/agents/pipeline.rs`)

`run_pipeline` drives a per-chapter / per-chunk state machine, emitting the full `AppEvent` sequence the UI renders. Per chunk: **Translator → Reviewer**, retrying up to `cfg.max_attempts`; the reviewer's feedback is routed back into the next translator attempt.

Optionally a **System One review gate** (`agents/review_gate.rs`) runs *before* the Reviewer — see "Review gate" below.

Two design rules that look surprising but are deliberate:
- **"Everything uses tools" — except the final append.** When the reviewer approves, the target-language text is appended **deterministically, app-side** (`workspace::translation::append_chunk`), *not* via an LLM tool. Only *metadata* mutation (new characters/terms/recap) goes through the Orchestrator's tool loop afterward.
- **Reference context is scoped per chunk.** (`agents/reference_scope.rs` closes the two gaps the string test leaves — a character carried only by pronoun/title is invisible to `contains`, and an overflowing list is trimmed by roster order rather than relevance. Code still does the recall pass; the judgement only adds what was missed and orders what overflowed. It is the one judgement on the critical path, *before* the Translator call, so it is asked only when the answer could change the bundle: nothing absent and nothing overflowing means no call.)
- **Reference context is scoped per chunk.** `build_reference_ctx` injects only the glossary terms and characters whose JP form actually appears in the chunk text (capped at 80 / 40), re-read every chunk. This stops the injected context from ballooning with the whole accumulated roster as a volume progresses. Continuity = the previous chunk's last N translated sentences, seeded from the previous *chapter's* tail at a chapter boundary.

**System One judgements (`AppConfig.system_one`, off by default).** `SystemOne` is one master `enabled` switch, one transport (provider + model + `min_confidence`), the tri-state `review_gate` mode, and a `bool` per judgement (`SystemOneFeature::ALL`). `SystemOne::feature(f)` is the only way to ask whether a judgement runs — it ANDs the per-feature toggle with the master switch, so flipping `enabled` off restores the deterministic path everywhere at once. Configs written before this carried the gate under a `review_gate` key; that shape still deserializes (via the `alias` plus `SystemOneWire`'s legacy `mode` field) and deliberately leaves every judgement added since **off**, so an upgrade never starts making calls the user did not ask for.

**Spine classification (`epub/judge.rs`).** `segment::classify` decides what a spine page is with per-publisher constants — a `toc` substring in the body class, "8+ internal links and <40 prose chars each", and a hardcoded list of Japanese chrome labels — which keep needing another entry for the next publisher. `classify_spine` asks instead, one Choice per document over a tagged table of the whole spine (TypeSafe's structure-recovery recipe: code keeps the direct evidence, the model gets only the ambiguous call). The answers arrive as `Option<DocRole>` per document and feed `segment_with_roles`; a role **never** overrides a mechanical class (`is_image_only`, `Empty`), and a missing or unconfident answer falls back per document, so a partial answer still helps. Batched at 32 questions per request over one shared state, capped at 120 docs, and it runs once per book at import — not per chunk.

**Character alignment (`agents/entity_align.rs`).** `characters::upsert` decides "same person" by comparing name surfaces, which cannot connect 高橋陽菜, ハル and 先輩. `Alignment { same_as, maybe, ruled_out }` is a caller-supplied verdict folded into the *uncertain* tiers only — an exact id or an exact written name still merges on its own. The asymmetry sets the policy (a wrong merge corrupts every linked fact; a missed one only leaves a duplicate): a merge needs `already_on_roster ≥ 0.75` **and** Choice confidence over `min_confidence`, an unconfident match becomes `maybe` (surfaced as the existing `InsertedWithCandidates`, which already tells the model to call `merge_character`), and a confident "new person" only ever *withholds* a weak suffix merge. The question pair is the semantic-find shape: a Choice whose options are the roster ids, plus a Noul for whether any applies. Wired into the Orchestrator tool (`agents/tools.rs`, via `Aligner`) and the prepass seeding loop.

**The audit's two tiers (`agents/audit.rs`).** Most audit checks settle from the text itself — counts, markers, scripts, glossary locks — and live in `audit_translation_mechanical` / `advisory_findings_mechanical`. Three are judgements about meaning, not properties of the string: whether a parenthetical is a gloss, whether `กู` is the pronoun, whether `วะ` is a final particle. Those are found by `semantic_candidates`, which deliberately **over-finds** (per TypeSafe's pre-parsed value extraction recipe: the judgement can reject a candidate but can never see one the scan omitted), and each candidate carries the hand-tuned predicate's verdict as `heuristic`. A fourth, the continuity echo, is whole-chunk rather than a span but rides the same machinery. `agents::audit_judge` decides them all with one Noul per candidate in a single request (the state is what costs, so the checks share it while each answers to its own toggle — `audit` or `continuity`); anything unusable — feature off, no key, oversized state, backend error, missing answer, wrong primitive, a probability between 0.25 and 0.75 — falls back to that candidate's `heuristic`, so a judgement can only sharpen a finding, never invent or lose one. A chunk with no candidates costs no call at all, which is the common case. The pre-split entry points survive `#[cfg(test)]` so a test can pin that mechanical + heuristic still equals the original audit.

**The review gate (`agents/review_gate.rs`, off by default).** TypeSafe's Jev answers typed questions (Choice/Score/Noul + calibrated probabilities) instead of generating text, so it can produce a verdict but never the `feedback: Vec<String>` the translator retries on. It is therefore **not** a `Provider` — it is a separate `AppConfig.review_gate` axis, and `ModelSet.reviewer` stays a text model (the coherence sweep reuses that slot). `try_review` returns an `Option<GateOutcome>` carrying a normal `ReviewerOut`, so `pipeline.rs` folds it in through the existing path and nothing downstream changes shape. **`None` means defer to the LLM reviewer, and every uncertainty resolves that way** — mode off, no key, a chunk the deterministic audit already rejected, an oversized state, a backend error, or an unparseable answer. The gate can only save a reviewer call, never block or fail a chunk. In `standalone` mode it rejects with feedback synthesized from the failing axes. It emits no new `AppEvent` variant on purpose (reusing `ReviewerRequested`/`ReviewerReturned` + a `Log` line), which keeps `remote/protocol.rs` and the relay repo out of the change.

Image-only chapters skip the agents entirely. `RunControl` is a cloneable `AtomicU8` (0 run / 1 pause / 2 stop) the UI toggles and the pipeline polls **between chunks** (pause/stop take effect after the current chunk finishes).

The live run queue is `ChapterQueue`, shared between the UI and pipeline like `RunControl`. It stores `(vol, chapter)` identities because chapter numbers repeat across volumes. The active chapter lives in a separate `running` slot, so UI mutations only touch pending items: enqueue, move up/down, sort, and remove. A single-volume run drains one workspace and rejects cross-volume enqueues; a whole-project run drains by volume and then sweeps any live-added volumes the original plan did not cover. Whenever the UI adds/removes chapters, `App` resyncs the recovery checkpoint so crash resume follows the live queue.

The three agents (`translator.rs`, `reviewer.rs`, plus the Orchestrator metadata turn) — and the Refine agent — each pick their own **provider + model + reasoning effort** (`ModelSet` of `AgentModel { provider, model, effort }`); prompts live in `prompts.rs`. `AgentModel` deserializes a bare model-id string from legacy configs (→ OpenRouter, no effort). The effort, when set, is sent as the request's `reasoning: {"effort": …}` param.

Language is project-owned: `PROJECT.md` persists `target_language`, and every translation, Refine, editor, resume, and export path reads it from the scanned active project. `AppConfig.preferred_language` only seeds the first language choice in the new-project wizard. Older configs accept the legacy `target_language` key as that preference; older projects with no language field default to Thai.

### LLM layer (`src/llm/`)

`LlmClient` is a `dyn`-compatible async trait; `OpenRouterClient` is the live impl and `mock.rs` (test-only) returns canned responses for the offline e2e suite. **OpenRouter and Tokenrouter are the same OpenAI-compatible `/chat/completions` wire format** — they share `OpenRouterClient`, differing only in base URL (`ClientConfig::for_endpoint`) + key (Tokenrouter key resolved from `HONYA_TOKENROUTER_API_KEY`/`TOKENROUTER_API_KEY`/config). `ClientSet` holds the per-provider clients built once per run; an agent routes to its provider via `ClientSet::for_agent` (the pipeline resolves it per call, failing fast with a clear message if that provider has no key). **Codex** (`Provider::Codex`) signs in with ChatGPT (PKCE OAuth in `src/codex/`, auto-importing `~/.codex/auth.json`) and talks to the ChatGPT-backend **Responses API** via `llm::codex::CodexClient` — which translates honya's chat/completions-shaped `ChatRequest` into Responses (`instructions` + typed `input` items + flat tools + `text.format` + `reasoning.effort`) and folds the `response.*` SSE stream back into a `ChatResponse`. **`llm::decisions`** is a separate transport for TypeSafe **System One** (Jev) and deliberately does *not* implement `LlmClient` — a decisions model returns typed answers, never text, so it can never stand in for an agent. One `DecisionsClient` serves both routes (they share a body shape, differing only in URL/key/model id): OpenRouter `POST https://openrouter.ai/api/alpha/decisions` with `typesafe/jev-1.13`, reusing the OpenRouter key — note the path is under `/api/`, **not** `/api/v1/`, so it cannot be derived from `OPENROUTER_BASE_URL` — or TypeSafe `POST https://api.typesafe.ai/v1/systemone` with `jev-latest` and its own key. It hangs off `ClientSet` as a `decisions` slot (excluded from `is_empty()`, and the TypeSafe key is excluded from `config::any_provider_key` — neither makes the app usable on its own). Wire detail: a **Noul answer carries no `confidence` field**, unlike Choice and Score, so `Answer::confidence()` substitutes its distance from a coin flip.

**`SystemOneHandle::ask` is the one seam every judgement goes through.** It takes a `Switch` (a per-feature toggle, the review gate's tri-state mode, or `PerQuestion` when the caller has already gated each question), the state and the questions, and returns `Option<Judgement>` — answers plus `usage`. **`None` is the single "use the deterministic path" answer** and covers every reason there is to: the judgement is switched off, there is nothing to ask, the state exceeds `MAX_STATE_CHARS`, or the backend failed. Each of the five judgements (`review_gate`, `audit_judge`, `entity_align`, `reference_scope`, `epub::judge`) used to spell that ladder out for itself, down to its own copy of the 24k character budget, under two different calling conventions. They now all take `Option<&SystemOneHandle>` and keep only what is theirs: the questions, and what to do with a missing or unconfident answer. A judgement can still only ever cost a call, never change a verdict. `state_fits` is public because a judgement that shrinks its own state (the spine classifier trims excerpts) needs the budget it is trimming towards.

`tool_loop::run_tool_loop` drives multi-turn tool calling against any `ToolExecutor` (the pipeline's executor is `agents::tools::WorkspaceTools`). `structured::chat_structured` handles strict-JSON-schema outputs (Translator/Reviewer return typed structs). Wire-format subtleties that are easy to break: `Message.content` must serialize as JSON `null` (not skipped) on a tool-call turn, and `FunctionCall.arguments` is a JSON *string* decoded again via `parse_args`.

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



## Cursor Cloud specific instructions

Single Rust (edition 2024) TUI crate — no companion services, DB, or Docker. Standard build/lint/test/run commands live in this file's **Commands** section and the README's **Development** section; use those.

- **Toolchain:** edition 2024 needs `rustc` ≥ 1.85. The base image may ship an older default (seen: 1.83), so the startup update script bumps the `stable` toolchain and adds `clippy`. If a build fails with an "edition 2024 is unstable"/edition error, run `rustup update stable && rustup default stable`.
- **Tests need no API key or network** — the suite uses the in-tree mock LLM client (`src/llm/mock.rs`); `cargo test --locked` runs fully offline.
- **Running the app needs a real TTY.** In a headless agent, launch it inside a PTY (a `tmux` session or a desktop terminal), not as a plain piped process. Set `HONYA_NO_UPDATE_CHECK=1` to skip the startup network update check.
- **No API key is required just to launch or to import.** With no key, honya shows a Welcome/sample-project offline path; EPUB/PDF/HTML/Markdown import + cleanse is pure Rust pre-processing (no LLM), so importing a source file into a new project is a good offline smoke test. A provider key (`HONYA_API_KEY`/`OPENROUTER_API_KEY`, etc.) is only needed to actually translate.
- **The current working directory is the "shelf."** Run `honya` from a folder that holds your projects and loose source files, not from the repo root.
