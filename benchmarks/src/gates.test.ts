import { describe, expect, test } from "bun:test";
import {
  evaluateProfileGates,
  type ProfileSpec,
  type RunResult,
} from "./gates";

const profile: ProfileSpec = {
  name: "balanced-1080p30-hevc",
  quality: "balanced",
  resolution: "1080p",
  fps: 30,
  codec: "hevc",
};

const run = (rep: number, startMs: number, gapMs: number): RunResult => ({
  id: `candidate-${rep}`,
  variant: "candidate",
  profile: profile.name,
  rep,
  warmup: false,
  command: [],
  target: "window:1",
  exitCode: 0,
  elapsedMs: 15_000,
  outputBytes: 1,
  recordingFilesDeleted: [],
  metrics: [],
  lastMetrics: {
    elapsed_secs: 15,
    output_bytes: 1,
    estimated_bitrate_mbps: 1,
    frames: 440,
    dropped_frames: 0,
  },
  latency: {
    startMs,
    finalizeMs: 100,
    recordingStartedAtMs: 0,
    durationElapsedAtMs: 0,
    terminalAtMs: 0,
  },
  processSamples: [],
  processSummary: {
    sampleCount: 1,
    maxTotalCpuPercent: 5,
    p95TotalCpuPercent: 5,
    avgTotalCpuPercent: 5,
    maxTotalRssBytes: 40 * 1024 * 1024,
    maxHelperCpuPercent: 5,
    maxHelperRssBytes: 30 * 1024 * 1024,
    maxDaemonCpuPercent: 0,
    maxDaemonRssBytes: 10 * 1024 * 1024,
  },
  decode: {
    codec: "hevc",
    dimensions: { width: 1920, height: 1080 },
    durationMs: 15_000,
    frames: Array.from({ length: 440 }, (_, index) => ({
      ptsMs: index * (1000 / 30),
      stimulusIndex: index,
    })),
  },
  observed: {
    decodedFrames: 440,
    readableStimulusFrames: 400,
    uniqueStimulusFrames: 400,
    duplicateStimulusFrames: 0,
    missingStimulusIndices: [],
    effectiveFps: 29.4,
    stimulusAchievedFps: 60,
    captureCompleteness: 0.5,
    maxInterFramePtsGapMs: gapMs,
    ptsMonotonic: true,
    firstPtsMs: 0,
    lastPtsMs: 15_000,
    codec: "hevc",
    dimensions: { width: 1920, height: 1080 },
    durationMs: 15_000,
    selfReportDisagreementRatio: 0,
  },
  jsonEvents: [],
  stdoutLines: [],
  stderrLines: [],
});

describe("release gate hardening", () => {
  test("uses decoded frames for accounting and ignores isolated outliers", () => {
    const runs = [
      run(1, 500, 100),
      run(2, 510, 110),
      run(3, 520, 120),
      run(4, 4_000, 900),
      run(5, 530, 130),
    ];
    const gates = evaluateProfileGates(
      profile,
      15_000,
      { width: 1920, height: 1080 },
      runs,
    );
    const status = (name: string) =>
      gates.find((gate) => gate.name === name)?.status;

    expect(status("self_report_disagreement")).toBe("pass");
    expect(status("start_latency_ms")).toBe("pass");
    expect(status("max_pts_gap_ms")).toBe("pass");
  });

  test("calibrates native 60 fps while retaining an A/B regression gate", () => {
    const nativeProfile: ProfileSpec = {
      name: "high-native60-hevc",
      quality: "high",
      resolution: "native",
      fps: 60,
      codec: "hevc",
    };
    const nativeRun = (
      rep: number,
      variant: "candidate" | "reference",
      effectiveFps: number,
    ) => {
      const base = run(rep, 1_500, 200);
      return {
        ...base,
        id: `${variant}-${rep}`,
        variant,
        profile: nativeProfile.name,
        decode: {
          ...base.decode!,
          dimensions: { width: 2560, height: 1440 },
        },
        observed: {
          ...base.observed!,
          effectiveFps,
          stimulusAchievedFps: 58,
          captureCompleteness: effectiveFps / 58,
          dimensions: { width: 2560, height: 1440 },
        },
      } satisfies RunResult;
    };
    const candidate = [52, 52.5, 53].map((fps, index) =>
      nativeRun(index + 1, "candidate", fps),
    );
    const reference = [55.5, 56, 56.5].map((fps, index) =>
      nativeRun(index + 1, "reference", fps),
    );
    const gates = evaluateProfileGates(
      nativeProfile,
      10_000,
      { width: 2560, height: 1440 },
      candidate,
      reference,
    );
    const status = (name: string) =>
      gates.find((gate) => gate.name === name)?.status;

    expect(status("observed_fps")).toBe("pass");
    expect(status("capture_completeness")).toBe("pass");
    expect(status("regression_observed_fps")).toBe("pass");
  });
});
