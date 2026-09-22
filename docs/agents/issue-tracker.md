# Issue tracker: Linear

Issues and specs for this repo live in **Linear**: workspace `altq`, team **letitia**
(key `ALT`), project **honya**. Issue identifiers look like `ALT-123`.

Code, PRs and releases stay on GitHub (`altqx/honya`). Only issue tracking is in Linear.

## Interface

Use the Linear MCP tools, named `mcp__plugin_design_linear__*`. They are deferred: load a
schema with `ToolSearch("select:mcp__plugin_design_linear__save_issue,…")` before the first
call in a session. Never guess parameters — read the schema.

There is no Linear CLI on this machine; don't reach for one.

## Conventions

- **Create an issue**: `save_issue` with `team: "letitia"`, `project: "honya"`, `title`, and
  `description` as Markdown. Pass literal newlines, not `\n` escapes. Omit `id` when creating.
- **Read an issue**: `get_issue` with the identifier (`ALT-123`), then `list_comments` with
  the same `issueId` for discussion.
- **List issues**: `list_issues` with `project: "honya"`, plus `state`, `label` or `assignee`
  filters. Ask for `fields` explicitly — e.g.
  `["title","description","status","labels","assignee","url","parentId"]`.
- **Comment**: `save_comment` with `issueId` and `body`. Reply in-thread with `parentId`.
- **Apply / remove labels**: `save_issue` with `addLabels` / `removeLabels`. Both are
  incremental. The `labels` parameter **replaces the entire set** — prefer the incremental
  pair so a triage pass never silently drops an `area:` label.
- **Close**: `save_issue` with `state: "Done"`. Use `state: "Canceled"` for won't-fix and
  `state: "Duplicate"` for duplicates.

Team `letitia` has these states: `Backlog`, `Todo`, `In Progress`, `In Review`, `Done`,
`Canceled`, `Duplicate`.

## Project and labels already exist

The Linear project is **honya** (`P-ALT-6`, https://linear.app/altq/project/honya-f1b47dc97112),
led by team `letitia`. The five triage labels exist too — see `triage-labels.md`.

Don't create either. `save_issue_label` rejects a duplicate name with a 400.

### Label scoping gotcha

The five triage labels are **team-scoped to `letitia`**, not workspace-scoped. `list_issue_labels`
called without `team` returns only the workspace-scoped set (`area:*`, `quality:*`, `track:*`,
`Bug`/`Feature`/`Improvement`) and the five will appear to be missing. Always pass
`team: "letitia"` when checking whether a triage label exists.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external GitHub PRs as
feature requests; `/triage` reads this flag.)_

When set to `yes`, read PRs with `gh pr view <n> --comments` and `gh pr diff <n>` against
`altqx/honya`, but record the triage verdict as a Linear issue — GitHub labels are not the
vocabulary here.

## When a skill says "publish to the issue tracker"

Create a Linear issue in team `letitia`, project `honya`.

## When a skill says "fetch the relevant ticket"

`get_issue` with the identifier, then `list_comments` for the thread.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a parent issue; **tickets** are its sub-issues.

- **Map**: an issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body.
- **Child ticket**: `save_issue` with `parentId: "<map identifier>"` and a
  `wayfinder:<type>` label (`research` / `prototype` / `grilling` / `task`).
- **Blocking**: Linear's native relations — `save_issue` with `blockedBy: ["ALT-12"]` or
  `blocks: [...]`. Both are append-only; use `removeBlockedBy` / `removeBlocks` to clear.
  A ticket is unblocked when every blocker reached a completed or canceled state.
- **Frontier query**: `list_issues` with `parentId: "<map>"` and `state` filtered to
  `Backlog`/`Todo`; drop any with an open blocker or an assignee; first in map order wins.
- **Claim**: `save_issue` with `id: "ALT-<n>"`, `assignee: "me"` — the session's first write.
- **Resolve**: `save_comment` with the answer, `save_issue` to `state: "Done"`, then append a
  context pointer to the map's Decisions-so-far via `save_issue` `patch`.
