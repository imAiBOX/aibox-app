import { describe, expect, it } from "vitest";
import routeFixture from "./plan-route-fixture.json";
import { buildPlan } from "./plan";
import type { AppManifest, ScanReport } from "../net/client";

// JSON inference widens registry enum strings; the fixture contains the route/package projection used here.
const fixture = routeFixture as unknown as { apps: AppManifest[] };
const scan = (platform: string, arch: string) => ({
  os: { platform, arch }, node: { installed: false }, git: { installed: false }, wsl: { installed: false }, installedApps: [],
}) as unknown as ScanReport;

describe("verified target route fixture", () => {
  it("fixture routes select the correct machine-specific target without external registry", () => {
    const failures: string[] = [];
    for (const manifest of fixture.apps) {
      for (const [region, routes] of Object.entries(manifest.routes ?? {})) for (const route of routes) {
        const plan = manifest.routePlans?.[route];
        if (!plan) { failures.push(`${manifest.id}/${region}/${route}: route plan missing`); continue; }
        for (const [platform, arch] of [["windows", "x64"], ["windows", "arm64"], ["macos", "x64"], ["macos", "arm64"]]) {
          const built = buildPlan(manifest, { platform, arch, region: region as "global" | "cn", scan: scan(platform, arch) });
          const chosen = plan.steps.map((id) => manifest.packages.find((p) => p.id === id)).filter((p) => p && (p.platforms.length === 0 || p.platforms.includes(platform)) && (!p.arch || p.arch === "any" || p.arch === arch));
          const target = plan.target ?? chosen[chosen.length - 1]?.id;
          const targetTasks = built.tasks.filter((t) => t.role === "target");
          if (built.route !== route) failures.push(`${manifest.id}/${region}/${route}/${platform}-${arch}: selected route ${built.route}`);
          if (chosen.length && (targetTasks.length !== 1 || targetTasks[0].id !== target)) failures.push(`${manifest.id}/${region}/${route}/${platform}-${arch}: expected target ${target}, got ${targetTasks.map((t) => t.id).join(",")}`);
        }
      }
    }
    expect(failures).toEqual([]);
  });
});
