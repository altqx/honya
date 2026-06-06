<div align="center">

# honya 本屋

**A calm, literary terminal app for AI-assisted Japanese → Thai light-novel translation.**

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-3A5078.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/altqx/honya?color=6A8258&label=release)](https://github.com/altqx/honya/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/altqx/honya/ci.yml?branch=main&label=ci&color=6A8258)](https://github.com/altqx/honya/actions/workflows/ci.yml)

[Installation](#installation) · [Quick start](#quick-start) · [The five screens](#the-five-screens)

</div>

---

Drop an EPUB into a folder and honya turns it into a finished Thai translation. It reads the book
in true spine order, relocates the illustrations, cleanses the XHTML into tidy Markdown, then runs
a three-agent LLM pipeline — **Orchestrator · Translator · Reviewer** — over an OpenRouter-compatible
API. The agents keep your character roster, glossary, and volume notes current **through tool calls**,
and every token and dollar is accounted for as the run progresses.

It's a single static binary, built on [Ratatui](https://ratatui.rs). No Electron, no browser, no
telemetry — just a quiet workspace in your terminal.

> [!IMPORTANT]
> honya needs an **OpenRouter API key**. On first launch it prompts you to paste one and saves it,
> so you're only asked once. Grab a key at <https://openrouter.ai/keys>.

## Highlights

- **EPUB in, Thai out** — spine-ordered import, illustration relocation, and HTML→Markdown cleanse, all automatic.
- **Three specialized agents** — Translator and Reviewer iterate per chunk; the Orchestrator persists new terms and characters.
- **Continuity that holds** — per-chunk glossary/character context plus the previous chunk's tail keep voice and terminology consistent across a whole volume.
- **Cost transparency** — live token + USD meter during a run, rolled up per chapter, volume, and project.
- **Side-by-side proofreading** — synced JA ↔ TH reader, both panes rendered from Markdown.
- **12 themes** with a live-preview picker (`Ctrl-T`), from Washi 和紙 paper to Tokyo Night.
- **Self-updating** — `honya update` swaps the binary in place after verifying its checksum.

## Installation

### Quick install (Linux · macOS)

```sh
curl https://honya.altqx.com/install.sh | bash
```

The installer downloads the latest prebuilt binary for your platform, **verifies its SHA-256
checksum**, and installs it to `~/.local/bin`. If that directory isn't on your `PATH`, the script
prints the exact line to add for bash / zsh / fish.

Prebuilt binaries are published for:

| OS | Architectures |
|----|---------------|
| **Linux** (gnu) | `x86_64` · `aarch64` |
| **macOS** | `x86_64` (Intel) · `aarch64` (Apple Silicon) |

<details>
<summary>Installer options</summary>

```sh
# install to a custom directory
curl https://honya.altqx.com/install.sh | bash -s -- --dir ~/bin

# pin a specific release
curl https://honya.altqx.com/install.sh | bash -s -- --version v0.1.0

# force a source build via cargo
curl https://honya.altqx.com/install.sh | bash -s -- --source
```

| Flag | Env var | Default | Meaning |
|------|---------|---------|---------|
| `--dir <path>` | `HONYA_INSTALL_DIR` | `$HOME/.local/bin` | Install directory |
| `--version <tag>` | `HONYA_VERSION` | latest | Release tag to install |
| `--source` | — | off | Build from source via `cargo install` |
| — | `NO_COLOR` | — | Disable colored output |

On a platform without a prebuilt asset, the installer automatically falls back to a `cargo` source build.

</details>

### With Cargo

```sh
cargo install --git https://github.com/altqx/honya --locked honya
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
subdirectory, and any loose `*.epub` files are offered as one-press imports. From the **書架 Shelf**
tab, press `i` to import an EPUB into a new project, then `t` / `T` on the **棚 Project** tab to
translate a chapter or a whole volume.

On first run, honya prompts for your OpenRouter key and saves it — see [API key & models](#api-key--models).

## The five screens

| Tab | | Purpose |
|----|----|---------|
| `1` | **書架 Shelf** | Pick a project or import a new EPUB (`i`). |
| `2` | **棚 Project** | Volume/chapter tree with waxing-moon status (`○ ◐ ◑ ●`), context files, and a detail card with per-chapter token/cost roll-up. `t` translate chapter · `T` whole volume. |
| `3` | **訳 Translate** | The live run: chunk gauge, three agent lines, token + USD meter, streaming Thai preview. `p` pause · `s` stop · `f` follow. |
| `4` | **読 Reader** | Synced side-by-side JA ↔ TH proofreading, both panes rendered from Markdown. `[ ]` chapters · `z` sync · `o` layout. |
| `5` | **辞 Lexicon** | Browse/edit Glossary, Characters, Style. `n` new · `e` edit · `d` delete · `/` search. |

**Global keys** (always available — `?` lists the full table):
`?` help · `:` command palette · `Ctrl-T` theme · `l` activity log · `1`–`5` / `Tab` switch tabs ·
`Esc` close overlay · `Esc` / `Backspace` dismiss notification · `q` quit.
On the **Project** tab, `l` expands/focuses the tree; use backtick (`` ` ``) for the activity log there instead.

## API key & models

honya talks to OpenRouter (`https://openrouter.ai/api/v1` by default — configurable in **Settings**),
so it needs a key. Resolution order:

1. `HONYA_API_KEY`, then `OPENROUTER_API_KEY` from the environment (these always win).
2. Otherwise, the key saved at `~/.config/honya/config.json` (or `$XDG_CONFIG_HOME/honya/config.json`).

If neither is present, honya prompts for the key at startup (hidden input) and writes it to that
config file (`0600` on Unix) so subsequent launches don't ask again.

The three agents have **independently configurable models** (a `ModelSet`, overridable per project);
the defaults are:

| Agent | Default model |
|-------|---------------|
| **Orchestrator** | `google/gemini-3.5-flash` |
| **Translator** | `google/gemini-3-flash-preview` |
| **Reviewer** | `google/gemini-3.1-flash-lite` |

<details>
<summary>All environment variables</summary>

| Variable | Effect |
|----------|--------|
| `HONYA_API_KEY` | OpenRouter API key (checked first). |
| `OPENROUTER_API_KEY` | OpenRouter API key (fallback). |
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

- **Spine-ordered** chapters from `content.opf` — true reading order, not filename order.
- **Media relocation** — every PNG/JPG/SVG illustration is copied into `images/` (dedup-safe).
- **Image-only detection** — illustration pages render their image link straight to `translated/`, skipping the agents entirely.
- **Image segmentation** — stray illustration pages fold into the surrounding chapter, and `m###`-style title plates are detected as chapter heads.
- **Cleanse rules** (exact): `<br>` → `---` (thematic break; stacked ones collapse to a single divider),
  bold/italic spans → `**` / `*`, 「…」 → "…", 『…』 → '…', `<ruby>` → `Base (Furigana)`,
  `<img>` / SVG `<image>` → `![ภาพประกอบ](../../images/file.png)`.

### The pipeline

Per chapter: chunk to ~1000 tokens (1200 hard cap) → for each chunk, inject the previous chunk's
last 5 Thai sentences for continuity and the **per-chunk** reference context (only the glossary terms
and characters whose Japanese form actually appears in this chunk, capped at 80 / 40), then run
**Translator → Reviewer**.

- On *reject*, the reviewer's itemized feedback is routed back for a retry (up to a configurable cap, default 3).
- On *approve*, the Thai is appended **deterministically, app-side** — not via an LLM tool — and the
  **Orchestrator** runs a tool turn to persist any new characters/terms/notes and advance the recap.
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
├── GLOSSARY.md       # locked terms, skills, item names, honorifics
├── STYLE.md          # translation-memory notes / reference examples
├── images/           # all illustrations, relocated here on import
└── Vol_01/
    ├── VOLUME.md     # running recap + per-chapter summaries + usage + run history
    ├── raw/          # ch_001.md … pre-processed clean Japanese Markdown
    └── translated/   # ch_001.md … final verified Thai Markdown
```

Each metadata file keeps its machine state in a `<!-- honya:data … honya:data -->` JSON block below
the human-readable table, so the tables you read are always re-rendered from the truth — **never
hand-edit the block**. Re-opening a project always re-scans these files from disk, so a finished
chapter never reverts to a stale snapshot.

## Updating

```sh
honya update          # download the latest release, verify its checksum, replace the binary
```

`honya update` (aliases: `self-update`, `upgrade`) replaces the installed binary **in place** — it
downloads the latest GitHub release for your platform, verifies its SHA-256 against the published
checksum, and atomically swaps the running executable. Re-running the installer works too.

At startup honya does a **best-effort, non-blocking** check for a newer release and shows a footer
hint (`⬆ … honya update`) when one is out. Opt out with `HONYA_NO_UPDATE_CHECK=1`.

Other commands: `honya --version` (`-V`), `honya --help` (`-h`).

## Development

```sh
cargo test       # full suite: cleanse rules, EPUB parse/segment, Markdown render,
                 # UI render smoke, and the offline mock e2e (no API key needed)
cargo clippy --all-targets --locked -- -D warnings   # lint clean, warnings as errors
```

The version in `Cargo.toml` is the **single source of truth** — CI auto-tags on a version change, so
a release is cut simply by bumping `version` there.

## License

Licensed under the [Apache License 2.0](LICENSE).
