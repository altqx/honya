# honya 本屋

A calm, literary **Ratatui terminal app** for AI-assisted **Japanese → Thai** light-novel
translation. Drop an EPUB in a folder, and honya pre-processes it (spine-ordered chapters,
relocated illustrations, HTML→Markdown cleanse) and runs a three-agent pipeline
(**Orchestrator · Translator · Reviewer**) over an OpenRouter-compatible API — with the agents
keeping your character, glossary, and volume notes current **through tool calls**.

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

The three models are configurable per agent; defaults follow the spec
(`google/gemini-3.5-flash` orchestrator, `google/gemini-3-flash-preview` translator,
`google/gemini-3.1-flash-lite` reviewer).

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
| `2` | **棚 Project** | Volume/chapter tree with waxing-moon status (`○ ◐ ◑ ●`), context files, detail card. `t` translate chapter · `T` whole volume. |
| `3` | **訳 Translate** | The live run: chunk gauge, three agent lines, token meter, streaming Thai preview. `p` pause · `s` stop · `f` follow. |
| `4` | **読 Reader** | Synced side-by-side JA ↔ TH proofreading. `[ ]` chapters · `z` sync · `o` layout. |
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
    ├── VOLUME.md     # running recap + per-chapter summaries
    ├── raw/          # ch_001.md … pre-processed clean Japanese Markdown
    └── translated/   # ch_001.md … final verified Thai Markdown
```

Each metadata file keeps its machine state in a `<!-- honya:data … honya:data -->` JSON block
below the human-readable table, so the tables you read are always re-rendered from the truth.

## Pre-processing (Rust, at import)

- **Spine-ordered** chapters from `content.opf` (true reading order, not filename order).
- **Media relocation**: every PNG/JPG/SVG illustration copied into `images/`.
- **Image-only detection**: illustration pages render their image link straight to `translated/`,
  skipping the agents entirely.
- **Cleanse rules** (exact): `<br>`→`&nbsp;`, bold/italic spans → `**` / `*`,
  「…」→ "…", 『…』→ '…', `<ruby>`→`Base (Furigana)`, `<img>`/SVG `<image>` →
  `![ภาพประกอบ](../../images/file.png)`.

## The pipeline

Per chapter: chunk to ~1000 tokens → for each chunk, inject the previous chunk's last 5 Thai
sentences for continuity, bundle the glossary/character/project/style context, then
**Translator → Reviewer**. On *reject*, the reviewer's itemized feedback is routed back for a retry
(up to a configurable cap). On *approve*, the Thai is appended deterministically, and the
**Orchestrator** runs a tool turn to persist any new characters/terms/notes and advance the recap.

## Development

```sh
cargo test       # 48 tests: cleanse rules, EPUB parsing, UI render smoke, full mock e2e
cargo clippy     # lint clean
```

---

🤖 Built with [Claude Code](https://claude.com/claude-code).
