# wrec docs site

Astro site for the public wrec landing page and docs.

## Commands

Run commands from this directory.

```sh
bun install
bun run dev
bun run format
bun run check
```

Do not use npm, pnpm, yarn, or npx in this project.

## Pages

- `src/pages/index.astro` is the minimal landing page.
- `src/pages/docs/` documents the agent CLI contract and runtime architecture.

The docs should stay aligned with the native shell in `apps/mac`, the shared
protocol in `crates/control`, and the agent-facing CLI in `crates/cli`.
