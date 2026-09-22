# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the
actual label strings used in this repo's issue tracker (Linear team `letitia`).

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding
label string from this table.

Edit the right-hand column to match whatever vocabulary you actually use.

## Linear notes

All five labels **already exist** in Linear, scoped to team `letitia`, with the descriptions
above. Apply them; never create them. `save_issue_label` rejects a duplicate name with a 400.

They are **team-scoped, not workspace-scoped**. `list_issue_labels` without a `team` argument
returns only the workspace-scoped labels (`area:*`, `quality:*`, `track:*`, plus
`Bug`/`Feature`/`Improvement`) and these five will look missing. Pass `team: "letitia"` when
checking.

Team `letitia` also carries `Area`, `Quality`, `Track` and `Type` label *groups*, all
`singleSelect`. Their children don't appear in a listing unless `includeGroups: true` is set.
None of them collide with the five triage labels.

Use `addLabels` / `removeLabels` on `save_issue` rather than `labels`, so a triage pass never
wipes an issue's `Area` or `Type` group selection.

`wontfix` is kept as a label for vocabulary parity with the skills, but Linear's own idiom is
the **Canceled** state. Apply both: the label for the skills to read, the state so the issue
leaves the active board.
