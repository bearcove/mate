# Mate Issues Skill

## What this skill covers

Use this skill when working with GitHub issues through `mate`.

`mate issues` keeps a repo-local issue mirror under `/tmp/mate-issues/<owner>/<repo>/`
so you can review, create, and assign issues without leaving your tmux workflow.

## Core idea

1. Run `mate issues` in the target repo.
2. Read issue files from the generated local layout.
3. Assign work with context using `mate assign --issue <number>`.

## Command flow

- `mate issues`
  - infers the repo from `git remote origin`
  - pushes local edits (compares `all/` against `.snapshots/`)
  - creates issues from drafts in `new/`
  - fetches issues from GitHub
  - generates local files and indexes under `/tmp/mate-issues/<owner>/<repo>/`
- `mate assign` and `mate assign --issue <number>`
  - sends tasks to the mate
  - `--issue` injects full synced issue content and adds a commit-reference reminder
- `mate show`, `mate spy`, `mate steer`, `mate update`, `mate wait`
  - normal request-management commands

## Directory layout

For repo `<owner>/<repo>`:

- `/tmp/mate-issues/<owner>/<repo>/all/`
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
cat <<'EOF' | mate assign --issue 42
Please investigate the regression in issue #42.
EOF
```

`mate` injects the full issue markdown into the task payload.

## Issue creation workflow

1. Create a draft in `new/` using the existing `TEMPLATE.md`.
2. Put metadata in the top block (`**Labels:**`, `**Milestone:**`, `**Assignees:**`)
   and write the issue body below the `---` separator.
3. Run `mate issues` — drafts are created and edits are pushed before syncing.

## Working flow for captains and mates

1. Captain: run `mate issues` to sync context.
2. Captain: assign tasks, possibly with `--issue`.
3. Mate: claim and work on tasks, then respond via `mate respond`.
4. Captain: use `mate update` for status and `mate steer` for clarification.
5. Captain: use `mate show` and `mate wait` to track completion.
