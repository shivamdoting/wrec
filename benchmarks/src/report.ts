import { readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import type { Gate, OverallStatus, ProfileResult, SuiteName } from "./gates";

// Renders benchmarks/results/ (the committed run summaries) into two
// self-contained HTML pages: index.html (newest run in full + history) and
// how.html (the methodology). Pixel UI in the app's own voice — Departure
// Mono, embedded as base64 so a page attached to a GitHub release still
// renders correctly on its own. Verdicts pair an icon with a label, so
// state never rides on color alone.

type Summary = {
  schema: string;
  id: string;
  generatedAt: string;
  suite: SuiteName;
  status: OverallStatus;
  duration: string;
  binaries: { candidate: string; reference?: string };
  git: { branch: string; commit: string; dirty: boolean };
  machine: { arch: string; cpu?: string; cpuCount?: number; totalMemoryBytes?: number };
  environment: {
    macos?: { productVersion: string };
    chip?: string;
    memoryBytes?: number;
    loadAverage?: number[];
    guards?: Array<{ name: string; status: string; measured: unknown }>;
  };
  profiles: ProfileResult[];
};

const root = path.resolve(import.meta.dir, "..");
const resultsDir = path.join(root, "results");
const fontPath = path.resolve(
  root,
  "..",
  "apps/mac/Sources/WrecApp/Resources/DepartureMono-Regular.otf",
);

const statusMeta: Record<string, { icon: string; word: string; tone: string }> = {
  pass: { icon: "✓", word: "PASS", tone: "good" },
  fail: { icon: "✕", word: "FAIL", tone: "critical" },
  inconclusive: { icon: "◐", word: "INCONCLUSIVE", tone: "warning" },
  skipped: { icon: "–", word: "SKIPPED", tone: "muted" },
};

export const writeReport = async () => {
  const summaries = await loadSummaries();
  const font = await loadFont();
  const indexPath = path.join(root, "index.html");
  await writeFile(indexPath, shell("bench", renderIndex(summaries), font));
  await writeFile(path.join(root, "how.html"), shell("how", renderHow(), font));
  return indexPath;
};

const loadFont = async () => {
  try {
    return (await readFile(fontPath)).toString("base64");
  } catch {
    return null;
  }
};

const loadSummaries = async (): Promise<Summary[]> => {
  let names: string[] = [];
  try {
    names = (await readdir(resultsDir)).filter((name) => name.endsWith(".json"));
  } catch {
    return [];
  }
  const parsed = await Promise.all(
    names.map(async (name) => {
      try {
        return JSON.parse(await readFile(path.join(resultsDir, name), "utf8")) as Summary;
      } catch {
        return null;
      }
    }),
  );
  return parsed
    .filter((item): item is Summary => item !== null && typeof item?.id === "string")
    .sort((a, b) => b.id.localeCompare(a.id));
};

const chip = (status: string) => {
  const meta = statusMeta[status] ?? statusMeta.skipped;
  return `<span class="chip chip-${meta.tone}"><span aria-hidden="true">${meta.icon}</span>${meta.word}</span>`;
};

const fmt = (value: unknown, digits = 2): string => {
  if (value === null || value === undefined) return "–";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    if (!Number.isFinite(value)) return "–";
    return Number.isInteger(value) ? String(value) : value.toFixed(digits);
  }
  return escapeHtml(String(value));
};

const fmtBytes = (bytes: number | null | undefined) => {
  if (!bytes && bytes !== 0) return "–";
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
};

const fmtPercent = (ratio: number | null | undefined) =>
  ratio === null || ratio === undefined ? "–" : `${(ratio * 100).toFixed(1)}%`;

const escapeHtml = (value: string) =>
  value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");

const gateRow = (gate: Gate) => {
  const bytes = gate.name.includes("rss");
  return `
  <tr>
    <td>${escapeHtml(gate.name)}</td>
    <td>${chip(gate.status)}</td>
    <td class="num">${bytes && typeof gate.measured === "number" ? fmtBytes(gate.measured) : fmt(gate.measured)}</td>
    <td class="muted">${escapeHtml(gate.threshold)}</td>
    <td class="num">${gate.delta === null || gate.delta === undefined ? "–" : bytes ? fmtBytes(gate.delta) : fmt(gate.delta)}${
      gate.deltaPercent === null || gate.deltaPercent === undefined
        ? ""
        : ` <span class="muted">(${gate.deltaPercent >= 0 ? "+" : ""}${gate.deltaPercent.toFixed(1)}%)</span>`
    }</td>
    <td class="muted">${escapeHtml(gate.details ?? "")}</td>
  </tr>`;
};

const tile = (label: string, value: string, sub?: string) => `
  <div class="tile px">
    <div class="tile-label">${escapeHtml(label)}</div>
    <div class="tile-value">${value}</div>
    ${sub ? `<div class="tile-sub">${sub}</div>` : ""}
  </div>`;

const profileSection = (profile: ProfileResult) => {
  const o = profile.observed;
  const tiles = [
    tile(
      "observed fps",
      fmt(o?.effectiveFps),
      o?.stimulusAchievedFps ? `stimulus ${fmt(o.stimulusAchievedFps)} fps` : undefined,
    ),
    tile("completeness", fmtPercent(o?.captureCompleteness)),
    tile("start", `${fmt(profile.latency.startMs, 0)}<span class="unit">ms</span>`),
    tile("finalize", `${fmt(profile.latency.finalizeMs, 0)}<span class="unit">ms</span>`),
    tile(
      "cpu avg/p95",
      `${fmt(profile.process.avgTotalCpuPercent, 1)}/${fmt(profile.process.p95TotalCpuPercent, 1)}<span class="unit">%</span>`,
    ),
    tile("peak rss", fmtBytes(profile.process.maxTotalRssBytes)),
  ].join("");

  return `
  <section class="card px">
    <header class="card-head">
      <h3>${escapeHtml(profile.name)}</h3>
      ${chip(profile.status)}
    </header>
    <div class="tiles">${tiles}</div>
    ${
      profile.gates.length
        ? `<div class="table-wrap"><table>
        <thead><tr><th>gate</th><th>status</th><th class="num">measured</th><th>threshold</th><th class="num">Δ vs ref</th><th>notes</th></tr></thead>
        <tbody>${profile.gates.map(gateRow).join("")}</tbody>
      </table></div>`
        : `<p class="muted">smoke run — recorded ungated.</p>`
    }
  </section>`;
};

const latestSection = (latest: Summary) => {
  const guards = latest.environment.guards ?? [];
  const env = [
    latest.environment.chip,
    latest.environment.macos ? `macOS ${latest.environment.macos.productVersion}` : null,
    `${latest.git.commit.slice(0, 7)}${latest.git.dirty ? " (dirty)" : ""}`,
    latest.suite === "release" && latest.binaries.reference ? "A/B vs reference" : latest.suite,
    latest.duration,
  ]
    .filter(Boolean)
    .map((item) => `<span>${escapeHtml(String(item))}</span>`)
    .join(`<span class="dot">·</span>`);

  return `
  <section class="verdict px">
    <div class="verdict-chip">${chip(latest.status)}</div>
    <div>
      <h2>${escapeHtml(latest.id)}</h2>
      <p class="meta">${env}</p>
      <p class="meta">${guards
        .map((guard) => `${chip(guard.status)} <span class="muted">${escapeHtml(guard.name)}</span>`)
        .join(" &nbsp; ")}</p>
    </div>
  </section>
  ${latest.profiles.map(profileSection).join("")}`;
};

const historyRow = (run: Summary) => `
  <tr>
    <td>${escapeHtml(run.id)}</td>
    <td>${chip(run.status)}</td>
    <td>${escapeHtml(run.suite)}</td>
    <td>${escapeHtml(run.git.commit.slice(0, 7))}${run.git.dirty ? "*" : ""}</td>
    <td>${run.binaries.reference ? "A/B" : "budgets"}</td>
    <td class="muted">${escapeHtml(run.generatedAt)}</td>
  </tr>`;

const renderIndex = (summaries: Summary[]) => {
  const latest = summaries[0];
  return `
  ${latest ? latestSection(latest) : `<p class="muted">no results yet — run <code>bun run bench</code>.</p>`}
  ${
    summaries.length > 1
      ? `<section class="card px">
      <header class="card-head"><h3>history</h3></header>
      <div class="table-wrap"><table>
        <thead><tr><th>run</th><th>status</th><th>suite</th><th>commit</th><th>mode</th><th>generated</th></tr></thead>
        <tbody>${summaries.slice(1).map(historyRow).join("")}</tbody>
      </table></div>
    </section>`
      : ""
  }`;
};

const markerDemo = () => {
  // A miniature of the real marker strip: guard pattern + a few index bits.
  const blocks = [1, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1];
  return `<div class="marker" aria-hidden="true">${blocks
    .map((bit) => `<span class="${bit ? "on" : "off"}"></span>`)
    .join("")}</div>`;
};

const renderHow = () => `
  <section class="card px prose">
    <h2>how this was recorded</h2>
    <p>
      the bench never trusts what wrec says about itself. every number on the
      bench page is decoded out of the pixels of the finished recording — the
      same file a user would keep.
    </p>

    <h3>1 · a window that counts out loud</h3>
    <p>
      a stimulus window redraws 60 times a second, synced to the display. each
      redraw paints its own frame number as a strip of big black/white blocks —
      1 bit per block, with guard patterns at both ends:
    </p>
    ${markerDemo()}
    <p>
      the number advances once per <em>rendered</em> frame, so every index that
      reaches the screen is consecutive. the window also holds a power
      assertion — a dimming display would otherwise read as a frame-rate
      collapse.
    </p>

    <h3>2 · wrec records it, black box</h3>
    <p>
      the harness drives the real release binary through the public CLI only:
      <code>wrec record start --target window:&lt;id&gt; --json</code>, one
      fresh isolated daemon per recording (own <code>WREC_HOME</code>, own
      socket, env overrides scrubbed). no bench mode, no test hooks — wrec
      cannot tell it is being measured.
    </p>

    <h3>3 · the file is the referee</h3>
    <p>
      after each run an AVAssetReader walks every frame of the
      <code>.mov</code> and reads the block numbers back. missing number =
      a displayed frame the recorder lost. from this fall out observed fps,
      capture completeness, duplicate frames, PTS gaps and ordering. the
      engine's own <code>frames/dropped</code> counters are recorded too —
      and if they disagree with the decoded truth by more than 5%, that is
      itself a failing gate.
    </p>

    <h3>4 · cost and latency on the side</h3>
    <p>
      while recording, <code>ps</code> samples cpu and rss of the daemon tree
      every 250 ms; start latency (command → first frame written) and finalize
      latency (duration elapsed → closed file) come from the timestamped
      <code>--json</code> job events.
    </p>

    <h3>5 · a/b, not vibes</h3>
    <p>
      for a release, candidate and reference binaries run interleaved —
      A B A B A B, three reps each per profile, same session — and gates
      compare paired medians. a regression fails only if the candidate is
      &gt;15% worse <em>and</em> beyond an absolute noise floor. thermal
      drift and background load hit both binaries equally, so they cancel.
    </p>

    <h3>6 · three verdicts, not two</h3>
    <p>
      on battery, under thermal pressure, under load, or when same-binary reps
      disagree beyond the noise floor, perf gates return
      <strong>inconclusive</strong> instead of pass/fail. correctness gates
      (codec, dimensions, PTS order, frame accounting) always stay hard —
      machine load cannot corrupt those.
    </p>

    <p class="muted">
      profiles: efficient-720p30-hevc · balanced-1080p30-hevc ·
      high-native60-hevc · balanced-1080p30-h264. run it:
      <code>bun run bench release --against &lt;previous wrec&gt;</code>
    </p>
  </section>`;

// NES-style stepped-corner border: a 5×5 SVG whose four plus-shaped pixels
// become the corners via border-image slicing.
const pxSvg = (hex: string) =>
  `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='5' height='5'%3E%3Cpath d='M2 1h1v1h-1zM1 2h1v1h-1zM3 2h1v1h-1zM2 3h1v1h-1z' fill='%23${hex}'/%3E%3C/svg%3E")`;

const shell = (active: "bench" | "how", body: string, font: string | null) => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>wrec bench${active === "how" ? " · how" : ""}</title>
<style>
  ${
    font
      ? `@font-face {
    font-family: "Departure Mono";
    src: url(data:font/otf;base64,${font}) format("opentype");
    font-display: swap;
  }`
      : ""
  }
  :root {
    color-scheme: dark;
    --plane: #0d0d0c; --surface: #161615;
    --ink: #e8e7e0; --ink-2: #b8b7ae; --muted: #898781;
    --line: #3a3936; --border: #8a8985; --border-dim: #4a4946;
    --good: #17c964; --warning: #fab219; --critical: #f24d4d;
    --px: ${pxSvg("8a8985")}; --px-dim: ${pxSvg("4a4946")};
  }
  @media (prefers-color-scheme: light) {
    :root {
      color-scheme: light;
      --plane: #f2f1ec; --surface: #fcfcfb;
      --ink: #16150f; --ink-2: #52514e; --muted: #898781;
      --line: #d9d8d0; --border: #52514e; --border-dim: #b1b0a8;
      --good: #0a8a0a; --warning: #9a6a00; --critical: #c22f2f;
      --px: ${pxSvg("52514e")}; --px-dim: ${pxSvg("b1b0a8")};
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 0 24px 64px; background: var(--plane); color: var(--ink);
    font-family: "Departure Mono", ui-monospace, monospace;
    font-size: 14px; line-height: 1.6;
    image-rendering: pixelated;
  }
  main { width: 100%; display: grid; gap: 20px; }
  nav {
    display: flex; align-items: center; gap: 14px;
    padding: 18px 0 16px; margin-bottom: 20px;
    border-bottom: 2px dashed var(--border-dim);
  }
  nav .brand { font-size: 18px; font-weight: 700; letter-spacing: 1px; margin-right: auto; }
  nav a {
    color: var(--ink); text-decoration: none; padding: 4px 14px;
    border: 4px solid transparent; border-image: var(--px-dim) 2 stretch;
  }
  nav a.active, nav a:hover {
    border-image: var(--px) 2 stretch;
    background: var(--ink); color: var(--plane);
  }
  h2 { font-size: 16px; margin: 0; letter-spacing: 0.5px; }
  h3 { font-size: 14px; margin: 0; font-weight: 700; }
  .px {
    border: 4px solid transparent;
    border-image: var(--px) 2 stretch;
    background: var(--surface);
  }
  .verdict { display: flex; gap: 18px; align-items: center; padding: 18px 20px; }
  .meta { margin: 6px 0 0; color: var(--ink-2); font-size: 12px; }
  .meta .dot { margin: 0 7px; color: var(--muted); }
  .card { padding: 16px 18px 18px; }
  .card-head { display: flex; justify-content: space-between; align-items: center; margin-bottom: 14px; gap: 12px; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; margin-bottom: 16px; }
  .tile { padding: 10px 12px; border-image: var(--px-dim) 2 stretch; }
  .tile-label { font-size: 11px; color: var(--ink-2); letter-spacing: 0.5px; }
  .tile-value { font-size: 22px; margin-top: 4px; }
  .tile-value .unit { font-size: 12px; color: var(--ink-2); margin-left: 2px; }
  .tile-sub { font-size: 11px; color: var(--muted); margin-top: 2px; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th {
    text-align: left; color: var(--muted); font-weight: 400; letter-spacing: 0.5px;
    padding: 6px 10px; border-bottom: 2px solid var(--line);
  }
  td { padding: 7px 10px; border-bottom: 2px solid var(--line); vertical-align: top; }
  tr:last-child td { border-bottom: none; }
  .num { font-variant-numeric: tabular-nums; text-align: right; }
  th.num { text-align: right; }
  .muted { color: var(--muted); }
  code { background: color-mix(in srgb, var(--ink) 8%, transparent); padding: 1px 5px; }
  .chip {
    display: inline-flex; align-items: center; gap: 6px;
    font-size: 11px; letter-spacing: 0.5px; padding: 2px 9px;
    border: 3px solid transparent; border-image: var(--px-dim) 2 stretch;
    white-space: nowrap;
  }
  .chip-good { color: var(--good); }
  .chip-critical { color: var(--critical); }
  .chip-warning { color: var(--warning); }
  .chip-muted { color: var(--muted); }
  .verdict-chip .chip { font-size: 15px; padding: 8px 16px; border-image: var(--px) 2 stretch; }
  .prose { max-width: 860px; }
  .prose p { margin: 10px 0 18px; color: var(--ink-2); }
  .prose h2 { margin-bottom: 6px; }
  .prose h3 { margin: 22px 0 4px; }
  .prose em { font-style: normal; color: var(--ink); }
  .prose strong { color: var(--warning); }
  .marker { display: inline-flex; gap: 3px; padding: 8px; background: #6c6b66; margin: 4px 0 10px; }
  .marker span { width: 18px; height: 40px; display: inline-block; }
  .marker .on { background: #fff; }
  .marker .off { background: #000; }
</style>
</head>
<body>
<nav>
  <span class="brand">wrec bench</span>
  <a href="index.html" class="${active === "bench" ? "active" : ""}">bench</a>
  <a href="how.html" class="${active === "how" ? "active" : ""}">how</a>
</nav>
<main>
${body}
</main>
</body>
</html>
`;

if (import.meta.main) {
  console.log(`report: ${await writeReport()}`);
}
