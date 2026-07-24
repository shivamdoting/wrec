---
name: wrec
description: Record the mac screen with the wrec CLI. Use when asked to record the screen, capture a display, window, or app, take a screen recording, or verify UI behavior with a recording on macOS.
---

# wrec

wrec is a JSON-first macOS screen recorder. Prefer the CLI, pass `--json` on
every command that supports it, select exact targets, and treat daemon job
state as the source of truth.

## Canonical documentation

- Agent and CLI contract: https://wrec.app/docs/agents
- Configuration: https://wrec.app/docs/configuration
- Architecture: https://wrec.app/docs/architecture

Use the agent contract for the current selectors, recording options, JSON
schema, examples, and recovery guidance. Use `wrec --help` to confirm the
options supported by the installed binary if its version differs from the
live documentation.

If `wrec` is not on PATH, ask the user to install it:
`curl -fsSL https://wrec.app/install | sh`

Keep it current with `wrec update` (`wrec update --check --json` reports
`update_available` without installing).

## Workflow

1. Discover targets. Ids are stable only for the current list — refresh before
   each new task:

   ```sh
   wrec targets --json
   ```

2. Record an exact target. Prefer `--target kind:id` over name matching.
   Foreground mode streams job events until the job reaches a terminal status:

   ```sh
   wrec record start --target display:1 --duration 30s --json
   ```

3. Use `--detach` when another process will monitor or control the job:

   ```sh
   wrec record start --target window:438 --duration 5m --detach --json
   ```

4. Inspect and control jobs with the id from `job_submitted` or `wrec jobs`:

   ```sh
   wrec jobs --json
   wrec job show <id> --json
   wrec job pause <id> --json
   wrec job resume <id> --json
   wrec job stop <id> --json
   wrec job cancel <id> --json
   ```

## Rules

- A recording is done only when its job status is `completed`, `failed`, or
  `cancelled`.
- Foreground `record start --json` exits 0 for `completed` and nonzero for
  `failed` or `cancelled`. Detached mode exits after submission; inspect the
  job later to determine final success.
- JSON errors include `code`, `message`, `recoverable`, and `next`. If
  `recoverable` is true, follow `next` as the retry instruction; otherwise
  stop and report the error.
- The daemon runs one active recording. Extra recordings queue by default
  unless `--no-queue` is passed.
- Recordings are hardware-encoded `.mov` files, `~/Movies/Wrec` by default.
  The final location is `output_path` on the job snapshot.
- A duration recording keeps running if stdin closes, so append `</dev/null`
  for non-interactive runs.
