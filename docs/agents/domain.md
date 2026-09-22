# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring
the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root
- **`docs/adr/`**: read ADRs that touch the area you're about to work in

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't
suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs`
and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually
get resolved.

## File structure

This is a **single-context** repo:

```
/
├── CONTEXT.md
├── docs/adr/
│   ├── 0001-background-tasks-never-touch-app-state.md
│   └── 0002-data-block-is-the-source-of-truth.md
└── src/
```

If honya ever splits into multiple bounded contexts, the signal is a `CONTEXT-MAP.md` at the
root pointing at one `CONTEXT.md` per context, with `src/<context>/docs/adr/` for
context-scoped decisions. It doesn't today.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a
hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms
the glossary explicitly avoids.

Note that `CLAUDE.md` already carries a lot of this vocabulary — *shelf*, *workspace*,
*chunk*, *data block*, *System One judgement*, *review gate*, *mutation funnel*. Treat it as
the de-facto glossary until a `CONTEXT.md` exists, and don't restate it there.

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently
overriding:

> _Contradicts ADR-0002 (data block is the source of truth), but worth reopening because…_
