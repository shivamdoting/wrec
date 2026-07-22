import type { OverallStatus, SuiteName } from "./gates";

export const nullableNumber = (value: unknown) => {
  if (value === null || value === undefined) {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
};

export const benchmarkExitCode = (suite: SuiteName, status: OverallStatus) =>
  suite === "release" && status !== "pass" ? 1 : 0;
