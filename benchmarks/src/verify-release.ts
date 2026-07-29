import { appendFile, readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { releaseProfiles } from "./gates";

type JsonRecord = Record<string, unknown>;

type VerificationOptions = {
  releaseCommit: string;
  repoRoot: string;
  resultsDir: string;
};

type GitResult = {
  exitCode: number;
  stdout: string;
  stderr: string;
};

type GitRunner = (args: string[]) => GitResult;

const isRecord = (value: unknown): value is JsonRecord =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const stringValue = (value: unknown) =>
  typeof value === "string" ? value : "";

export const validateReleaseSummary = (value: unknown) => {
  const errors: string[] = [];
  if (!isRecord(value)) {
    return ["summary is not a JSON object"];
  }

  if (value.schema !== "wrec.perf/v1")
    errors.push("schema is not wrec.perf/v1");
  if (value.suite !== "release") errors.push("suite is not release");
  if (value.status !== "pass") errors.push("status is not pass");

  const git = isRecord(value.git) ? value.git : {};
  if (!stringValue(git.commit)) errors.push("git.commit is missing");
  if (git.dirty !== false) errors.push("benchmark checkout was dirty");

  const binaries = isRecord(value.binaries) ? value.binaries : {};
  const candidate = stringValue(binaries.candidate);
  const reference = stringValue(binaries.reference);
  if (!candidate) errors.push("candidate binary is missing");
  if (!reference)
    errors.push("reference binary is missing; an A/B release run is required");
  if (
    candidate &&
    reference &&
    path.resolve(candidate) === path.resolve(reference)
  ) {
    errors.push("candidate and reference point to the same binary");
  }

  const profiles = Array.isArray(value.profiles) ? value.profiles : [];
  for (const expected of releaseProfiles) {
    const profile = profiles.find(
      (item) => isRecord(item) && item.name === expected.name,
    ) as JsonRecord | undefined;
    if (!profile) {
      errors.push(`release profile is missing: ${expected.name}`);
      continue;
    }
    const measured = isRecord(profile.measured) ? profile.measured : {};
    const candidateRuns = Array.isArray(measured.candidate)
      ? measured.candidate
      : [];
    const referenceRuns = Array.isArray(measured.reference)
      ? measured.reference
      : [];
    if (candidateRuns.length < 3) {
      errors.push(`${expected.name} has fewer than 3 candidate reps`);
    }
    if (referenceRuns.length < 3) {
      errors.push(`${expected.name} has fewer than 3 reference reps`);
    }
  }

  return errors;
};

const defaultGitRunner =
  (repoRoot: string): GitRunner =>
  (args) => {
    const result = Bun.spawnSync(["git", ...args], {
      cwd: repoRoot,
      stdout: "pipe",
      stderr: "pipe",
    });
    return {
      exitCode: result.exitCode,
      stdout: result.stdout.toString(),
      stderr: result.stderr.toString(),
    };
  };

export const verifyReleaseBench = async (
  options: VerificationOptions,
  runGit: GitRunner = defaultGitRunner(options.repoRoot),
) => {
  const names = (await readdir(options.resultsDir).catch(() => []))
    .filter((name) => name.endsWith(".json"))
    .sort((left, right) => right.localeCompare(left));
  const rejected: string[] = [];

  for (const name of names) {
    const file = path.join(options.resultsDir, name);
    let summary: unknown;
    try {
      summary = JSON.parse(await readFile(file, "utf8"));
    } catch (error) {
      rejected.push(`${name}: invalid JSON (${String(error)})`);
      continue;
    }

    const errors = validateReleaseSummary(summary);
    if (errors.length) {
      rejected.push(`${name}: ${errors.join("; ")}`);
      continue;
    }

    const git = (summary as JsonRecord).git as JsonRecord;
    const benchedCommit = git.commit as string;
    if (
      runGit(["cat-file", "-e", `${benchedCommit}^{commit}`]).exitCode !== 0
    ) {
      rejected.push(`${name}: benched commit is not available locally`);
      continue;
    }
    if (
      runGit([
        "merge-base",
        "--is-ancestor",
        benchedCommit,
        options.releaseCommit,
      ]).exitCode !== 0
    ) {
      rejected.push(
        `${name}: benched commit is not an ancestor of the release commit`,
      );
      continue;
    }

    const diff = runGit([
      "diff",
      "--name-only",
      benchedCommit,
      options.releaseCommit,
      "--",
      ".",
      ":(exclude)benchmarks",
    ]);
    if (diff.exitCode !== 0) {
      throw new Error(`git diff failed: ${diff.stderr.trim()}`);
    }
    if (diff.stdout.trim()) {
      rejected.push(`${name}: non-benchmark code changed after the run`);
      continue;
    }

    return file;
  }

  const details = rejected
    .slice(0, 6)
    .map((item) => `\n- ${item}`)
    .join("");
  throw new Error(
    `No passing A/B release benchmark covers ${options.releaseCommit}.${details}`,
  );
};

const parseCliArgs = (args: string[]) => {
  const repoRoot = path.resolve(import.meta.dir, "../..");
  let releaseCommit = "HEAD";
  let resultsDir = path.join(repoRoot, "benchmarks", "results");

  for (let index = 0; index < args.length; index += 1) {
    const [flag, inlineValue] = args[index].split("=", 2);
    const value = inlineValue ?? args[index + 1];
    if (flag === "--release-commit") {
      if (!value) throw new Error("--release-commit requires a value");
      releaseCommit = value;
      if (!inlineValue) index += 1;
    } else if (flag === "--results-dir") {
      if (!value) throw new Error("--results-dir requires a value");
      resultsDir = path.resolve(value);
      if (!inlineValue) index += 1;
    } else {
      throw new Error(`unknown option: ${args[index]}`);
    }
  }

  return { releaseCommit, repoRoot, resultsDir };
};

if (import.meta.main) {
  try {
    const options = parseCliArgs(Bun.argv.slice(2));
    const summary = await verifyReleaseBench(options);
    console.log(`verified release benchmark: ${summary}`);
    if (Bun.env.GITHUB_OUTPUT) {
      await appendFile(Bun.env.GITHUB_OUTPUT, `summary=${summary}\n`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
