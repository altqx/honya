<div align="center">

# honya 本屋

**A calm, literary terminal app for AI-assisted Japanese → Thai or English light-novel translation.**

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-3A5078.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/altqx/honya?color=6A8258&label=release)](https://github.com/altqx/honya/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/altqx/honya/ci.yml?branch=main&label=ci&color=6A8258)](https://github.com/altqx/honya/actions/workflows/ci.yml)

[Installation](#installation) · [Quick start](#quick-start) · [The five screens](#the-five-screens)

</div>

---

Drop an EPUB, PDF, DOCX, HTML, or text-based file into a folder and honya turns it into a finished Thai or English translation. Settings holds your preferred language; the new-project wizard starts there, and stores the chosen target language on that project. Thai remains the default. EPUBs keep true spine order, while other formats are converted through MarkItDown-style Rust converters into tidy Markdown before the same pipeline runs.

For EPUBs, honya reads the book
in true spine order, relocates the illustrations, cleanses the XHTML into tidy Markdown, then runs
a three-agent LLM pipeline — **Orchestrator · Translator · Reviewer** — over an OpenAI-compatible
provider. The agents keep your character roster, glossary, and volume notes current **through tool calls**,
and every token and dollar is accounted for as the run progresses.

It's a single static binary, built on [Ratatui](https://ratatui.rs). No Electron, no browser, no
telemetry — just a quiet workspace in your terminal.

> [!IMPORTANT]
> honya needs a provider key. On first launch it can prompt for an OpenRouter key, or you can
> configure another provider in Settings.

## Highlights

- **Many sources in, Thai or English out** — EPUB, PDF, Word, HTML, Markdown/Text, CSV, JSON, and XML; EPUBs keep spine order, illustration relocation, and light-novel cleanse rules.
- **Three specialized agents** — Translator and Reviewer iterate per chunk; the Orchestrator persists new terms and characters.
- **One-press runs with a live queue** — translate a chapter, a volume, or the whole project (`A`) across multiple volumes, reordering the queue mid-run.
- **Resilient by default** — crash/partial-chapter resume, a stall watchdog that retries stuck chunks, and `NeedsReview` flags instead of failed chapters.
- **Continuity & cost** — per-chunk glossary/character context, protected term locks, honorific rules, and a live token + USD meter rolled up per chapter/volume/project.
- **Proofread & export** — synced source ↔ translation reader with notes and search, then export finished volumes to Markdown, EPUB, or DOCX.
- **Yours to keep** — Glossary/Characters/Style as editable Markdown, 12 live themes (`Ctrl-T`), mouse + full keyboard, and self-update with stable/dev channels.

## Installation

### Quick install (Linux · macOS)

```sh
curl -fsSL https://honya.altqx.com/install.sh | bash
```

The installer downloads the latest prebuilt binary for your platform, **verifies its SHA-256
checksum**, and installs it to `~/.local/bin`. If that directory isn't on your `PATH`, the script
prints the exact line to add for bash / zsh / fish.

Prebuilt binaries are published for:

| OS | Architectures |
|----|---------------|
| **Linux** (gnu) | `x86_64` · `aarch64` |
| **macOS** | `x86_64` (Intel) · `aarch64` (Apple Silicon) |
| **Windows** (msvc) | `x86_64` · `aarch64` |

<details>
<summary>Installer options</summary>

```sh
# install to a custom directory
curl -fsSL https://honya.altqx.com/install.sh | bash -s -- --dir ~/bin

# pin a specific release
curl -fsSL https://honya.altqx.com/install.sh | bash -s -- --version v0.1.0

# force a source build via cargo
curl -fsSL https://honya.altqx.com/install.sh | bash -s -- --source
```

| Flag | Env var | Default | Meaning |
|------|---------|---------|---------|
| `--dir <path>` | `HONYA_INSTALL_DIR` | `$HOME/.local/bin` | Install directory |
| `--version <tag>` | `HONYA_VERSION` | latest | Release tag to install |
| `--source` | — | off | Build from source via `cargo install` |
| — | `NO_COLOR` | — | Disable colored output |

On a platform without a prebuilt asset, the installer automatically falls back to a `cargo` source build.

</details>

### Windows (PowerShell)

```powershell
irm https://honya.altqx.com/install.ps1 | iex
```

Downloads the prebuilt `honya.exe` for your architecture, **verifies its SHA-256**, installs it to
`%LOCALAPPDATA%\Programs\honya`, and adds that directory to your user `PATH`. To pin a version or
change the directory, pass flags through the one-liner:

```powershell
iex "& { $(irm https://honya.altqx.com/install.ps1) } -Version v0.1.0 -Dir C:\tools\honya"
```

`$env:HONYA_VERSION` and `$env:HONYA_INSTALL_DIR` work too. On a platform without a prebuilt asset the
script falls back to `cargo install honya` (run with `-Source` to force it). Windows Terminal — or
Windows 10 1809+ — is recommended for full color/Unicode rendering.

### With Cargo

```sh
cargo install honya
```

### From source

```sh
git clone https://github.com/altqx/honya
cd honya
cargo run --release        # or: cargo install --path . --locked
```

Requires a recent stable Rust toolchain ([rustup.rs](https://rustup.rs)).

## Quick start

```sh
honya          # launch the TUI in the current directory
```

honya treats the **current working directory** as your *shelf*: each translation project is a
subdirectory, and any loose supported source files (`*.epub`, `*.pdf`, `*.docx`, `*.html`, `*.txt`, `*.md`, `*.csv`, `*.json`, `*.xml`, …) are offered as one-press imports. From the **書架 Shelf**
tab, press `i` to import a file into a new project, then `t` / `T` on the **棚 Project** tab to
translate a chapter or a whole volume.

On first run, honya can prompt for an OpenRouter key and save it; other providers are configured in
Settings — see [API key & models](#api-key--models).

## The five screens

| Tab | | Purpose |
|----|----|---------|
| `1` | **書架 Shelf** | Pick a project or import a new source file (`i`). |
| `2` | **棚 Project** | Volume/chapter tree with waxing-moon status (`○ ◐ ◑ ●`), context files, and a detail card with per-chapter token/cost roll-up. `t` chapter · `T` / `Shift-T` volume · `A` whole project · `i` queue chapters. |
| `3` | **訳 Translate** | The live run: chunk gauge, three agent lines, token + USD meter, streaming translation preview. `p` pause · `s` stop · `f` follow. |
| `4` | **読 Reader** | Synced side-by-side source ↔ translation proofreading with inline notes. `[ ]` chapters · `z` sync · `o` layout · `n` note. |
| `5` | **辞 Lexicon** | Browse/edit Glossary, Characters, Style. `n` new · `e` edit · `d` delete · `/` search. |

In **Lexicon → Glossary**, set `Protected` to `yes` to lock a human-approved term. Protected terms are shown in GLOSSARY.md and surfaced to the Orchestrator when term discoveries are processed; automatic upserts cannot overwrite them.

**Global keys** (always available — `?` lists the full table):
`?` help · `:` command palette · `Ctrl-T` theme · `l` activity log · `1`–`5` / `Tab` switch tabs ·
`Esc` close overlay · `Esc` / `Backspace` dismiss notification · `q` quit.
On the **Project** tab, `l` expands/focuses the tree; use backtick (`` ` ``) for the activity log there instead.

## API key & models

honya supports OpenRouter, Tokenrouter, Google Gemini, Cloudflare Workers AI, and Codex. For
OpenRouter, key resolution order is:

1. `HONYA_API_KEY`, then `OPENROUTER_API_KEY` from the environment (these always win).
2. Otherwise, the key saved at `~/.config/honya/config.json` (or `$XDG_CONFIG_HOME/honya/config.json`).

If neither is present, honya prompts for the key at startup (hidden input) and writes it to that
config file (`0600` on Unix) so subsequent launches don't ask again.

Cloudflare Workers AI uses Cloudflare's OpenAI-compatible endpoint and requires both an account id
and API token. Use a Workers AI model id such as `@cf/meta/llama-3.1-8b-instruct`.

The three agents have **independently configurable providers and models** (a `ModelSet`, overridable per project);
the defaults are:

| Agent | Default model |
|-------|---------------|
| **Orchestrator** | `google/gemini-3.5-flash` |
| **Translator** | `google/gemini-3-flash-preview` |
| **Reviewer** | `google/gemini-3.1-flash-lite` |

Open **Settings** (`:` → *Settings*) to pick each agent's provider/model, add provider credentials,
set the **retry attempts** per chunk (1–20), pick an OpenRouter **service tier** (Off / Flex /
Priority, `Ctrl-Y`), and choose a **release channel** (stable / dev, `Ctrl-G`).

<details>
<summary>All environment variables</summary>

| Variable | Effect |
|----------|--------|
| `HONYA_API_KEY` | OpenRouter API key (checked first). |
| `OPENROUTER_API_KEY` | OpenRouter API key (fallback). |
| `HONYA_TOKENROUTER_API_KEY` / `TOKENROUTER_API_KEY` | Tokenrouter API key. |
| `HONYA_GOOGLE_API_KEY` / `GEMINI_API_KEY` / `GOOGLE_API_KEY` | Google Gemini API key. |
| `HONYA_CLOUDFLARE_ACCOUNT_ID` / `CLOUDFLARE_ACCOUNT_ID` / `CF_ACCOUNT_ID` | Cloudflare account id for Workers AI. |
| `HONYA_CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_API_KEY` / `CF_API_TOKEN` | Cloudflare Workers AI API token. |
| `XDG_CONFIG_HOME` | Override the config directory root (`$XDG_CONFIG_HOME/honya`). |
| `HONYA_NO_UPDATE_CHECK` | Set to any value to skip the startup update check. |
| `HONYA_SESSION_FILE` | Override the crash-recovery checkpoint path (absolute). |

</details>

## Themes

honya ships a curated palette set and a **live-preview** picker — open it with `Ctrl-T` (or `:` →
*Theme*). Arrow / `j` `k` through the list and the **whole UI recolors as you move**; `Enter` applies
and saves, `Esc` reverts. Your choice persists to `config.json`, so honya reopens in the theme you picked.

| | Themes |
|----|--------|
| **Light** | Washi 和紙 (paper + sumi ink, the default) · Solarized Light |
| **Native dark** | Sumi 墨 (warm ink, indigo 藍 accent) |
| **Adaptive** | Terminal — uses your terminal's own ANSI colors, so honya matches whatever scheme it's already set to |
| **Popular schemes** | Gruvbox · Nord · Tokyo Night · Dracula · Catppuccin Mocha · Solarized Dark · Everforest · Rosé Pine |

Every palette honors one semantic contract: a single accent for focus/nav, green = done, amber =
caution, and red reserved **only** for failure — so status always reads the same way across themes.

## How it works

### Pre-processing (Rust, at import)

- **MarkItDown-style file conversion** for PDF, Word (`.docx`), HTML, Markdown/Text, CSV, JSON, and XML; obvious `#` sections become raw chapters.
- **Spine-ordered EPUB chapters** from `content.opf` — true reading order, not filename order.
- **Media relocation** — every PNG/JPG/SVG illustration is copied into `images/` (dedup-safe).
- **Image-only detection** — illustration pages render their image link straight to `translated/`, skipping the agents entirely.
- **Image segmentation** — stray illustration pages fold into the surrounding chapter, and `m###`-style title plates are detected as chapter heads.
- **Cleanse rules** (exact): `<br>` → `---` (thematic break; stacked ones collapse to a single divider),
  bold/italic spans → `**` / `*`, 「…」 → "…", 『…』 → '…', `<ruby>` → `Base (Furigana)`,
  `<img>` / SVG `<image>` → `![ภาพประกอบ](../../images/file.png)`.

### The pipeline

Per chapter: chunk to ~1000 tokens (1200 hard cap) → for each chunk, inject the previous chunk's
last 5 translated sentences for continuity and the **per-chunk** reference context (only the glossary terms
and characters whose Japanese form actually appears in this chunk, capped at 80 / 40), then run
**Translator → Reviewer**.

- On *reject*, the reviewer's itemized feedback is routed back for a retry (up to a configurable cap, default 3).
- On *approve*, the target-language text is appended **deterministically, app-side** — not via an LLM tool — and the
  **Orchestrator** runs a tool turn to persist any new characters/terms/notes and advance the recap, while respecting protected glossary locks.
- If retries are exhausted, the best attempt is committed with a `[REVIEW NEEDED]` marker and the chapter
  is flagged **NeedsReview** (rather than failing the whole chapter); the Orchestrator metadata turn is
  skipped so an unverified pass can't pollute your glossary.

### Cost & usage tracking

Every model round — Translator, Reviewer, and the Orchestrator's tool turn — is metered. honya
accumulates **prompt/completion tokens, tool-call count, and USD cost** (BYOK-aware: OpenRouter's fee
plus the upstream provider charge) and shows them live in the **Translate** meter, then persists them
per chapter and rolls them up to the volume and the whole project. Costs are cumulative "lifetime
spend" — re-translating a chapter adds to its running total rather than resetting it.

### Resume & run history

When a run starts, honya writes a recovery checkpoint before spending tokens and appends a matching
run-history row to `VOLUME.md`. If the app is killed or power is lost, the next launch offers to
resume the same chapter queue and skips clean chunk markers already committed to `translated/`.
Finishing, stopping, failing, or discarding an interrupted run closes out the durable history row with
counts, review-needed flags, token/tool usage, and USD cost.

## Project layout

A project directory mirrors the spec exactly:

```
your_project/
├── PROJECT.md        # synopsis, world-building, localization guide
├── CHARACTERS.md     # roster: self/target pronouns, speech style, relationships
├── GLOSSARY.md       # locked/protected terms, skills, item names, honorifics
├── STYLE.md          # translation-memory notes / reference examples
├── images/           # all illustrations, relocated here on import
└── Vol_01/
    ├── VOLUME.md     # running recap + chapter summaries + usage + run history + reader notes
    ├── raw/          # ch_001.md … pre-processed clean Japanese Markdown
    └── translated/   # ch_001.md … final verified target-language Markdown
```

Each metadata file keeps its machine state in a `<!-- honya:data … honya:data -->` JSON block below
the human-readable table, so the tables you read are always re-rendered from the truth — **never
hand-edit the block**. Re-opening a project always re-scans these files from disk, so a finished
chapter never reverts to a stale snapshot.

## Exporting

When a volume is finished, export it from the command palette (`:` → *Export*) to
**Markdown**, **EPUB**, or **DOCX**. honya checks integrity first and warns you if a chapter is
still incomplete or flagged **NeedsReview**, so a partial volume can't ship by accident.

## Updating

```sh
honya update          # download the latest release, verify its checksum, replace the binary
```

`honya update` (aliases: `self-update`, `upgrade`) replaces the installed binary **in place** — it
downloads the latest GitHub release for your platform, verifies its SHA-256 against the published
checksum, and atomically swaps the running executable. Re-running the installer works too.

On Windows the running `honya.exe` can't be overwritten while it's mapped, so `honya update` moves the
old binary aside and installs the new one **immediately** — the updated version runs from the next
launch onward, and the moved-aside file is reaped automatically the next time honya starts.

At startup honya does a **best-effort, non-blocking** check for a newer release and shows a footer
hint (`⬆ … honya update`) when one is out. Opt out with `HONYA_NO_UPDATE_CHECK=1`.

Other commands: `honya --version` (`-V`), `honya --help` (`-h`).

## Development

```sh
cargo test       # full suite: cleanse rules, EPUB parse/segment, Markdown render,
                 # UI render smoke, and the offline mock e2e (no API key needed)
cargo clippy --all-targets --locked -- -D warnings   # lint clean, warnings as errors
```

## License

Licensed under the [Apache License 2.0](LICENSE).
