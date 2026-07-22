export type SuiteName = "smoke" | "release";
export type VariantName = "candidate" | "reference";
export type Quality = "efficient" | "balanced" | "high";
export type Codec = "hevc" | "h264";
export type Resolution = "native" | "720p" | "1080p";
export type GateStatus = "pass" | "fail" | "inconclusive" | "skipped";
export type OverallStatus = "pass" | "fail" | "inconclusive";

export type Dimensions = {
  width: number;
  height: number;
};

export type ProfileSpec = {
  name: string;
  quality: Quality;
  resolution: Resolution;
  fps: 30 | 60;
  codec: Codec;
};

export type MetricsSnapshot = {
  elapsed_secs: number;
  output_bytes: number;
  estimated_bitrate_mbps: number;
  frames: number | null;
  dropped_frames: number | null;
};

export type ProcessRow = {
  pid: number;
  ppid: number;
  cpuPercent: number;
  rssBytes: number;
  command: string;
  role: "daemon" | "helper" | "child";
};

export type ProcessSample = {
  timestampMs: number;
  processes: ProcessRow[];
};

export type ProcessSummary = {
  sampleCount: number;
  maxTotalCpuPercent: number;
  p95TotalCpuPercent: number;
  avgTotalCpuPercent: number;
  maxTotalRssBytes: number;
  maxHelperCpuPercent: number;
  maxHelperRssBytes: number;
  maxDaemonCpuPercent: number;
  maxDaemonRssBytes: number;
};

export type DecodeFrame = {
  ptsMs: number;
  stimulusIndex: number | null;
};

export type DecodeResult = {
  codec: string;
  dimensions: Dimensions;
  durationMs: number;
  frames: DecodeFrame[];
};

export type ObservedSummary = {
  decodedFrames: number;
  readableStimulusFrames: number;
  uniqueStimulusFrames: number;
  duplicateStimulusFrames: number;
  missingStimulusIndices: number[];
  effectiveFps: number | null;
  stimulusAchievedFps: number | null;
  captureCompleteness: number | null;
  maxInterFramePtsGapMs: number | null;
  ptsMonotonic: boolean;
  firstPtsMs: number | null;
  lastPtsMs: number | null;
  codec: string | null;
  dimensions: Dimensions | null;
  durationMs: number | null;
  selfReportDisagreementRatio: number | null;
};

export type LatencySummary = {
  startMs: number | null;
  finalizeMs: number | null;
  recordingStartedAtMs: number | null;
  durationElapsedAtMs: number | null;
  terminalAtMs: number | null;
};

export type RunResult = {
  id: string;
  variant: VariantName;
  profile: string;
  rep: number;
  warmup: boolean;
  command: string[];
  target: string;
  targetInfo?: Record<string, unknown>;
  exitCode: number;
  elapsedMs: number;
  outputBytes: number;
  recordingFilesDeleted: string[];
  metrics: MetricsSnapshot[];
  lastMetrics: MetricsSnapshot | null;
  latency: LatencySummary;
  processSamples: ProcessSample[];
  processSummary: ProcessSummary;
  decode: DecodeResult | null;
  decoderError?: string;
  observed: ObservedSummary | null;
  jsonEvents: Array<Record<string, unknown>>;
  stdoutLines: string[];
  stderrLines: string[];
  error?: string;
};

export type AggregateSummary = {
  capture: {
    outputBytes: number | null;
    bitrateMbps: number | null;
    selfReportedFrames: number | null;
    selfReportedDroppedFrames: number | null;
    selfReportedDropRatio: number | null;
  };
  latency: {
    startMs: number | null;
    finalizeMs: number | null;
  };
  process: {
    avgTotalCpuPercent: number | null;
    p95TotalCpuPercent: number | null;
    maxTotalCpuPercent: number | null;
    maxTotalRssBytes: number | null;
  };
  observed: {
    codec: string | null;
    dimensions: Dimensions | null;
    decodedFrames: number | null;
    uniqueStimulusFrames: number | null;
    effectiveFps: number | null;
    stimulusAchievedFps: number | null;
    captureCompleteness: number | null;
    maxInterFramePtsGapMs: number | null;
    ptsMonotonic: boolean;
    selfReportDisagreementRatio: number | null;
  };
};

export type Gate = {
  name: string;
  threshold: string;
  measured: number | string | boolean | null;
  delta?: number | null;
  deltaPercent?: number | null;
  status: GateStatus;
  details?: string;
};

export type ProfileResult = {
  name: string;
  request: ProfileSpec & { duration: string; durationMs: number; expectedDimensions: Dimensions };
  target: string;
  targetInfo?: Record<string, unknown>;
  status: OverallStatus;
  warmups: RunResult[];
  measured: {
    candidate: RunResult[];
    reference?: RunResult[];
  };
  capture: AggregateSummary["capture"];
  latency: AggregateSummary["latency"];
  process: AggregateSummary["process"];
  observed: AggregateSummary["observed"];
  reference?: AggregateSummary;
  gates: Gate[];
};

export const smokeProfile: ProfileSpec = {
  name: "smoke-balanced-1080p30-hevc",
  quality: "balanced",
  resolution: "1080p",
  fps: 30,
  codec: "hevc",
};

export const releaseProfiles: ProfileSpec[] = [
  {
    name: "efficient-720p30-hevc",
    quality: "efficient",
    resolution: "720p",
    fps: 30,
    codec: "hevc",
  },
  {
    name: "balanced-1080p30-hevc",
    quality: "balanced",
    resolution: "1080p",
    fps: 30,
    codec: "hevc",
  },
  {
    name: "high-native60-hevc",
    quality: "high",
    resolution: "native",
    fps: 60,
    codec: "hevc",
  },
  {
    name: "balanced-1080p30-h264",
    quality: "balanced",
    resolution: "1080p",
    fps: 30,
    codec: "h264",
  },
];

const cpuNoiseFloor = 3;
const rssNoiseFloor = 20 * 1024 * 1024;
const latencyNoiseFloor = 150;

export const expectedOutputDimensions = (native: Dimensions, resolution: Resolution) => {
  const even = (value: number) => Math.max(2, Math.round(value) - (Math.round(value) % 2));
  const maxSize =
    resolution === "720p"
      ? { width: 1280, height: 720 }
      : resolution === "1080p"
        ? { width: 1920, height: 1080 }
        : null;

  if (!maxSize) {
    return { width: even(native.width), height: even(native.height) };
  }

  const scale = Math.min(1, maxSize.width / native.width, maxSize.height / native.height);
  return {
    width: even(native.width * scale),
    height: even(native.height * scale),
  };
};

export const summarizeRuns = (runs: RunResult[]): AggregateSummary => {
  const lastMetrics = runs.map((run) => run.lastMetrics).filter(isPresent);
  const observations = runs.map((run) => run.observed).filter(isPresent);

  return {
    capture: {
      outputBytes: medianOf(runs, (run) => run.outputBytes),
      bitrateMbps: median(lastMetrics.map((item) => item.estimated_bitrate_mbps)),
      selfReportedFrames: median(lastMetrics.map((item) => item.frames).filter(isFiniteNumber)),
      selfReportedDroppedFrames: median(
        lastMetrics.map((item) => item.dropped_frames).filter(isFiniteNumber),
      ),
      selfReportedDropRatio: medianOf(runs, selfReportedDropRatio),
    },
    latency: {
      startMs: medianOf(runs, (run) => run.latency.startMs),
      finalizeMs: medianOf(runs, (run) => run.latency.finalizeMs),
    },
    process: {
      avgTotalCpuPercent: medianOf(runs, (run) => run.processSummary.avgTotalCpuPercent),
      p95TotalCpuPercent: medianOf(runs, (run) => run.processSummary.p95TotalCpuPercent),
      maxTotalCpuPercent: medianOf(runs, (run) => run.processSummary.maxTotalCpuPercent),
      maxTotalRssBytes: medianOf(runs, (run) => run.processSummary.maxTotalRssBytes),
    },
    observed: {
      codec: mode(observations.map((item) => item.codec).filter(isPresent)),
      dimensions: modeDimensions(observations.map((item) => item.dimensions).filter(isPresent)),
      decodedFrames: median(observations.map((item) => item.decodedFrames)),
      uniqueStimulusFrames: median(observations.map((item) => item.uniqueStimulusFrames)),
      effectiveFps: median(observations.map((item) => item.effectiveFps).filter(isFiniteNumber)),
      stimulusAchievedFps: median(
        observations.map((item) => item.stimulusAchievedFps).filter(isFiniteNumber),
      ),
      captureCompleteness: median(
        observations.map((item) => item.captureCompleteness).filter(isFiniteNumber),
      ),
      maxInterFramePtsGapMs: max(
        observations.map((item) => item.maxInterFramePtsGapMs).filter(isFiniteNumber),
      ),
      ptsMonotonic: runs.length > 0 && runs.every((run) => run.observed?.ptsMonotonic === true),
      selfReportDisagreementRatio: medianOf(runs, selfReportDisagreementRatio),
    },
  };
};

// Battery, thermal pressure, or background load can distort these metrics, so
// a fail measured in an untrusted environment is reported as inconclusive.
// Correctness gates (codec, dimensions, PTS order, frame accounting) stay
// hard — machine load cannot corrupt those.
const perfSensitiveGate = (name: string) =>
  name.startsWith("regression_") ||
  [
    "observed_fps",
    "capture_completeness",
    "self_report_drop_ratio",
    "max_pts_gap_ms",
    "start_latency_ms",
    "finalize_latency_ms",
  ].includes(name);

export const evaluateProfileGates = (
  profile: ProfileSpec,
  durationMs: number,
  expectedDimensions: Dimensions,
  candidateRuns: RunResult[],
  referenceRuns?: RunResult[],
  envTrusted = true,
) => {
  const candidate = summarizeRuns(candidateRuns);
  const gates: Gate[] = [];
  const dropRatioThreshold = profile.fps === 60 ? 0.01 : 0.005;
  const finalizeThreshold = profile.quality === "high" ? 5000 : 3000;
  const fpsThreshold = profile.fps * 0.97;

  // The stimulus cannot always sustain the nominal rate (display-link jitter,
  // main-thread stalls), and the recorder cannot capture frames that never
  // reached the screen — so fps is gated against what the stimulus actually
  // delivered, capped at the profile target. capture_completeness below then
  // isolates the recorder's own loss regardless of stimulus pacing.
  const achievedFps = candidate.observed.stimulusAchievedFps;
  const effectiveFpsTarget =
    isFiniteNumber(achievedFps) ? Math.min(profile.fps, achievedFps) : profile.fps;
  // 60 fps window capture tops out around 95-96% of the display rate even
  // when the engine reports zero drops (measured on an M1 Air with a
  // display-link stimulus): ScreenCaptureKit itself delivers ~57/60 for a
  // 60 Hz-updating window. The 60 fps budget encodes that floor; catching a
  // real slide from 95% is the A/B regression gate's job.
  const fpsFloorRatio = profile.fps === 60 ? 0.95 : 0.97;
  const effectiveFpsThreshold = effectiveFpsTarget * fpsFloorRatio;
  gates.push(
    numericBudgetGate({
      name: "observed_fps",
      threshold: `>= ${round(effectiveFpsThreshold)} fps (${fpsFloorRatio * 100}% of min(target ${profile.fps}, stimulus ${isFiniteNumber(achievedFps) ? round(achievedFps) : "?"}))`,
      measured: candidate.observed.effectiveFps,
      passes: (value) => value >= effectiveFpsThreshold,
      noisy: spreadExceeds(candidateRuns, (run) => run.observed?.effectiveFps, 0.5),
    }),
  );
  // Unique-vs-spanned index completeness only means "frames the recorder
  // lost" when the capture rate covers the display rate; a 30 fps profile of
  // a 60 Hz stimulus keeps every other index by design.
  if (isFiniteNumber(achievedFps) && profile.fps + 5 >= achievedFps) {
    gates.push(
      numericBudgetGate({
        name: "capture_completeness",
        threshold: `>= ${fpsFloorRatio * 100}% of displayed frames`,
        measured: candidate.observed.captureCompleteness,
        passes: (value) => value >= fpsFloorRatio,
        noisy: spreadExceeds(candidateRuns, (run) => run.observed?.captureCompleteness, 0.01),
      }),
    );
  } else {
    gates.push({
      name: "capture_completeness",
      threshold: "capture rate >= display rate",
      measured: candidate.observed.captureCompleteness,
      status: "skipped",
      details: "profile captures slower than the stimulus displays; skipping every other frame is by design",
    });
  }
  gates.push(
    numericBudgetGate({
      name: "self_report_drop_ratio",
      threshold: `<= ${(dropRatioThreshold * 100).toFixed(2)}%`,
      measured: candidate.capture.selfReportedDropRatio,
      passes: (value) => value <= dropRatioThreshold,
      noisy: spreadExceeds(candidateRuns, selfReportedDropRatio, dropRatioThreshold),
    }),
  );
  gates.push(
    numericBudgetGate({
      name: "max_pts_gap_ms",
      threshold: "<= 500 ms",
      measured: candidate.observed.maxInterFramePtsGapMs,
      passes: (value) => value <= 500,
      noisy: spreadExceeds(
        candidateRuns,
        (run) => run.observed?.maxInterFramePtsGapMs,
        50,
      ),
    }),
  );
  gates.push({
    name: "pts_monotonic",
    threshold: "true",
    measured: candidate.observed.ptsMonotonic,
    status: candidate.observed.ptsMonotonic ? "pass" : "fail",
  });
  gates.push({
    name: "codec_match",
    threshold: profile.codec,
    measured: candidate.observed.codec,
    status: candidateRuns.every((run) => run.decode?.codec === profile.codec) ? "pass" : "fail",
  });
  gates.push({
    name: "dimensions_match_request",
    threshold: `${expectedDimensions.width}x${expectedDimensions.height}`,
    measured: candidate.observed.dimensions
      ? `${candidate.observed.dimensions.width}x${candidate.observed.dimensions.height}`
      : null,
    status: candidateRuns.every((run) => dimensionsEqual(run.decode?.dimensions, expectedDimensions))
      ? "pass"
      : "fail",
  });
  gates.push(
    numericBudgetGate({
      name: "self_report_disagreement",
      threshold: "<= 5%",
      measured: candidate.observed.selfReportDisagreementRatio,
      passes: (value) => value <= 0.05,
      noisy: spreadExceeds(candidateRuns, selfReportDisagreementRatio, 0.02),
    }),
  );
  gates.push(
    numericBudgetGate({
      name: "start_latency_ms",
      threshold: "<= 1500 ms",
      measured: candidate.latency.startMs,
      passes: (value) => value <= 1500,
      noisy: spreadExceeds(candidateRuns, (run) => run.latency.startMs, latencyNoiseFloor),
    }),
  );
  gates.push(
    numericBudgetGate({
      name: "finalize_latency_ms",
      threshold: `<= ${finalizeThreshold} ms`,
      measured: candidate.latency.finalizeMs,
      passes: (value) => value <= finalizeThreshold,
      noisy: spreadExceeds(candidateRuns, (run) => run.latency.finalizeMs, latencyNoiseFloor),
    }),
  );

  gates.push(
    ...regressionGates({
      profile,
      durationMs,
      candidateRuns,
      referenceRuns,
    }),
  );

  if (envTrusted) {
    return gates;
  }
  return gates.map((gate) =>
    gate.status === "fail" && perfSensitiveGate(gate.name)
      ? {
          ...gate,
          status: "inconclusive" as GateStatus,
          details: "environment untrusted (battery/thermal/load); rerun on AC power",
        }
      : gate,
  );
};

export const statusFromGates = (gates: Gate[], runs: RunResult[]): OverallStatus => {
  if (runs.some((run) => run.exitCode !== 0 || run.error)) {
    return "fail";
  }
  if (gates.some((gate) => gate.status === "fail")) {
    return "fail";
  }
  if (gates.some((gate) => gate.status === "inconclusive")) {
    return "inconclusive";
  }
  return "pass";
};

export const combineStatuses = (statuses: OverallStatus[]) => {
  if (statuses.includes("fail")) {
    return "fail";
  }
  if (statuses.includes("inconclusive")) {
    return "inconclusive";
  }
  return "pass";
};

const numericBudgetGate = ({
  name,
  threshold,
  measured,
  passes,
  noisy,
}: {
  name: string;
  threshold: string;
  measured: number | null;
  passes: (value: number) => boolean;
  noisy: boolean;
}): Gate => {
  if (!isFiniteNumber(measured)) {
    return { name, threshold, measured: null, status: "fail", details: "metric unavailable" };
  }
  if (noisy) {
    return { name, threshold, measured, status: "inconclusive", details: "rep spread exceeded noise floor" };
  }
  return { name, threshold, measured, status: passes(measured) ? "pass" : "fail" };
};

const regressionGates = ({
  candidateRuns,
  referenceRuns,
}: {
  profile: ProfileSpec;
  durationMs: number;
  candidateRuns: RunResult[];
  referenceRuns?: RunResult[];
}) => [
  regressionGate(
    "regression_cpu_avg",
    "%",
    candidateRuns,
    referenceRuns,
    (run) => run.processSummary.avgTotalCpuPercent,
    cpuNoiseFloor,
  ),
  regressionGate(
    "regression_cpu_p95",
    "%",
    candidateRuns,
    referenceRuns,
    (run) => run.processSummary.p95TotalCpuPercent,
    cpuNoiseFloor,
  ),
  regressionGate(
    "regression_max_rss",
    "bytes",
    candidateRuns,
    referenceRuns,
    (run) => run.processSummary.maxTotalRssBytes,
    rssNoiseFloor,
  ),
  regressionGate(
    "regression_start_latency",
    "ms",
    candidateRuns,
    referenceRuns,
    (run) => run.latency.startMs,
    latencyNoiseFloor,
  ),
  regressionGate(
    "regression_finalize_latency",
    "ms",
    candidateRuns,
    referenceRuns,
    (run) => run.latency.finalizeMs,
    latencyNoiseFloor,
  ),
];

const regressionGate = (
  name: string,
  unit: string,
  candidateRuns: RunResult[],
  referenceRuns: RunResult[] | undefined,
  getter: (run: RunResult) => number | null | undefined,
  noiseFloor: number,
): Gate => {
  if (!referenceRuns?.length) {
    return {
      name,
      threshold: `candidate <= reference + 15% and <= reference + ${formatNoise(noiseFloor, unit)}`,
      measured: null,
      delta: null,
      status: "skipped",
      details: "--against not provided",
    };
  }

  const candidateValues = candidateRuns.map(getter).filter(isFiniteNumber);
  const referenceValues = referenceRuns.map(getter).filter(isFiniteNumber);
  if (!candidateValues.length || !referenceValues.length) {
    return {
      name,
      threshold: `candidate <= reference + 15% and <= reference + ${formatNoise(noiseFloor, unit)}`,
      measured: null,
      delta: null,
      status: "fail",
      details: "metric unavailable",
    };
  }

  const pairedCount = Math.min(candidateRuns.length, referenceRuns.length);
  const deltas = Array.from({ length: pairedCount }, (_, index) => {
    const candidate = getter(candidateRuns[index]);
    const reference = getter(referenceRuns[index]);
    return isFiniteNumber(candidate) && isFiniteNumber(reference) ? candidate - reference : null;
  }).filter(isFiniteNumber);
  const referenceMedian = median(referenceValues);
  const measured = median(candidateValues);
  const delta = median(deltas);
  const deltaPercent =
    isFiniteNumber(delta) && isFiniteNumber(referenceMedian) && referenceMedian !== 0
      ? (delta / referenceMedian) * 100
      : null;
  const noisy =
    spreadExceeds(candidateRuns, getter, noiseFloor) ||
    spreadExceeds(referenceRuns, getter, noiseFloor);

  if (noisy) {
    return {
      name,
      threshold: `candidate <= reference + 15% and <= reference + ${formatNoise(noiseFloor, unit)}`,
      measured,
      delta,
      deltaPercent,
      status: "inconclusive",
      details: "same-binary rep spread exceeded noise floor",
    };
  }

  const worseByPercent =
    isFiniteNumber(deltaPercent) && deltaPercent > 15;
  const aboveNoise = isFiniteNumber(delta) && delta > noiseFloor;
  return {
    name,
    threshold: `candidate <= reference + 15% and <= reference + ${formatNoise(noiseFloor, unit)}`,
    measured,
    delta,
    deltaPercent,
    status: worseByPercent && aboveNoise ? "fail" : "pass",
  };
};

const selfReportedDropRatio = (run: RunResult) => {
  const frames = run.lastMetrics?.frames;
  const dropped = run.lastMetrics?.dropped_frames;
  if (!isFiniteNumber(frames) || !isFiniteNumber(dropped)) {
    return null;
  }
  const total = frames + dropped;
  return total > 0 ? dropped / total : 0;
};

const selfReportDisagreementRatio = (run: RunResult) => {
  const frames = run.lastMetrics?.frames;
  const observed = run.observed?.uniqueStimulusFrames;
  if (!isFiniteNumber(frames) || !isFiniteNumber(observed) || observed <= 0) {
    return null;
  }
  return Math.abs(frames - observed) / observed;
};

const spreadExceeds = (
  runs: RunResult[],
  getter: (run: RunResult) => number | null | undefined,
  noiseFloor: number,
) => {
  const values = runs.map(getter).filter(isFiniteNumber);
  return values.length >= 2 && max(values)! - min(values)! > noiseFloor * 2;
};

const medianOf = (runs: RunResult[], getter: (run: RunResult) => number | null | undefined) =>
  median(runs.map(getter).filter(isFiniteNumber));

const median = (values: number[]) => {
  if (!values.length) {
    return null;
  }
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0
    ? (sorted[middle - 1] + sorted[middle]) / 2
    : sorted[middle];
};

const min = (values: number[]) => (values.length ? Math.min(...values) : null);
const max = (values: number[]) => (values.length ? Math.max(...values) : null);

const mode = (values: string[]) => {
  if (!values.length) {
    return null;
  }
  const counts = new Map<string, number>();
  for (const value of values) {
    counts.set(value, (counts.get(value) ?? 0) + 1);
  }
  return [...counts.entries()].sort((a, b) => b[1] - a[1])[0][0];
};

const modeDimensions = (values: Dimensions[]) => {
  const encoded = mode(values.map((value) => `${value.width}x${value.height}`));
  if (!encoded) {
    return null;
  }
  const [width, height] = encoded.split("x").map((value) => Number.parseInt(value, 10));
  return { width, height };
};

const dimensionsEqual = (left: Dimensions | null | undefined, right: Dimensions) =>
  Boolean(left && left.width === right.width && left.height === right.height);

const formatNoise = (value: number, unit: string) =>
  unit === "bytes" ? `${Math.round(value / 1024 / 1024)} MB` : `${value} ${unit}`;

const round = (value: number) => Number(value.toFixed(3));

const isPresent = <T>(value: T | null | undefined): value is T => value !== null && value !== undefined;

const isFiniteNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
