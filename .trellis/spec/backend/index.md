# Backend Development Guidelines

> Best practices for backend development in this project.

---

## Overview

The backend is **Rust** (`src/prism-core`), one crate producing two binaries:

- `prism-core.exe` — broker, runs as the ordinary user. Owns app inventory, web
  keywords, usage history, Shell/COM actions, user settings, and forwards file
  search to the indexer.
- `prism-indexer-service.exe` — Windows service running as LocalSystem. Owns
  MFT/USN indexing, the v5 disk cache, the pinyin sidecar, and a versioned
  **read-only** search protocol. It must never execute Shell verbs, touch the
  clipboard, run user commands, or make network requests.

Several files here were generated from a web-service template. The table states
which carry real project content; ignore template prompts about ORMs, migrations,
or HTTP endpoints — this project has none of those.

---

## Guidelines Index

| Guide | Description | Status |
|-------|-------------|--------|
| [Quality Guidelines](./quality-guidelines.md) | Forbidden patterns, memory acceptance gate, indexing known gaps | **Filled — authoritative** |
| [Error Handling](./error-handling.md) | Broker-owned Shell boundary, `ShellError` kinds, STA worker | **Filled — authoritative** |
| [Logging Guidelines](./logging-guidelines.md) | Per-process JSONL, redaction, panic hook under `panic = "abort"` | **Filled — authoritative** |
| [Directory Structure](./directory-structure.md) | Module layout and process ownership | **Filled** |
| [Database Guidelines](./database-guidelines.md) | Not applicable | **Not applicable** — no database; see persistence table in directory-structure |

---

## How to Fill These Guidelines

For each guideline file:

1. Document your project's **actual conventions** (not ideals)
2. Include **code examples** from your codebase
3. List **forbidden patterns** and why
4. Add **common mistakes** your team has made

The goal is to help AI assistants and new team members understand how YOUR project works.

---

**Language**: All documentation should be written in **English**.
