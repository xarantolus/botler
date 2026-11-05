import { describe, expect, it } from "vitest";
import { addDays, presetRange, todayIn, tokens } from "./costs";

describe("cost spans", () => {
  it("counts days across months and years", () => {
    expect(addDays("2026-11-01", -1)).toBe("2026-10-31");
    expect(addDays("2026-12-31", 1)).toBe("2027-01-01");
  });

  it("ends the presets today, or last month's last day", () => {
    const today = "2026-03-05";
    expect(presetRange("7d", today)).toEqual({ from: "2026-02-27", to: today });
    expect(presetRange("30d", today)).toEqual({ from: "2026-02-04", to: today });
    expect(presetRange("month", today)).toEqual({ from: "2026-03-01", to: today });
    expect(presetRange("last-month", today)).toEqual({ from: "2026-02-01", to: "2026-02-28" });
    expect(presetRange("last-month", "2026-01-10")).toEqual({ from: "2025-12-01", to: "2025-12-31" });
  });

  it("takes today in the hub's time zone", () => {
    const late = new Date("2026-11-04T23:30:00Z");
    expect(todayIn("Europe/Berlin", late)).toBe("2026-11-05");
    expect(todayIn("America/New_York", late)).toBe("2026-11-04");
  });

  it("shortens token counts", () => {
    expect([tokens(950), tokens(12_400), tokens(3_250_000)]).toEqual(["950", "12k", "3.3M"]);
  });
});
