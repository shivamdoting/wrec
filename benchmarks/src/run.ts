import { mkdir, readdir, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import {
  combineStatuses,
  evaluateProfileGates,
  expectedOutputDimensions,
  releaseProfiles,
  smokeProfile,
  statusFromGates,
  summarizeRuns,
  type Codec,
  type DecodeResult,
  type Dimensions,
  type Gate,
  type GateStatus,
  type MetricsSnapshot,
  type ObservedSummary,
  type OverallStatus,
  type ProcessRow,
  type ProcessSample,
  type ProcessSummary,
  type ProfileResult,
  type ProfileSpec,
  type SuiteName,
  type VariantName,
  type RunResult,
} from "./gates";
import { benchmarkExitCode, nullableNumber } from "./benchmark-values";
import { writeSummary } from "./summary";

type CliOptions = {
  suite: SuiteName;
  duration: string;
  durationMs: number;
  wrecBin: string;
  autoBuild: boolean;
  againstBin?: string;
  sampleIntervalMs: number;
};

type CommandResult = {
  exitCode: number;
  elapsedMs: number;
  startedAtMs: number;
  completedAtMs: number;
  stdout: string;
  stderr: string;
};

type BinaryRuntime = {
  variant: VariantName;
  path: string;
  wrecHome: string;
  dataDir: string;
};

type RunContext = {
  id: string;
  generatedAt: string;
  runtimeDir: string;
  recordingsRoot: string;
  tools: {
    stimulus: string;
    decoder: string;
  };
  binaries: BinaryRuntime[];
};

type StimulusRuntime = {
  proc: ReturnType<typeof Bun.spawn>;
  readyLine: string;
  title: string;
  points: Dimensions;
  pixels: Dimensions;
  scale: number;
  stderr: Promise<string>;
};

type TargetInfo = {
  id: number;
  kind: string;
  name: string;
  [key: string]: unknown;
};

type EnvironmentCommand = {
  command: string[];
  exitCode: number;
  stdout: string;
  stderr: string;
};

type EnvironmentGuard = {
  name: string;
  threshold: string;
  measured: string | number | boolean | null;
  status: GateStatus;
  details?: string;
};

type BenchmarkResult = {
  schema: "wrec.perf/v1";
  id: string;
  generatedAt: string;
  suite: SuiteName;
  status: OverallStatus;
  duration: string;
  durationMs: number;
  sampleIntervalMs: number;
  binaries: {
    candidate: string;
    reference?: string;
  };
  runtime: {
    root: string;
    candidate: {
      wrecHome: string;
      dataDir: string;
    };
    reference?: {
      wrecHome: string;
      dataDir: string;
    };
  };
  stimulus: {
    title: string;
    readyLine: string;
    points: Dimensions;
    pixels: Dimensions;
    scale: number;
  };
  git: Awaited<ReturnType<typeof gitMetadata>>;
  machine: ReturnType<typeof machineMetadata>;
  environment: Awaited<ReturnType<typeof environmentPreamble>>;
  profiles: ProfileResult[];
};

const root = path.resolve(import.meta.dir, "..");
const repoRoot = path.resolve(root, "..");
const runsDir = path.join(root, "runs");
const resultsDir = path.join(root, "results");
// Daemon homes live under the run dir, and the daemon binds a unix socket at
// <home>/wrec.sock. macOS caps socket paths at ~104 bytes (SUN_LEN), so the
// run dir must stay short — a repo-relative .tmp/ path is already too long.
const tmpRoot = "/tmp/wrec-bench";
const stimulusTitle = "wrec-bench-stimulus";

const main = async () => {
  const options = parseArgs(Bun.argv.slice(2));
  // Belt to the stimulus's suspenders: keep display, idle, and system awake
  // for the whole run — display sleep mid-run reads as a frame-rate collapse.
  // unref, or bun waits for caffeinate while caffeinate -w waits for bun.
  Bun.spawn(["caffeinate", "-dimsu", "-w", String(process.pid)], {
    stdout: "ignore",
    stderr: "ignore",
  }).unref();
  if (options.autoBuild) {
    await buildCandidate();
  }
  const generatedAt = new Date().toISOString();
  const git = await gitMetadata();
  const id = `${slugDate(generatedAt)}-${shortSha(git.commit)}`;
  const runtimeDir = path.join(tmpRoot, id);
  const candidate = binaryRuntime("candidate", options.wrecBin, runtimeDir);
  const reference = options.againstBin
    ? binaryRuntime("reference", options.againstBin, runtimeDir)
    : undefined;
  const context: RunContext = {
    id,
    generatedAt,
    runtimeDir,
    recordingsRoot: path.join(runtimeDir, "recordings"),
    tools: {
      stimulus: path.join(runtimeDir, "stimulus"),
      decoder: path.join(runtimeDir, "decode"),
    },
    binaries: [candidate, reference].filter(isPresent),
  };

  await mkdir(runsDir, { recursive: true });
  await rm(runtimeDir, { recursive: true, force: true });
  await mkdir(context.recordingsRoot, { recursive: true });

  let stimulus: StimulusRuntime | undefined;
  let result: BenchmarkResult | undefined;
  try {
    await compileNativeTools(context);
    stimulus = await startStimulus(context.tools.stimulus);
    const environment = await environmentPreamble(options.suite);
    const environmentStatus = statusFromEnvironment(environment.guards, options.suite);
    const profiles = await runSuite(options, context, stimulus, environmentStatus === "pass");
    const status = combineStatuses([environmentStatus, ...profiles.map((profile) => profile.status)]);

    result = {
      schema: "wrec.perf/v1",
      id,
      generatedAt,
      suite: options.suite,
      status,
      duration: options.duration,
      durationMs: options.durationMs,
      sampleIntervalMs: options.sampleIntervalMs,
      binaries: {
        candidate: candidate.path,
        reference: reference?.path,
      },
      runtime: {
        root: context.runtimeDir,
        candidate: {
          wrecHome: candidate.wrecHome,
          dataDir: candidate.dataDir,
        },
        reference: reference
          ? {
              wrecHome: reference.wrecHome,
              dataDir: reference.dataDir,
            }
          : undefined,
      },
      stimulus: {
        title: stimulus.title,
        readyLine: stimulus.readyLine,
        points: stimulus.points,
        pixels: stimulus.pixels,
        scale: stimulus.scale,
      },
      git,
      machine: machineMetadata(),
      environment,
      profiles,
    };
  } finally {
    await stopAllDaemons(context.binaries);
    if (stimulus) {
      await stopStimulus(stimulus);
    }
  }

  if (!result) {
    throw new Error("benchmark run did not produce a result");
  }

  const resultPath = path.join(runsDir, `${result.id}.json`);
  await writeFile(resultPath, `${JSON.stringify(result, null, 2)}\n`);
  // Only release suites join the committed record (and the site's history);
  // smoke runs are local sanity checks.
  const summaryPath =
    options.suite === "release" ? await writeSummary(resultsDir, result) : null;

  console.log(`status: ${result.status}`);
  console.log(`results: ${resultPath}`);
  if (summaryPath) {
    console.log(`summary: ${summaryPath}`);
    console.log("view: wrec.app/benchmarks (or `bun run dev` in docs/) after committing the summary");
  }
  process.exitCode = benchmarkExitCode(result.suite, result.status);
};

const parseArgs = (args: string[]): CliOptions => {
  let suite: SuiteName = "smoke";
  let duration: string | undefined;
  let wrecBin = Bun.env.WREC_BIN ?? path.join(repoRoot, "target", "release", "wrec");
  // A candidate the caller never pointed at is built fresh before the run;
  // an explicit --wrec/WREC_BIN is benchmarked exactly as given.
  let wrecExplicit = Boolean(Bun.env.WREC_BIN);
  let againstBin: string | undefined;
  let sampleIntervalMs = 250;

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "smoke" || arg === "release") {
      suite = arg;
      continue;
    }
    const [flag, inlineValue] = arg.split("=", 2);
    const value = inlineValue ?? args[index + 1];

    if (flag === "--help" || flag === "-h") {
      printHelp();
      process.exit(0);
    } else if (flag === "--suite") {
      suite = parseSuite(requireValue(flag, value));
      if (!inlineValue) index += 1;
    } else if (flag === "--duration") {
      duration = requireValue(flag, value);
      if (!inlineValue) index += 1;
    } else if (flag === "--wrec") {
      wrecBin = path.resolve(requireValue(flag, value));
      wrecExplicit = true;
      if (!inlineValue) index += 1;
    } else if (flag === "--against") {
      againstBin = path.resolve(requireValue(flag, value));
      if (!inlineValue) index += 1;
    } else if (flag === "--sample-interval-ms") {
      sampleIntervalMs = parsePositiveInt(requireValue(flag, value), flag);
      if (!inlineValue) index += 1;
    } else {
      throw new Error(`unknown option: ${arg}`);
    }
  }

  const resolvedDuration = duration ?? (suite === "release" ? "15s" : "5s");
  return {
    suite,
    duration: resolvedDuration,
    durationMs: parseDurationMs(resolvedDuration),
    wrecBin: path.resolve(wrecBin),
    autoBuild: !wrecExplicit,
    againstBin,
    sampleIntervalMs,
  };
};

const buildCandidate = async () => {
  console.log("building candidate: cargo build --release -p cli -p daemon");
  const result = await runProcess(
    ["cargo", "build", "--release", "-p", "cli", "-p", "daemon"],
    repoRoot,
    Bun.env,
  );
  if (result.exitCode !== 0) {
    throw new Error(`cargo build failed:\n${result.stderr || result.stdout}`);
  }
};

const printHelp = () => {
  console.log(`wrec ground-truth performance benchmark

Usage (from the repo root):
  bun run bench                        # smoke: one ungated 5s recording
  bun run bench release                # gated release profiles
  bun run bench release --against /path/to/reference/wrec

Options:
  smoke | release             suite to run (default: smoke); --suite <name> also works
  --duration <time>           override profile duration, e.g. 5s, 15s, 1m
  --wrec <path>               candidate binary (default: target/release/wrec, built automatically)
  --against <path>            reference wrec binary for interleaved A/B regression gates
  --sample-interval-ms <n>    ps sampler interval (default: 250)
  -h, --help                  show this help
`);
};

const parseSuite = (value: string): SuiteName => {
  if (value === "smoke" || value === "release") {
    return value;
  }
  throw new Error("--suite expects smoke or release");
};

const requireValue = (flag: string, value?: string) => {
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
};

const parsePositiveInt = (value: string, flag: string) => {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${flag} expects a positive integer`);
  }
  return parsed;
};

const parseDurationMs = (value: string) => {
  const match = value.match(/^(\d+)(ms|s|m|h)$/);
  if (!match) {
    throw new Error(`duration expects a value like 5000ms, 5s, 1m, or 1h: ${value}`);
  }
  const amount = Number.parseInt(match[1], 10);
  const unit = match[2];
  const multiplier = unit === "ms" ? 1 : unit === "s" ? 1000 : unit === "m" ? 60_000 : 3_600_000;
  return amount * multiplier;
};

const binaryRuntime = (variant: VariantName, binPath: string, runtimeDir: string): BinaryRuntime => ({
  variant,
  path: path.resolve(binPath),
  wrecHome: path.join(runtimeDir, variant, "home"),
  dataDir: path.join(runtimeDir, variant, "data"),
});

const compileNativeTools = async (context: RunContext) => {
  await mkdir(path.dirname(context.tools.stimulus), { recursive: true });
  await compileSwift(
    path.join(root, "native", "stimulus.swift"),
    context.tools.stimulus,
    ["AppKit"],
  );
  await compileSwift(
    path.join(root, "native", "decode.swift"),
    context.tools.decoder,
    ["AVFoundation", "CoreMedia", "CoreVideo"],
  );
};

const compileSwift = async (source: string, output: string, frameworks: string[]) => {
  const args = [
    "swiftc",
    source,
    "-o",
    output,
    "-module-cache-path",
    path.join(path.dirname(output), "swift-module-cache"),
    ...frameworks.flatMap((framework) => ["-framework", framework]),
  ];
  const result = await shell(args);
  if (result.exitCode !== 0) {
    throw new Error(`swiftc failed for ${path.relative(repoRoot, source)}:\n${result.stderr || result.stdout}`);
  }
};

const startStimulus = async (stimulusBin: string): Promise<StimulusRuntime> => {
  const proc = Bun.spawn([stimulusBin], {
    cwd: root,
    stdout: "pipe",
    stderr: "pipe",
  });
  const stderr = new Response(proc.stderr).text();
  let readyLine: string;
  try {
    readyLine = await waitForStimulusReady(proc, stderr);
  } catch (error) {
    proc.kill("SIGTERM");
    await Promise.race([
      proc.exited.catch(() => undefined),
      Bun.sleep(1000),
    ]);
    throw error;
  }
  const ready = parseStimulusReady(readyLine);
  return {
    proc,
    readyLine,
    stderr,
    ...ready,
  };
};

const waitForStimulusReady = async (
  proc: ReturnType<typeof Bun.spawn>,
  stderr: Promise<string>,
) => {
  const decoder = new TextDecoder();
  const reader = proc.stdout.getReader();
  let buffer = "";
  const ready = (async () => {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        throw new Error(`stimulus exited before READY: ${await stderr}`);
      }
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? "";
      const line = lines.find((item) => item.startsWith("STIMULUS_READY "));
      if (line) {
        return line;
      }
    }
  })();
  const exited = proc.exited.then(async (code) => {
    throw new Error(`stimulus exited before READY (${code}): ${await stderr}`);
  });
  const timeout = Bun.sleep(10_000).then(() => {
    throw new Error("timed out waiting for stimulus READY");
  });
  return Promise.race([ready, exited, timeout]);
};

const parseStimulusReady = (line: string) => {
  const title = line.match(/\btitle=([^\s]+)/)?.[1] ?? stimulusTitle;
  const points = parseDimensions(line.match(/\bpoints=(\d+x\d+)/)?.[1]) ?? {
    width: 1280,
    height: 720,
  };
  const pixels = parseDimensions(line.match(/\bpixels=(\d+x\d+)/)?.[1]) ?? points;
  const scale = Number.parseFloat(line.match(/\bscale=([\d.]+)/)?.[1] ?? "1");
  return {
    title,
    points,
    pixels,
    scale: Number.isFinite(scale) ? scale : 1,
  };
};

const stopStimulus = async (stimulus: StimulusRuntime) => {
  stimulus.proc.kill("SIGTERM");
  const exited = await Promise.race([
    stimulus.proc.exited.then(() => true),
    Bun.sleep(2_000).then(() => false),
  ]);
  if (!exited) {
    stimulus.proc.kill("SIGKILL");
    await stimulus.proc.exited.catch(() => undefined);
  }
};

const runSuite = async (
  options: CliOptions,
  context: RunContext,
  stimulus: StimulusRuntime,
  envTrusted: boolean,
) => {
  const profiles = options.suite === "release" ? releaseProfiles : [smokeProfile];
  const candidate = context.binaries.find((binary) => binary.variant === "candidate")!;
  const reference = context.binaries.find((binary) => binary.variant === "reference");
  const results: ProfileResult[] = [];

  for (const profile of profiles) {
    const expectedDimensions = expectedOutputDimensions(stimulus.pixels, profile.resolution);
    if (options.suite === "smoke") {
      const run = await runCapture({
        context,
        binary: candidate,
        allBinaries: context.binaries,
        profile,
        duration: options.duration,
        durationMs: options.durationMs,
        expectedDimensions,
        sampleIntervalMs: options.sampleIntervalMs,
        rep: 1,
        warmup: false,
        stimulus,
      });
      const aggregate = summarizeRuns([run]);
      results.push({
        name: profile.name,
        request: { ...profile, duration: options.duration, durationMs: options.durationMs, expectedDimensions },
        target: run.target,
        targetInfo: run.targetInfo,
        status: statusFromGates([], [run]),
        warmups: [],
        measured: {
          candidate: [run],
        },
        capture: aggregate.capture,
        latency: aggregate.latency,
        process: aggregate.process,
        observed: aggregate.observed,
        gates: [],
      });
      continue;
    }

    const warmups = [
      await runCapture({
        context,
        binary: candidate,
        allBinaries: context.binaries,
        profile,
        duration: options.duration,
        durationMs: options.durationMs,
        expectedDimensions,
        sampleIntervalMs: options.sampleIntervalMs,
        rep: 0,
        warmup: true,
        stimulus,
      }),
    ];
    if (reference) {
      warmups.push(
        await runCapture({
          context,
          binary: reference,
          allBinaries: context.binaries,
          profile,
          duration: options.duration,
          durationMs: options.durationMs,
          expectedDimensions,
          sampleIntervalMs: options.sampleIntervalMs,
          rep: 0,
          warmup: true,
          stimulus,
        }),
      );
    }

    const candidateRuns: RunResult[] = [];
    const referenceRuns: RunResult[] = [];
    for (let rep = 1; rep <= 3; rep += 1) {
      candidateRuns.push(
        await runCapture({
          context,
          binary: candidate,
          allBinaries: context.binaries,
          profile,
          duration: options.duration,
          durationMs: options.durationMs,
          expectedDimensions,
          sampleIntervalMs: options.sampleIntervalMs,
          rep,
          warmup: false,
          stimulus,
        }),
      );
      if (reference) {
        referenceRuns.push(
          await runCapture({
            context,
            binary: reference,
            allBinaries: context.binaries,
            profile,
            duration: options.duration,
            durationMs: options.durationMs,
            expectedDimensions,
            sampleIntervalMs: options.sampleIntervalMs,
            rep,
            warmup: false,
            stimulus,
          }),
        );
      }
    }

    const aggregate = summarizeRuns(candidateRuns);
    const referenceAggregate = reference ? summarizeRuns(referenceRuns) : undefined;
    const gates = evaluateProfileGates(
      profile,
      options.durationMs,
      expectedDimensions,
      candidateRuns,
      reference ? referenceRuns : undefined,
      envTrusted,
    );
    const status = statusFromGates(gates, [
      ...warmups,
      ...candidateRuns,
      ...referenceRuns,
    ]);
    results.push({
      name: profile.name,
      request: { ...profile, duration: options.duration, durationMs: options.durationMs, expectedDimensions },
      target: candidateRuns[0]?.target ?? warmups[0]?.target ?? `window:${stimulus.title}`,
      targetInfo: candidateRuns[0]?.targetInfo ?? warmups[0]?.targetInfo,
      status,
      warmups,
      measured: {
        candidate: candidateRuns,
        reference: reference ? referenceRuns : undefined,
      },
      capture: aggregate.capture,
      latency: aggregate.latency,
      process: aggregate.process,
      observed: aggregate.observed,
      reference: referenceAggregate,
      gates,
    });
  }

  return results;
};

const runCapture = async ({
  context,
  binary,
  allBinaries,
  profile,
  duration,
  durationMs,
  sampleIntervalMs,
  rep,
  warmup,
  stimulus,
}: {
  context: RunContext;
  binary: BinaryRuntime;
  allBinaries: BinaryRuntime[];
  profile: ProfileSpec;
  duration: string;
  durationMs: number;
  expectedDimensions: Dimensions;
  sampleIntervalMs: number;
  rep: number;
  warmup: boolean;
  stimulus: StimulusRuntime;
}): Promise<RunResult> => {
  const runId = `${profile.name}-${binary.variant}-${warmup ? "warmup" : `rep-${rep}`}`;
  const outDir = path.join(context.recordingsRoot, runId);
  await rm(outDir, { recursive: true, force: true });
  await mkdir(outDir, { recursive: true });

  let target = `window:${stimulus.title}`;
  let targetInfo: TargetInfo | undefined;
  let daemonPid = 0;
  try {
    await stopAllDaemons(allBinaries);
    await ensureDaemon(binary);
    daemonPid = await daemonPidFor(binary);
    targetInfo = await resolveStimulusTarget(binary, stimulus.title);
    target = `window:${targetInfo.id}`;
  } catch (error) {
    return failedSetupRun({
      id: runId,
      binary,
      profile,
      rep,
      warmup,
      target,
      outDir,
      error,
    });
  }

  const args = [
    "record",
    "start",
    "--target",
    target,
    "--quality",
    profile.quality,
    "--fps",
    String(profile.fps),
    "--codec",
    profile.codec,
    "--resolution",
    profile.resolution,
    "--duration",
    duration,
    "--out",
    outDir,
    "--json",
    "--no-queue",
  ];
  const command = [binary.path, ...args];
  let keepSampling = true;
  const samplesPromise = sampleProcesses(
    daemonPid,
    sampleIntervalMs,
    () => keepSampling,
  );
  let result: CommandResult;
  let error: string | undefined;

  try {
    result = await runWrec(binary, args);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    const now = Date.now();
    result = {
      exitCode: 1,
      elapsedMs: 0,
      startedAtMs: now,
      completedAtMs: now,
      stdout: "",
      stderr: "",
    };
    error = message;
  } finally {
    keepSampling = false;
  }
  const processSamples = await samplesPromise;
  const recordingFiles = await listFiles(outDir);
  const outputBytes = await sumFileSizes(recordingFiles);
  const movieFile = await selectMovieFile(recordingFiles);
  const decodeAttempt = movieFile
    ? await decodeMovie(context.tools.decoder, movieFile)
    : { decode: null, decoderError: "no .mov output found" };
  await rm(outDir, { recursive: true, force: true });

  const jsonEvents = parseJsonEvents(result.stdout);
  const metrics = parseMetrics(jsonEvents);
  const lastMetrics = metrics.at(-1) ?? null;
  const observed = decodeAttempt.decode
    ? deriveObserved(decodeAttempt.decode, lastMetrics)
    : null;

  return {
    id: runId,
    variant: binary.variant,
    profile: profile.name,
    rep,
    warmup,
    command,
    target,
    targetInfo,
    exitCode: result.exitCode,
    elapsedMs: result.elapsedMs,
    outputBytes,
    recordingFilesDeleted: recordingFiles.map((file) => path.relative(root, file)),
    metrics,
    lastMetrics,
    latency: deriveLatency(jsonEvents, result.startedAtMs, result.completedAtMs, durationMs),
    processSamples,
    processSummary: summarizeProcessSamples(processSamples),
    decode: decodeAttempt.decode,
    decoderError: decodeAttempt.decoderError,
    observed,
    jsonEvents,
    stdoutLines: lines(result.stdout),
    stderrLines: lines(result.stderr),
    error,
  };
};

const failedSetupRun = async ({
  id,
  binary,
  profile,
  rep,
  warmup,
  target,
  outDir,
  error,
}: {
  id: string;
  binary: BinaryRuntime;
  profile: ProfileSpec;
  rep: number;
  warmup: boolean;
  target: string;
  outDir: string;
  error: unknown;
}): Promise<RunResult> => {
  await rm(outDir, { recursive: true, force: true });
  const message = error instanceof Error ? error.message : String(error);
  return {
    id,
    variant: binary.variant,
    profile: profile.name,
    rep,
    warmup,
    command: [binary.path, "record", "start"],
    target,
    exitCode: 1,
    elapsedMs: 0,
    outputBytes: 0,
    recordingFilesDeleted: [],
    metrics: [],
    lastMetrics: null,
    latency: {
      startMs: null,
      finalizeMs: null,
      recordingStartedAtMs: null,
      durationElapsedAtMs: null,
      terminalAtMs: null,
    },
    processSamples: [],
    processSummary: summarizeProcessSamples([]),
    decode: null,
    decoderError: "recording did not start",
    observed: null,
    jsonEvents: [],
    stdoutLines: [],
    stderrLines: [],
    error: message,
  };
};

const ensureDaemon = async (binary: BinaryRuntime) => {
  const result = await runWrec(binary, ["daemon", "start", "--json"]);
  if (result.exitCode !== 0) {
    throw new Error(`daemon start failed for ${binary.variant}:\n${result.stderr || result.stdout}`);
  }
};

const daemonPidFor = async (binary: BinaryRuntime) => {
  const result = await runWrec(binary, ["daemon", "status", "--json"]);
  if (result.exitCode !== 0) {
    throw new Error(`daemon status failed for ${binary.variant}:\n${result.stderr || result.stdout}`);
  }
  const status = JSON.parse(result.stdout) as Record<string, unknown>;
  const pid = Number(status.pid);
  if (!Number.isFinite(pid)) {
    throw new Error(`daemon status did not include a numeric pid: ${result.stdout}`);
  }
  return pid;
};

const resolveStimulusTarget = async (binary: BinaryRuntime, title: string) => {
  const result = await runWrec(binary, ["list", "--json"]);
  if (result.exitCode !== 0) {
    throw new Error(`target detection failed for ${binary.variant}:\n${result.stderr || result.stdout}`);
  }
  const targets = JSON.parse(result.stdout) as TargetInfo[];
  const windows = targets.filter((item) => item.kind === "window");
  const exact = windows.find((item) => item.name === title);
  const fuzzy = windows.find((item) => item.name?.includes(title));
  const target = exact ?? fuzzy;
  if (!target) {
    throw new Error(`stimulus window not found in wrec list --json; saw windows: ${windows.map((item) => item.name).join(", ")}`);
  }
  return target;
};

const stopAllDaemons = async (binaries: BinaryRuntime[]) => {
  for (const binary of binaries) {
    await runWrec(binary, ["daemon", "stop", "--json"]).catch(() => undefined);
  }
  // `daemon stop` can return before the daemon has unlinked its socket; a
  // back-to-back `daemon start` on the same home would then fail to bind.
  for (const binary of binaries) {
    const socket = path.join(binary.wrecHome, "wrec.sock");
    for (let waited = 0; waited < 2000; waited += 50) {
      if (!(await Bun.file(socket).exists())) {
        break;
      }
      await Bun.sleep(50);
    }
  }
};

const runWrec = (binary: BinaryRuntime, args: string[]) =>
  runProcess([binary.path, ...args], repoRoot, {
    ...baseWrecEnv(),
    WREC_HOME: binary.wrecHome,
    WREC_DATA_DIR: binary.dataDir,
  });

const shell = (cmd: string[]) => runProcess(cmd, repoRoot, Bun.env);

const baseWrecEnv = () => {
  const env = { ...Bun.env };
  delete env.WREC_DAEMON_BIN;
  delete env.WREC_CAPTURE_ENGINE_PATH;
  delete env.WREC_MIC_PILL_TEST;
  return env;
};

const runProcess = async (
  cmd: string[],
  cwd: string,
  env: NodeJS.ProcessEnv,
): Promise<CommandResult> => {
  const startedAtMs = Date.now();
  const started = performance.now();
  let proc: ReturnType<typeof Bun.spawn>;
  try {
    proc = Bun.spawn(cmd, {
      cwd,
      env,
      stdout: "pipe",
      stderr: "pipe",
    });
  } catch (error) {
    return {
      exitCode: 127,
      elapsedMs: Math.round(performance.now() - started),
      startedAtMs,
      completedAtMs: Date.now(),
      stdout: "",
      stderr: error instanceof Error ? error.message : String(error),
    };
  }

  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);

  return {
    exitCode,
    elapsedMs: Math.round(performance.now() - started),
    startedAtMs,
    completedAtMs: Date.now(),
    stdout,
    stderr,
  };
};

const decodeMovie = async (decoderBin: string, movieFile: string) => {
  const result = await shell([decoderBin, movieFile]);
  if (result.exitCode !== 0) {
    return {
      decode: null,
      decoderError: result.stderr || result.stdout || `decoder exited ${result.exitCode}`,
    };
  }

  try {
    return {
      decode: JSON.parse(result.stdout) as DecodeResult,
      decoderError: undefined,
    };
  } catch (error) {
    return {
      decode: null,
      decoderError: error instanceof Error ? error.message : String(error),
    };
  }
};

const deriveObserved = (
  decode: DecodeResult,
  lastMetrics: MetricsSnapshot | null,
): ObservedSummary => {
  const frames = decode.frames;
  const readable = frames.filter((frame) => Number.isFinite(frame.stimulusIndex));
  const indices = readable.map((frame) => frame.stimulusIndex!).sort((a, b) => a - b);
  const uniqueIndices = [...new Set(indices)];
  const missingStimulusIndices =
    uniqueIndices.length > 1
      ? range(uniqueIndices[0], uniqueIndices.at(-1)!).filter((index) => !uniqueIndices.includes(index))
      : [];
  const pts = frames.map((frame) => frame.ptsMs).filter(Number.isFinite);
  const firstPtsMs = pts.length ? Math.min(...pts) : null;
  const lastPtsMs = pts.length ? Math.max(...pts) : null;
  const steadyStart = firstPtsMs === null ? null : firstPtsMs + 1000;
  const steadyEnd = lastPtsMs === null ? null : lastPtsMs - 1000;
  const steadyReadable =
    steadyStart !== null && steadyEnd !== null && steadyEnd > steadyStart
      ? readable.filter((frame) => frame.ptsMs >= steadyStart && frame.ptsMs <= steadyEnd)
      : [];
  const steadyUnique = new Set(steadyReadable.map((frame) => frame.stimulusIndex));
  const steadyDurationSecs =
    steadyStart !== null && steadyEnd !== null && steadyEnd > steadyStart
      ? (steadyEnd - steadyStart) / 1000
      : 0;
  // The stimulus advances its index once per rendered frame, so the index
  // span is how many frames actually reached the screen during the steady
  // window; the unique count is how many of those the recorder kept. Gating
  // against the span keeps the recorder from being blamed for frames the
  // stimulus never displayed.
  const steadyIndices = steadyReadable.map((frame) => frame.stimulusIndex!);
  const steadySpan =
    steadyIndices.length > 1
      ? Math.max(...steadyIndices) - Math.min(...steadyIndices) + 1
      : null;
  const stimulusAchievedFps =
    steadySpan !== null && steadyDurationSecs > 0 ? steadySpan / steadyDurationSecs : null;
  const captureCompleteness =
    steadySpan !== null && steadySpan > 0 ? steadyUnique.size / steadySpan : null;
  // Gaps and monotonicity are computed in AVAssetReader decode order, which
  // equals presentation order only when the stream has no B-frames. wrec's
  // realtime capture pipeline never emits B-frames today; if it ever starts,
  // pts_monotonic fails loudly and that encoder change gets reviewed here.
  const ptsGaps = pts.slice(1).map((value, index) => value - pts[index]);
  const selfReportDisagreementRatio =
    typeof lastMetrics?.frames === "number" && uniqueIndices.length > 0
      ? Math.abs(lastMetrics.frames - uniqueIndices.length) / uniqueIndices.length
      : null;

  return {
    decodedFrames: frames.length,
    readableStimulusFrames: readable.length,
    uniqueStimulusFrames: uniqueIndices.length,
    duplicateStimulusFrames: Math.max(0, readable.length - uniqueIndices.length),
    missingStimulusIndices,
    effectiveFps: steadyDurationSecs > 0 ? steadyUnique.size / steadyDurationSecs : null,
    stimulusAchievedFps,
    captureCompleteness,
    maxInterFramePtsGapMs: ptsGaps.length ? Math.max(...ptsGaps) : null,
    ptsMonotonic: ptsGaps.every((gap) => gap >= 0),
    firstPtsMs,
    lastPtsMs,
    codec: decode.codec,
    dimensions: decode.dimensions,
    durationMs: decode.durationMs,
    selfReportDisagreementRatio,
  };
};

const deriveLatency = (
  jsonEvents: Array<Record<string, unknown>>,
  commandStartedAtMs: number,
  commandCompletedAtMs: number,
  durationMs: number,
) => {
  const jobEvents = jsonEvents.filter((event) => event.event === "job_event");
  const started =
    findJobEventMs(jobEvents, "recording started") ??
    findJobEventMs(jobEvents, "recording active");
  const durationElapsed = findJobEventMs(jobEvents, "duration elapsed; stopping");
  const terminal =
    [...jobEvents]
      .reverse()
      .map((event) => timestampMs(event))
      .find(isFiniteNumber) ?? null;
  const fallbackDurationExpiry =
    started === null ? null : started + durationMs;

  return {
    startMs: started === null ? null : Math.max(0, started - commandStartedAtMs),
    finalizeMs:
      terminal === null
        ? null
        : durationElapsed !== null
          ? Math.max(0, terminal - durationElapsed)
          : fallbackDurationExpiry === null
            ? Math.max(0, commandCompletedAtMs - terminal)
            : Math.max(0, terminal - fallbackDurationExpiry),
    recordingStartedAtMs: started,
    durationElapsedAtMs: durationElapsed,
    terminalAtMs: terminal,
  };
};

const findJobEventMs = (events: Array<Record<string, unknown>>, message: string) =>
  timestampMs(events.find((event) => event.message === message));

const timestampMs = (event?: Record<string, unknown>) => {
  const value = Number(event?.timestamp_ms);
  return Number.isFinite(value) ? value : null;
};

const sampleProcesses = async (
  daemonPid: number,
  intervalMs: number,
  shouldContinue: () => boolean,
): Promise<ProcessSample[]> => {
  const samples: ProcessSample[] = [];
  while (shouldContinue()) {
    samples.push({
      timestampMs: Date.now(),
      processes: await processRows(daemonPid),
    });
    await Bun.sleep(intervalMs);
  }

  samples.push({
    timestampMs: Date.now(),
    processes: await processRows(daemonPid),
  });
  return samples;
};

const processRows = async (daemonPid: number): Promise<ProcessRow[]> => {
  if (!daemonPid) {
    return [];
  }
  const children = await descendantPids(daemonPid);
  const pids = [daemonPid, ...children];
  const result = await shell([
    "ps",
    "-o",
    "pid=",
    "-o",
    "ppid=",
    "-o",
    "pcpu=",
    "-o",
    "rss=",
    "-o",
    "comm=",
    "-p",
    pids.join(","),
  ]);
  if (result.exitCode !== 0) {
    return [];
  }

  return lines(result.stdout)
    .map((line) => parseProcessRow(line, daemonPid))
    .filter(isPresent);
};

const descendantPids = async (pid: number): Promise<number[]> => {
  const direct = await childPids(pid);
  const descendants = await Promise.all(direct.map(descendantPids));
  return [...direct, ...descendants.flat()];
};

const childPids = async (pid: number) => {
  const result = await shell(["pgrep", "-P", String(pid)]);
  if (result.exitCode !== 0) {
    return [];
  }
  return lines(result.stdout)
    .map((line) => Number.parseInt(line.trim(), 10))
    .filter(Number.isFinite);
};

const parseProcessRow = (line: string, daemonPid: number): ProcessRow | undefined => {
  const match = line.trim().match(/^(\d+)\s+(\d+)\s+([\d.]+)\s+(\d+)\s+(.+)$/);
  if (!match) {
    return undefined;
  }
  const pid = Number.parseInt(match[1], 10);
  const ppid = Number.parseInt(match[2], 10);
  const command = match[5];
  const role =
    pid === daemonPid
      ? "daemon"
      : command.includes("capture-engine") || command.includes("wrec-helper")
        ? "helper"
        : "child";

  return {
    pid,
    ppid,
    cpuPercent: Number.parseFloat(match[3]),
    rssBytes: Number.parseInt(match[4], 10) * 1024,
    command,
    role,
  };
};

const summarizeProcessSamples = (samples: ProcessSample[]): ProcessSummary => {
  const totals = samples.map((sample) => ({
    cpuPercent: sum(sample.processes.map((process) => process.cpuPercent)),
    rssBytes: sum(sample.processes.map((process) => process.rssBytes)),
  }));
  const byRole = (role: ProcessRow["role"]) =>
    samples.flatMap((sample) => sample.processes.filter((process) => process.role === role));
  const helpers = byRole("helper");
  const daemons = byRole("daemon");

  return {
    sampleCount: samples.length,
    maxTotalCpuPercent: max(totals.map((total) => total.cpuPercent)),
    p95TotalCpuPercent: percentile(totals.map((total) => total.cpuPercent), 95),
    avgTotalCpuPercent: average(totals.map((total) => total.cpuPercent)),
    maxTotalRssBytes: max(totals.map((total) => total.rssBytes)),
    maxHelperCpuPercent: max(helpers.map((process) => process.cpuPercent)),
    maxHelperRssBytes: max(helpers.map((process) => process.rssBytes)),
    maxDaemonCpuPercent: max(daemons.map((process) => process.cpuPercent)),
    maxDaemonRssBytes: max(daemons.map((process) => process.rssBytes)),
  };
};

const parseMetrics = (events: Array<Record<string, unknown>>) =>
  events.flatMap((event) => {
    if (!isRecord(event.metrics)) {
      return [];
    }
    return [
      {
        elapsed_secs: Number(event.metrics.elapsed_secs ?? 0),
        output_bytes: Number(event.metrics.output_bytes ?? 0),
        estimated_bitrate_mbps: Number(event.metrics.estimated_bitrate_mbps ?? 0),
        frames: nullableNumber(event.metrics.frames),
        dropped_frames: nullableNumber(event.metrics.dropped_frames),
      },
    ];
  });

const parseJsonEvents = (stdout: string) =>
  lines(stdout).flatMap((line) => {
    try {
      return [JSON.parse(line) as Record<string, unknown>];
    } catch {
      return [];
    }
  });

const environmentPreamble = async (suite: SuiteName) => {
  const [battery, thermal, productVersion, buildVersion, chip, model, memsize] =
    await Promise.all([
      commandSnapshot(["pmset", "-g", "batt"]),
      commandSnapshot(["pmset", "-g", "therm"]),
      commandText(["sw_vers", "-productVersion"]),
      commandText(["sw_vers", "-buildVersion"]),
      commandText(["sysctl", "-n", "machdep.cpu.brand_string"]),
      commandText(["sysctl", "-n", "hw.model"]),
      commandText(["sysctl", "-n", "hw.memsize"]),
    ]);
  const loadAverage = os.loadavg();
  const cpuCount = os.cpus().length;
  const guards = environmentGuards({ battery, thermal, loadAverage, cpuCount }, suite);

  return {
    acPower: battery,
    thermal,
    macos: {
      productVersion,
      buildVersion,
    },
    chip,
    model,
    memoryBytes: Number.parseInt(memsize, 10) || os.totalmem(),
    loadAverage,
    guards,
  };
};

const environmentGuards = (
  {
    battery,
    thermal,
    loadAverage,
    cpuCount,
  }: {
    battery: EnvironmentCommand;
    thermal: EnvironmentCommand;
    loadAverage: number[];
    cpuCount: number;
  },
  suite: SuiteName,
): EnvironmentGuard[] => {
  if (suite !== "release") {
    return [];
  }

  const onAc = /AC Power/i.test(battery.stdout);
  const thermalLimits = parseThermalLimits(thermal.stdout);
  const thermalOk =
    thermal.exitCode === 0 &&
    thermalLimits.every((value) => value >= 100) &&
    !/thermal warning level:\s*(?!0|none)/i.test(thermal.stdout);
  const loadOne = loadAverage[0] ?? 0;

  return [
    {
      name: "env_ac_power",
      threshold: "AC Power",
      measured: onAc ? "AC Power" : battery.stdout.trim().split("\n")[0] ?? null,
      status: onAc ? "pass" : "inconclusive",
    },
    {
      name: "env_thermal",
      threshold: "thermal limits at 100%",
      measured: thermal.exitCode === 0 ? thermal.stdout.trim() : thermal.stderr.trim(),
      status: thermalOk ? "pass" : "inconclusive",
    },
    {
      name: "env_load_average",
      threshold: `1m load <= ${cpuCount}`,
      measured: loadOne,
      status: loadOne <= cpuCount ? "pass" : "inconclusive",
    },
  ];
};

const statusFromEnvironment = (guards: EnvironmentGuard[], suite: SuiteName): OverallStatus => {
  if (suite !== "release") {
    return "pass";
  }
  return guards.some((guard) => guard.status === "inconclusive") ? "inconclusive" : "pass";
};

const parseThermalLimits = (stdout: string) =>
  [...stdout.matchAll(/(?:CPU_Speed_Limit|CPU_Scheduler_Limit)\s*=\s*(\d+)/g)].map((match) =>
    Number.parseInt(match[1], 10),
  );

const commandSnapshot = async (cmd: string[]): Promise<EnvironmentCommand> => {
  const result = await shell(cmd);
  return {
    command: cmd,
    exitCode: result.exitCode,
    stdout: result.stdout,
    stderr: result.stderr,
  };
};

const commandText = async (cmd: string[]) => (await shell(cmd)).stdout.trim();

const gitMetadata = async () => ({
  branch: (await shell(["git", "branch", "--show-current"])).stdout.trim(),
  commit: (await shell(["git", "rev-parse", "HEAD"])).stdout.trim(),
  dirty: (await shell(["git", "status", "--short"])).stdout.trim().length > 0,
});

const machineMetadata = () => ({
  hostname: os.hostname(),
  platform: os.platform(),
  release: os.release(),
  arch: os.arch(),
  cpu: os.cpus()[0]?.model ?? "unknown",
  cpuCount: os.cpus().length,
  totalMemoryBytes: os.totalmem(),
});

const listFiles = async (dir: string): Promise<string[]> => {
  const entries = await readdir(dir, { withFileTypes: true }).catch(() => []);
  const files = await Promise.all(
    entries.map(async (entry) => {
      const child = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        return listFiles(child);
      }
      return entry.isFile() ? [child] : [];
    }),
  );
  return files.flat();
};

const selectMovieFile = async (files: string[]) => {
  const movies = files.filter((file) => file.endsWith(".mov"));
  if (!movies.length) {
    return undefined;
  }
  const withSizes = await Promise.all(
    movies.map(async (file) => ({
      file,
      size: (await stat(file)).size,
    })),
  );
  return withSizes.sort((a, b) => b.size - a.size)[0].file;
};

const sumFileSizes = async (files: string[]) => {
  let bytes = 0;
  for (const file of files) {
    bytes += (await stat(file)).size;
  }
  return bytes;
};

const parseDimensions = (value?: string) => {
  if (!value) {
    return undefined;
  }
  const [width, height] = value.split("x").map((part) => Number.parseInt(part, 10));
  return Number.isFinite(width) && Number.isFinite(height) ? { width, height } : undefined;
};

const range = (start: number, end: number) =>
  Array.from({ length: Math.max(0, end - start + 1) }, (_, index) => start + index);

const lines = (value: string) => value.split(/\r?\n/).filter((line) => line.length > 0);
const slugDate = (value: string) => value.replaceAll(/[:.]/g, "-");
const shortSha = (sha: string) => sha.slice(0, 7);
const sum = (values: number[]) => values.reduce((total, value) => total + value, 0);
const max = (values: number[]) => (values.length ? Math.max(...values) : 0);
const average = (values: number[]) => (values.length ? sum(values) / values.length : 0);
const percentile = (values: number[], percent: number) => {
  if (!values.length) {
    return 0;
  }
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil((percent / 100) * sorted.length) - 1));
  return sorted[index];
};
const isRecord = <K extends string, V>(value: unknown): value is Record<K, V> =>
  typeof value === "object" && value !== null;
const isPresent = <T>(value: T | null | undefined): value is T => value !== null && value !== undefined;
const isFiniteNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);

if (import.meta.main) {
  await main();
}
