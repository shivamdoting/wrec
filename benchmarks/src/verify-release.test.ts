import { describe, expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { validateReleaseSummary, verifyReleaseBench } from "./verify-release";

const profileNames = [
  "efficient-720p30-hevc",
  "balanced-1080p30-hevc",
  "high-native60-hevc",
  "balanced-1080p30-h264",
];

const passingSummary = () => ({
  schema: "wrec.perf/v1",
  suite: "release",
  status: "pass",
  git: { commit: "abc123", dirty: false },
  binaries: {
    candidate: "/tmp/candidate/wrec",
    reference: "/tmp/reference/wrec",
  },
  profiles: profileNames.map((name) => ({
    name,
    measured: {
      candidate: [{}, {}, {}],
      reference: [{}, {}, {}],
    },
  })),
});

describe("validateReleaseSummary", () => {
  test("accepts a complete passing A/B release run", () => {
    expect(validateReleaseSummary(passingSummary())).toEqual([]);
  });

  test("rejects budget-only and self-vs-self runs", () => {
    const missingReference = passingSummary();
    missingReference.binaries.reference = "";
    expect(validateReleaseSummary(missingReference)).toContain(
      "reference binary is missing; an A/B release run is required",
    );

    const sameBinary = passingSummary();
    sameBinary.binaries.reference = sameBinary.binaries.candidate;
    expect(validateReleaseSummary(sameBinary)).toContain(
      "candidate and reference point to the same binary",
    );
  });

  test("rejects failed, dirty, or incomplete runs", () => {
    const summary = passingSummary();
    summary.status = "inconclusive";
    summary.git.dirty = true;
    summary.profiles[0].measured.reference = [];

    expect(validateReleaseSummary(summary)).toEqual(
      expect.arrayContaining([
        "status is not pass",
        "benchmark checkout was dirty",
        "efficient-720p30-hevc has fewer than 3 reference reps",
      ]),
    );
  });
});

describe("verifyReleaseBench", () => {
  test("skips an invalid newer result and selects the newest valid result", async () => {
    const resultsDir = await mkdtemp(
      path.join(os.tmpdir(), "wrec-release-check-"),
    );
    try {
      const valid = passingSummary();
      const invalid = passingSummary();
      invalid.binaries.reference = "";
      await writeFile(
        path.join(resultsDir, "2026-07-21-valid.json"),
        JSON.stringify(valid),
      );
      await writeFile(
        path.join(resultsDir, "2026-07-22-invalid.json"),
        JSON.stringify(invalid),
      );

      const selected = await verifyReleaseBench(
        { releaseCommit: "HEAD", repoRoot: resultsDir, resultsDir },
        () => ({ exitCode: 0, stdout: "", stderr: "" }),
      );
      expect(path.basename(selected)).toBe("2026-07-21-valid.json");
    } finally {
      await rm(resultsDir, { recursive: true, force: true });
    }
  });

  test("rejects a result when code changed after it was recorded", async () => {
    const resultsDir = await mkdtemp(
      path.join(os.tmpdir(), "wrec-release-check-"),
    );
    try {
      await writeFile(
        path.join(resultsDir, "2026-07-22-valid.json"),
        JSON.stringify(passingSummary()),
      );

      await expect(
        verifyReleaseBench(
          { releaseCommit: "HEAD", repoRoot: resultsDir, resultsDir },
          (args) => ({
            exitCode: 0,
            stdout: args[0] === "diff" ? "crates/engine/src/lib.rs\n" : "",
            stderr: "",
          }),
        ),
      ).rejects.toThrow("non-benchmark code changed after the run");
    } finally {
      await rm(resultsDir, { recursive: true, force: true });
    }
  });
});
