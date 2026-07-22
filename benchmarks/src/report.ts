import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import type { Gate, OverallStatus, ProfileResult, SuiteName } from "./gates";

type BenchmarkResult = {
  schema: "wrec.perf/v1";
  id: string;
  generatedAt: string;
  suite: SuiteName;
  status: OverallStatus;
  duration: string;
  sampleIntervalMs: number;
  binaries: {
    candidate: string;
    reference?: string;
  };
  git: {
    branch: string;
    commit: string;
    dirty: boolean;
  };
  machine: {
    arch: string;
    cpu: string;
    cpuCount: number;
    totalMemoryBytes: number;
  };
  environment: {
    acPower?: {
      stdout: string;
    };
    macos?: {
      productVersion: string;
      buildVersion: string;
    };
    chip?: string;
    loadAverage?: number[];
    guards?: Array<{
      name: string;
      threshold: string;
      measured: string | number | boolean | null;
      status: string;
      details?: string;
    }>;
  };
  stimulus: {
    title: string;
    pixels: {
      width: number;
      height: number;
    };
    scale: number;
  };
  profiles: ProfileResult[];
};

const root = path.resolve(import.meta.dir, "..");
const runsDir = path.join(root, "runs");

export const writeReport = async () => {
  await mkdir(root, { recursive: true });
  const runs = await readRuns();
  await writeFile(path.join(root, "index.html"), renderHtml(runs));
};

const readRuns = async () => {
  const files = await readdir(runsDir).catch(() => []);
  const runs: Array<BenchmarkResult & { resultPath: string }> = [];

  for (const file of files.filter((item) => item.endsWith(".json")).sort().reverse()) {
    const fullPath = path.join(runsDir, file);
    try {
      const run = JSON.parse(await readFile(fullPath, "utf8")) as BenchmarkResult;
      if (run.schema === "wrec.perf/v1") {
        runs.push({ ...run, resultPath: path.relative(root, fullPath) });
      }
    } catch {
      // Ignore stale or partial local run files.
    }
  }

  return runs;
};

const renderHtml = (runs: Array<BenchmarkResult & { resultPath: string }>) => {
  const latest = runs[0];
  const profileRows = latest?.profiles.map(renderProfileRow).join("\n") ?? "";
  const gateRows = latest?.profiles.flatMap(renderGateRows).join("\n") ?? "";
  const envRows = latest?.environment.guards?.map(renderEnvironmentRow).join("\n") ?? "";
  const historyRows = runs.map(renderHistoryRow).join("\n");

  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>wrec performance benchmarks</title>
  <style>
    :root {
      color-scheme: light;
      font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      --background: #f7f7f4;
      --foreground: #202124;
      --muted: #5d6269;
      --surface: #ffffff;
      --surface-alt: #ececea;
      --border: #d9d9d2;
      --row-border: #ededeb;
      --pass: #17633a;
      --fail: #b42318;
      --inconclusive: #8a5a00;
      --skipped: #69707a;
      background: var(--background);
      color: var(--foreground);
    }

    :root[data-theme="dark"] {
      color-scheme: dark;
      --background: #151515;
      --foreground: #eeeeee;
      --muted: #a6abb2;
      --surface: #202124;
      --surface-alt: #2b2c2f;
      --border: #3b3d41;
      --row-border: #303236;
      --pass: #58c98b;
      --fail: #ff7b72;
      --inconclusive: #f0b84d;
      --skipped: #9aa1ab;
    }

    html,
    body {
      margin: 0;
      overscroll-behavior: none;
    }

    body {
      padding: 32px;
      background: var(--background);
      color: var(--foreground);
    }

    main {
      max-width: 1320px;
      margin: 0 auto;
    }

    h1,
    h2 {
      margin: 0;
      letter-spacing: 0;
    }

    h1 {
      font-size: 28px;
      font-weight: 700;
    }

    h2 {
      margin-top: 32px;
      margin-bottom: 12px;
      font-size: 18px;
      font-weight: 700;
    }

    .topbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 20px;
    }

    .theme-toggle {
      appearance: none;
      border: 0;
      padding: 0;
      background: transparent;
      color: var(--foreground);
      font: inherit;
      font-size: 15px;
      font-weight: 650;
      cursor: pointer;
    }

    .meta {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
      gap: 10px 18px;
      margin: 20px 0 28px;
      color: var(--muted);
      font-size: 14px;
    }

    .meta div {
      min-width: 0;
      overflow-wrap: anywhere;
    }

    table {
      width: 100%;
      border-collapse: collapse;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 8px;
      overflow: hidden;
    }

    th,
    td {
      padding: 10px 12px;
      border-bottom: 1px solid var(--row-border);
      text-align: left;
      font-size: 13px;
      vertical-align: top;
      white-space: nowrap;
    }

    td.wrap,
    th.wrap {
      white-space: normal;
      overflow-wrap: anywhere;
    }

    th {
      background: var(--surface-alt);
      color: var(--muted);
      font-weight: 700;
    }

    tr:last-child td {
      border-bottom: 0;
    }

    .status-pass {
      color: var(--pass);
      font-weight: 700;
    }

    .status-fail {
      color: var(--fail);
      font-weight: 700;
    }

    .status-inconclusive {
      color: var(--inconclusive);
      font-weight: 700;
    }

    .status-skipped {
      color: var(--skipped);
      font-weight: 700;
    }

    code {
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      font-size: 12px;
    }
  </style>
</head>
<body>
  <main>
    <div class="topbar">
      <h1>wrec performance benchmarks</h1>
      <button type="button" class="theme-toggle" data-theme-toggle>dark</button>
    </div>
    ${latest ? renderMeta(latest) : "<p>No wrec.perf/v1 benchmark runs yet.</p>"}
    <h2>Latest Profiles</h2>
    <table>
      <thead>
        <tr>
          <th>Status</th>
          <th>Profile</th>
          <th>Observed FPS</th>
          <th>CPU avg / p95</th>
          <th>Max RSS</th>
          <th>Start / Finalize</th>
          <th class="wrap">A/B Deltas</th>
        </tr>
      </thead>
      <tbody>${profileRows}</tbody>
    </table>
    <h2>Gates</h2>
    <table>
      <thead>
        <tr>
          <th>Status</th>
          <th>Profile</th>
          <th>Gate</th>
          <th>Measured</th>
          <th>Threshold</th>
          <th>Delta</th>
          <th class="wrap">Details</th>
        </tr>
      </thead>
      <tbody>${gateRows || `<tr><td colspan="7">No gates for this suite.</td></tr>`}</tbody>
    </table>
    ${
      envRows
        ? `<h2>Environment Guards</h2>
    <table>
      <thead>
        <tr>
          <th>Status</th>
          <th>Guard</th>
          <th>Measured</th>
          <th>Threshold</th>
        </tr>
      </thead>
      <tbody>${envRows}</tbody>
    </table>`
        : ""
    }
    <h2>Run History</h2>
    <table>
      <thead>
        <tr>
          <th>Status</th>
          <th>Generated</th>
          <th>Suite</th>
          <th>Duration</th>
          <th>Git</th>
          <th>Result</th>
        </tr>
      </thead>
      <tbody>${historyRows}</tbody>
    </table>
  </main>
  <script>
    const root = document.documentElement;
    const toggle = document.querySelector("[data-theme-toggle]");
    const setTheme = (theme) => {
      root.dataset.theme = theme;
      localStorage.setItem("wrec-benchmark-theme", theme);
      const nextTheme = theme === "dark" ? "light" : "dark";
      toggle.textContent = nextTheme;
      toggle.setAttribute("aria-label", "Switch to " + nextTheme + " mode");
    };
    const savedTheme = localStorage.getItem("wrec-benchmark-theme");
    setTheme(savedTheme === "dark" ? "dark" : "light");
    toggle.addEventListener("click", () => setTheme(root.dataset.theme === "dark" ? "light" : "dark"));
  </script>
</body>
</html>`;
};

const renderMeta = (run: BenchmarkResult) => `<section class="meta">
  <div><strong>Status</strong><br>${statusLabel(run.status)}</div>
  <div><strong>Generated</strong><br><time datetime="${escapeHtml(run.generatedAt)}">${escapeHtml(formatDate(run.generatedAt))}</time></div>
  <div><strong>Suite</strong><br>${escapeHtml(run.suite)}</div>
  <div><strong>Duration</strong><br>${escapeHtml(run.duration)}</div>
  <div><strong>Stimulus</strong><br>${escapeHtml(`${run.stimulus.pixels.width}x${run.stimulus.pixels.height} @ ${run.stimulus.scale.toFixed(2)}x`)}</div>
  <div><strong>Machine</strong><br>${escapeHtml(`${run.machine.arch} ${run.environment.chip || run.machine.cpu}`)}</div>
  <div><strong>macOS</strong><br>${escapeHtml(`${run.environment.macos?.productVersion ?? ""} ${run.environment.macos?.buildVersion ?? ""}`.trim())}</div>
  <div><strong>Git</strong><br>${escapeHtml(shortSha(run.git.commit))}${run.git.dirty ? " dirty" : ""}</div>
</section>`;

const renderProfileRow = (profile: ProfileResult) => `<tr>
  <td>${statusLabel(profile.status)}</td>
  <td>${escapeHtml(profile.name)}<br><code>${escapeHtml(`${profile.request.quality} ${profile.request.resolution} ${profile.request.fps}fps ${profile.request.codec}`)}</code></td>
  <td>${formatNumber(profile.observed.effectiveFps)}</td>
  <td>${formatNumber(profile.process.avgTotalCpuPercent)}% / ${formatNumber(profile.process.p95TotalCpuPercent)}%</td>
  <td>${formatBytes(profile.process.maxTotalRssBytes)}</td>
  <td>${formatMs(profile.latency.startMs)} / ${formatMs(profile.latency.finalizeMs)}</td>
  <td class="wrap">${renderRegressionSummary(profile.gates)}</td>
</tr>`;

const renderGateRows = (profile: ProfileResult) =>
  profile.gates.map((gate) => `<tr>
  <td>${statusLabel(gate.status)}</td>
  <td>${escapeHtml(profile.name)}</td>
  <td>${escapeHtml(gate.name)}</td>
  <td>${escapeHtml(formatMeasured(gate.measured))}</td>
  <td>${escapeHtml(gate.threshold)}</td>
  <td>${escapeHtml(formatDelta(gate))}</td>
  <td class="wrap">${escapeHtml(gate.details ?? "")}</td>
</tr>`);

const renderEnvironmentRow = (guard: {
  name: string;
  threshold: string;
  measured: string | number | boolean | null;
  status: string;
}) => `<tr>
  <td>${statusLabel(guard.status)}</td>
  <td>${escapeHtml(guard.name)}</td>
  <td class="wrap">${escapeHtml(formatMeasured(guard.measured))}</td>
  <td>${escapeHtml(guard.threshold)}</td>
</tr>`;

const renderHistoryRow = (run: BenchmarkResult & { resultPath: string }) => `<tr>
  <td>${statusLabel(run.status)}</td>
  <td><time datetime="${escapeHtml(run.generatedAt)}">${escapeHtml(formatDate(run.generatedAt))}</time></td>
  <td>${escapeHtml(run.suite)}</td>
  <td>${escapeHtml(run.duration)}</td>
  <td>${escapeHtml(shortSha(run.git.commit))}${run.git.dirty ? " dirty" : ""}</td>
  <td><a href="${escapeHtml(run.resultPath)}">${escapeHtml(run.id)}</a></td>
</tr>`;

const renderRegressionSummary = (gates: Gate[]) => {
  const deltas = gates
    .filter((gate) => gate.name.startsWith("regression_") && gate.status !== "skipped")
    .map((gate) => `${gate.name.replace("regression_", "")}: ${formatDelta(gate)}`);
  return deltas.length ? escapeHtml(deltas.join(", ")) : "-";
};

const statusLabel = (status: string) =>
  `<span class="status-${escapeHtml(status)}">${escapeHtml(status)}</span>`;

const formatMeasured = (value: string | number | boolean | null) => {
  if (typeof value === "number") {
    return Number.isInteger(value) ? String(value) : value.toFixed(3);
  }
  if (typeof value === "boolean") {
    return value ? "true" : "false";
  }
  return value ?? "-";
};

const formatDelta = (gate: Gate) => {
  if (typeof gate.delta !== "number" || !Number.isFinite(gate.delta)) {
    return "-";
  }
  const delta = gate.name.includes("rss")
    ? formatBytes(gate.delta)
    : gate.delta.toFixed(2);
  const percent =
    typeof gate.deltaPercent === "number" && Number.isFinite(gate.deltaPercent)
      ? ` (${gate.deltaPercent.toFixed(1)}%)`
      : "";
  return `${gate.delta >= 0 ? "+" : ""}${delta}${percent}`;
};

const formatNumber = (value: number | null | undefined) =>
  typeof value === "number" && Number.isFinite(value) ? value.toFixed(2) : "-";

const formatMs = (value: number | null | undefined) =>
  typeof value === "number" && Number.isFinite(value) ? `${Math.round(value)} ms` : "-";

const formatDate = (value: string) =>
  new Intl.DateTimeFormat("en", {
    dateStyle: "medium",
    timeStyle: "medium",
  }).format(new Date(value));

const formatBytes = (bytes: number | null | undefined) => {
  if (typeof bytes !== "number" || !Number.isFinite(bytes)) {
    return "-";
  }
  const sign = bytes < 0 ? "-" : "";
  const value = Math.abs(bytes);
  if (value >= 1_000_000_000) {
    return `${sign}${(value / 1_000_000_000).toFixed(2)} GB`;
  }
  if (value >= 1_000_000) {
    return `${sign}${(value / 1_000_000).toFixed(2)} MB`;
  }
  if (value >= 1_000) {
    return `${sign}${(value / 1_000).toFixed(2)} KB`;
  }
  return `${sign}${value} B`;
};

const shortSha = (sha: string) => sha.slice(0, 7);

const escapeHtml = (value: string) =>
  value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");

if (import.meta.main) {
  await writeReport();
  console.log(`report: ${path.join(root, "index.html")}`);
}
