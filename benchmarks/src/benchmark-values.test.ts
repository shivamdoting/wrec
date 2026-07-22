import { describe, expect, test } from "bun:test";
import { benchmarkExitCode, nullableNumber } from "./benchmark-values";

describe("nullableNumber", () => {
  test("preserves missing metrics instead of coercing them to zero", () => {
    expect(nullableNumber(null)).toBeNull();
    expect(nullableNumber(undefined)).toBeNull();
  });

  test("accepts finite numeric values", () => {
    expect(nullableNumber(0)).toBe(0);
    expect(nullableNumber("42.5")).toBe(42.5);
    expect(nullableNumber("not-a-number")).toBeNull();
  });
});

describe("benchmarkExitCode", () => {
  test("blocks releases that do not pass", () => {
    expect(benchmarkExitCode("release", "pass")).toBe(0);
    expect(benchmarkExitCode("release", "fail")).toBe(1);
    expect(benchmarkExitCode("release", "inconclusive")).toBe(1);
  });

  test("keeps smoke runs informational", () => {
    expect(benchmarkExitCode("smoke", "pass")).toBe(0);
    expect(benchmarkExitCode("smoke", "fail")).toBe(0);
  });
});
