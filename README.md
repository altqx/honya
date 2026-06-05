# honya 本屋

A calm, literary **Ratatui terminal app** for AI-assisted **Japanese → Thai** light-novel
translation. Drop an EPUB in a folder, and honya pre-processes it (spine-ordered chapters,
relocated illustrations, HTML→Markdown cleanse) and runs a three-agent pipeline
(**Orchestrator · Translator · Reviewer**) over an OpenRouter-compatible API — with the agents
keeping your character, glossary, and volume notes current **through tool calls**, and every
token and dollar accounted for as it goes.

> honya requires an **OpenRouter API key**. On first launch it
> prompts you to paste a key, then saves it so you're only asked once.

## Build & run

```sh
cargo run --release        # launches the TUI in the current directory
```

honya treats the **current working directory** as your shelf: each translation project is a
subdirectory, and any loose `*.epub` files are offered as one-press imports.

## API key (required)

honya talks to OpenRouter (`https://openrouter.ai/api/v1` by default — configurable in
**Settings**), so it needs a key. Resolution order is:

1. `HONYA_API_KEY`, then `OPENROUTER_API_KEY` from the environment (these always win):

   ```sh
   export HONYA_API_KEY=sk-or-...
   ```

2. Otherwise, the key saved at `~/.config/honya/config.json` (`$XDG_CONFIG_HOME/honya` if set).

If neither is present, honya **prompts for the key at startup** (hidden input) and writes it to
that config file (`0600` on Unix) so subsequent launches don't ask again. Get a key at
<https://openrouter.ai/keys>.

The three agents have **independently configurable models** (a `ModelSet`, overridable
per project); the defaults follow the spec:

| Agent | Default model |
|-------|---------------|
| **Orchestrator** | `google/gemini-3.5-flash` |
| **Translator** | `google/gemini-3-flash-preview` |
| **Reviewer** | `google/gemini-3.1-flash-lite` |

## Updating

```sh
honya update          # download the latest release, verify its checksum, replace the binary
```

`honya update` updates the installed binary **in place** — it downloads the latest GitHub
release for your platform, verifies its SHA-256 against the published checksum, and atomically
replaces the running executable. Re-running the installer
(`curl https://honya.altqx.com/install.sh | bash`) works too.

At startup honya does a **best-effort, non-blocking** check for a newer release and shows a
footer hint (`⬆ … honya update`) when one is out. Opt out with `HONYA_NO_UPDATE_CHECK=1`. Other
useful commands: `honya --version`, `honya --help`.

## The five screens

| Tab | | Purpose |
|----|----|---------|
| `1` | **書架 Shelf** | Pick a project or import a new EPUB (`i`). |
| `2` | **棚 Project** | Volume/chapter tree with waxing-moon status (`○ ◐ ◑ ●`), context files, detail card with per-chapter token/cost roll-up. `t` translate chapter · `T` whole volume. |
| `3` | **訳 Translate** | The live run: chunk gauge, three agent lines, token + USD meter, streaming Thai preview. `p` pause · `s` stop · `f` follow. |
| `4` | **読 Reader** | Synced side-by-side JA ↔ TH proofreading, both panes rendered from Markdown. `[ ]` chapters · `z` sync · `o` layout. |
| `5` | **辞 Lexicon** | Browse/edit Glossary, Characters, Style. `n` new · `e` edit · `d` delete · `/` search. |

Global keys are always shown in the footer: `?` help · `:` command palette · `l` activity log ·
`1`–`5`/`Tab` switch tabs · `Esc` close overlay · `q` quit.

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
    ├── VOLUME.md     # running recap + per-chapter summaries + usage roll-up
    ├── raw/          # ch_001.md … pre-processed clean Japanese Markdown
    └── translated/   # ch_001.md … final verified Thai Markdown
```

Each metadata file keeps its machine state in a `<!-- honya:data … honya:data -->` JSON block
below the human-readable table, so the tables you read are always re-rendered from the truth —
never hand-edit the block. Re-opening a project always re-scans these files from disk, so a
finished chapter never reverts to a stale snapshot.

## Pre-processing (Rust, at import)

- **Spine-ordered** chapters from `content.opf` (true reading order, not filename order).
- **Media relocation**: every PNG/JPG/SVG illustration copied into `images/` (dedup-safe).
- **Image-only detection**: illustration pages render their image link straight to `translated/`,
  skipping the agents entirely.
- **Image segmentation**: stray illustration pages are folded into the surrounding chapter, and
  `m###`-style title plates are detected as chapter heads.
- **Cleanse rules** (exact): `<br>`→`&nbsp;`, bold/italic spans → `**` / `*`,
  「…」→ "…", 『…』→ '…', `<ruby>`→`Base (Furigana)`, `<img>`/SVG `<image>` →
  `![ภาพประกอบ](../../images/file.png)`.

## The pipeline

Per chapter: chunk to ~1000 tokens → for each chunk, inject the previous chunk's last 5 Thai
sentences for continuity and the **per-chunk** reference context (only the glossary terms and
characters whose Japanese form actually appears in this chunk, capped at 80 / 40), then
**Translator → Reviewer**. On *reject*, the reviewer's itemized feedback is routed back for a
retry (up to a configurable cap). On *approve*, the Thai is appended **deterministically,
app-side** — not via an LLM tool — and the **Orchestrator** runs a tool turn to persist any new
characters/terms/notes and advance the recap.

## Cost & usage tracking

Every model round — Translator, Reviewer, and the Orchestrator's tool turn — is metered. honya
accumulates **prompt/completion tokens, tool-call count, and USD cost** (BYOK-aware: OpenRouter's
fee plus the upstream provider charge) and:

- shows them live in the **Translate** meter as the run progresses;
- **persists** them per chapter and rolls them up to the volume and the whole project, so the
  **Project** screen's detail card shows what each chapter, volume, and project has cost so far.

Costs are cumulative "lifetime spend" — re-translating a chapter adds to its running total
rather than resetting it.

## Development

```sh
cargo test       # full suite: cleanse rules, EPUB parse/segment, Markdown render,
                 # UI render smoke, and the offline mock e2e (no API key needed)
cargo clippy --all-targets --locked -- -D warnings   # lint clean, warnings as errors
```

The version in `Cargo.toml` is the **single source of truth** — CI auto-tags on a version change,
so a release is cut simply by bumping `version` there.
