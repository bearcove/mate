# Bud Issues Skill

## What this skill covers

Use this skill when working with GitHub issues through `bud`.

`bud issues` keeps a repo-local issue mirror under `/tmp/bud-issues/<owner>/<repo>/`
so you can review, create, and assign issues without leaving your tmux workflow.

## Core idea

1. Run `bud issues` in the target repo.
2. Read issue files from the generated local layout.
3. Assign work with context using `bud assign --issue <number>`.

## Command flow

- `bud issues`
  - infers the repo from `git remote origin`
  - pushes local edits (compares `all/` against `.snapshots/`)
  - creates issues from drafts in `new/`
  - fetches issues from GitHub
  - generates local files and indexes under `/tmp/bud-issues/<owner>/<repo>/`
- `bud assign` and `bud assign --issue <number>`
  - sends tasks to the buddy
  - `--issue` injects full synced issue content and adds a commit-reference reminder
- `bud show`, `bud spy`, `bud steer`, `bud update`, `bud wait`
  - normal request-management commands

## Directory layout

For repo `<owner>/<repo>`:

- `/tmp/bud-issues/<owner>/<repo>/all/`
  - one markdown file per issue
  - filenames: `<number> - <title>.md`
- `open/`, `closed/`
  - state-filtered symlinks into `all/`
- `by-created/`, `by-updated/`
  - date-sorted symlinks by created/updated timestamps
- `labels/`
  - optional per-label symlink directories
- `milestones/`
  - optional per-milestone symlink directories
- `deps/`
  - optional dependency relationship symlinks
- `new/`
  - issue draft queue for creation
  - contains `TEMPLATE.md`
- `.snapshots/`
  - issue content snapshots used for edit detection
- `INDEX.md`
  - quick index for open/closed issue references
- `DEPS.md`
  - dependency graph summary
- `LABELS.md`
  - known label metadata
- `MILESTONES.md`
  - known milestone metadata

## Assignment with issue context

Use:

```
cat <<'EOF' | bud assign --issue 42
Please investigate the regression in issue #42.
EOF
```

`bud` injects the full issue markdown into the task payload.

## Issue creation workflow

1. Create a draft in `new/` using the existing `TEMPLATE.md`.
2. Put metadata in the top block (`**Labels:**`, `**Milestone:**`, `**Assignees:**`)
   and write the issue body below the `---` separator.
3. Run `bud issues` — drafts are created and edits are pushed before syncing.

## Working flow for captains and buddies

1. Captain: run `bud issues` to sync context.
2. Captain: assign tasks, possibly with `--issue`.
3. Buddy: claim and work on tasks, then respond via `bud respond`.
4. Captain: use `bud update` for status and `bud steer` for clarification.
5. Captain: use `bud show` and `bud wait` to track completion.
