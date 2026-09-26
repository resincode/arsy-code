# ADR-0015: Split `.arsy` into what a person edits and what ARSY keeps

- Status: Proposed
- Date: 2026-09-26

## Context

Both `.arsy` directories grew one file at a time. A workspace's `.arsy/` holds
files meant to be committed — `arsy.json`, `guard.json`, `AGENTS.md`,
`plugins/` — beside the session store, artifacts, the repository map, and the
git worktrees subagents run in. Ignoring the second kind without the first
takes a `.gitignore` of negated patterns that nobody writes correctly the first
time. The operator's `~/.arsy/` keeps credential files and a cache at the same
level as the configuration, so a directory listing does not say which files are
safe to delete and which hold secrets.

Nothing in that layout could be managed from ARSY CODE itself: `/settings`
wrote only the user file, `/hooks` could only switch a hook off, and no command
showed where any of it lived.

## Decision

Every file ARSY writes on its own belongs to one of three directories, and
everything a person edits stays at the top of `.arsy/`:

```
~/.arsy/                       <project>/.arsy/
├── arsy.json                  ├── arsy.json
├── guard.json                 ├── guard.json
├── secrets/   (0700)          ├── AGENTS.md
│   ├── credentials.json       ├── plugins/
│   └── <handle>   (0600)      └── state/        (ignores itself)
└── cache/                         ├── sessions.sqlite3
    └── mcp-tools.json             ├── repo-map.json
                                   ├── artifacts/
                                   ├── views/
                                   └── eval/
```

- `state/` and `cache/` may be deleted; ARSY rebuilds what it needs.
  `state/` writes its own `.gitignore` (`*`), which `storage.state_gitignore`
  can switch off.
- `secrets/` is written only through the credential commands.
- An existing layout is moved once, by rename, and never over a file that is
  already at the destination. Workspace state moves on the first run in that
  workspace; a credential moves the first time it is resolved. `views/` and
  `eval/` are git worktrees and are not moved: `arsy storage clean views`
  removes the old ones.

Every location can be read and managed without editing a file by hand:
`/settings` and `arsy config set|unset [--workspace]` write either layer,
`/storage` and `arsy storage` list and clean the directories, and `/hooks` and
`arsy hook add|remove [--workspace]` edit ARSY's own `guard.json`. Claude and
Codex files stay read-only.

## Consequences

A workspace needs no `.gitignore` entry for ARSY's runtime files. A second ARSY
CODE process of an older version, running in the same workspace during the
move, can recreate files at the old paths; they are then ignored. The shared
`~/.arsy` is also read by ARSY: once ARSY CODE moves a credential into
`secrets/`, an ARSY build that only reads the old location no longer finds it,
so the credential move must not ship before ARSY resolves `secrets/` as well.

## Alternatives

Keeping one flat directory and documenting a `.gitignore` leaves every
workspace carrying the same negated patterns. Moving state out of the workspace
(for example under `~/.arsy/workspaces/<hash>`) separates it from the git
worktrees and cleanup it shares a disk with, and loses it when a checkout is
moved. Reading both old and new locations forever keeps two code paths alive
for every file.

## Invariant

ARSY CODE writes runtime state only under `<workspace>/.arsy/state/`, caches
only under `~/.arsy/cache/`, and secrets only under `~/.arsy/secrets/`. A
migration never replaces a file that already exists.
