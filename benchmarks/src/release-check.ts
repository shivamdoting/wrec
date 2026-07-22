import { access, realpath } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { verifyReleaseBench } from "./verify-release";

const repoRoot = path.resolve(import.meta.dir, "../..");

const git = (args: string[]) =>
  Bun.spawnSync(["git", ...args], {
    cwd: repoRoot,
    stdout: "pipe",
    stderr: "pipe",
  });

const parseArgs = (args: string[]) => {
  let reference = "";
  const forwarded: string[] = [];

  for (let index = 0; index < args.length; index += 1) {
    const [flag, inlineValue] = args[index].split("=", 2);
    const value = inlineValue ?? args[index + 1];
    if (flag === "--help" || flag === "-h") {
      console.log(`wrec local release check

Usage:
  bun run release:check --against /path/to/previous-release/wrec

Options forwarded to the benchmark:
  --duration <time>
  --sample-interval-ms <n>
`);
      process.exit(0);
    }
    if (flag === "--against") {
      if (!value)
        throw new Error(
          "--against requires the previous release's wrec binary",
        );
      reference = path.resolve(value);
      if (!inlineValue) index += 1;
      continue;
    }
    if (flag === "--duration" || flag === "--sample-interval-ms") {
      if (!value) throw new Error(`${flag} requires a value`);
      forwarded.push(flag, value);
      if (!inlineValue) index += 1;
      continue;
    }
    throw new Error(`unknown option: ${args[index]}`);
  }

  if (!reference) {
    throw new Error(
      "--against is required; releases must compare against the previous version",
    );
  }
  return { forwarded, reference };
};

const ensureReady = async (reference: string) => {
  if (process.platform !== "darwin") {
    throw new Error(
      "the release benchmark must run on a Mac with Screen Recording permission",
    );
  }

  const status = git(["status", "--porcelain", "--untracked-files=all"]);
  if (status.exitCode !== 0) {
    throw new Error(
      `unable to inspect the checkout: ${status.stderr.toString().trim()}`,
    );
  }
  if (status.stdout.toString().trim()) {
    throw new Error("the checkout must be clean before a release benchmark");
  }

  await access(reference, constants.X_OK).catch(() => {
    throw new Error(
      `reference binary is missing or not executable: ${reference}`,
    );
  });

  const candidate = path.join(repoRoot, "target", "release", "wrec");
  const resolvedReference = await realpath(reference);
  const resolvedCandidate = await realpath(candidate).catch(() => candidate);
  if (resolvedReference === resolvedCandidate) {
    throw new Error(
      "the reference must be the previous release, not the candidate binary",
    );
  }
};

const main = async () => {
  const { forwarded, reference } = parseArgs(Bun.argv.slice(2));
  await ensureReady(reference);

  const command = [
    "bun",
    path.join(repoRoot, "benchmarks", "src", "run.ts"),
    "release",
    "--against",
    reference,
    ...forwarded,
  ];
  console.log(`running: ${command.join(" ")}`);
  const benchmark = Bun.spawn(command, {
    cwd: repoRoot,
    env: Bun.env,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  const exitCode = await benchmark.exited;
  if (exitCode !== 0) {
    throw new Error(
      "release benchmark did not pass; fix failures or stabilize an inconclusive environment before rerunning",
    );
  }

  const summary = await verifyReleaseBench({
    releaseCommit: "HEAD",
    repoRoot,
    resultsDir: path.join(repoRoot, "benchmarks", "results"),
  });
  console.log(`release check passed: ${summary}`);
  console.log("commit the benchmark summary before creating the version tag");
};

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
