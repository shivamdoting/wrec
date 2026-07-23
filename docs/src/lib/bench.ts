import { readFile, readdir } from "node:fs/promises";
import path from "node:path";

// Build-time loader for the committed bench summaries in benchmarks/results/
// (schema wrec.perf/v1, written by `bun run bench`). The site prerenders
// them; a new bench appears on the next deploy.

export type Gate = {
  name: string;
  threshold: string;
  measured: number | string | boolean | null;
  delta?: number | null;
  deltaPercent?: number | null;
  status: "pass" | "fail" | "inconclusive" | "skipped";
  details?: string;
};

export type BenchProfile = {
  name: string;
  status: "pass" | "fail" | "inconclusive";
  latency: { startMs: number | null; finalizeMs: number | null };
  process: {
    avgTotalCpuPercent: number | null;
    p95TotalCpuPercent: number | null;
    maxTotalRssBytes: number | null;
  };
  observed: {
    effectiveFps: number | null;
    stimulusAchievedFps: number | null;
    captureCompleteness: number | null;
  } | null;
  gates: Gate[];
};

export type BenchSummary = {
  schema: string;
  id: string;
  generatedAt: string;
  suite: string;
  status: "pass" | "fail" | "inconclusive";
  duration: string;
  binaries: { candidate: string; reference?: string };
  git: { commit: string; dirty: boolean };
  environment: {
    macos?: { productVersion: string };
    chip?: string;
    guards?: Array<{ name: string; status: string }>;
  };
  profiles: BenchProfile[];
};

const candidateDirs = () => [
  path.resolve(process.cwd(), "..", "benchmarks", "results"),
  path.resolve(process.cwd(), "benchmarks", "results"),
];

export const loadSummaries = async (): Promise<BenchSummary[]> => {
  for (const dir of candidateDirs()) {
    let names: string[];
    try {
      names = (await readdir(dir)).filter((name) => name.endsWith(".json"));
    } catch {
      continue;
    }
    const parsed = await Promise.all(
      names.map(async (name) => {
        try {
          return JSON.parse(
            await readFile(path.join(dir, name), "utf8"),
          ) as BenchSummary;
        } catch {
          return null;
        }
      }),
    );
    return parsed
      .filter(
        (item): item is BenchSummary =>
          item !== null && typeof item?.id === "string",
      )
      .sort((a, b) => b.id.localeCompare(a.id));
  }
  return [];
};

export const fmt = (value: unknown, digits = 2): string => {
  if (value === null || value === undefined) return "–";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    if (!Number.isFinite(value)) return "–";
    return Number.isInteger(value) ? String(value) : value.toFixed(digits);
  }
  return String(value);
};

export const fmtBytes = (bytes: number | null | undefined): string => {
  if (!bytes && bytes !== 0) return "–";
  if (bytes >= 1024 * 1024 * 1024)
    return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
};

export const fmtPercent = (ratio: number | null | undefined): string =>
  ratio === null || ratio === undefined ? "–" : `${(ratio * 100).toFixed(1)}%`;

const benchmarkDateFormat = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
  hour12: true,
  timeZone: "UTC",
  timeZoneName: "short",
});

export const fmtDate = (value: string): string => {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : benchmarkDateFormat.format(date);
};

export const gateMeasured = (gate: Gate): string =>
  gate.name.includes("rss") && typeof gate.measured === "number"
    ? fmtBytes(gate.measured)
    : fmt(gate.measured);

export const gateDelta = (gate: Gate): string => {
  if (gate.delta === null || gate.delta === undefined) return "–";
  const base = gate.name.includes("rss")
    ? fmtBytes(gate.delta)
    : fmt(gate.delta);
  if (gate.deltaPercent === null || gate.deltaPercent === undefined)
    return base;
  return `${base} (${gate.deltaPercent >= 0 ? "+" : ""}${gate.deltaPercent.toFixed(1)}%)`;
};

export const statusMeta = (
  status: string,
): { icon: string; word: string; tone: string } =>
  ({
    pass: { icon: "✓", word: "PASS", tone: "good" },
    fail: { icon: "✕", word: "FAIL", tone: "critical" },
    inconclusive: { icon: "◐", word: "INCONCLUSIVE", tone: "warning" },
  })[status] ?? { icon: "–", word: "SKIPPED", tone: "muted" };
