import { describe, expect, it } from "vitest";
import { completeUpgradeBackup, createUpgradeQueue } from "./upgradeQueue";

describe("batch upgrade", () => {
  it("keeps the second and third apps and advances once per completion", () => {
    const queue = createUpgradeQueue();
    queue.replace(["second", "third"]);
    expect(queue.next(true)).toBe("second");
    expect(queue.next(true)).toBeUndefined();
    expect(queue.next(false)).toBeUndefined();
    expect(queue.next(true)).toBe("third");
    expect(queue.next(false)).toBeUndefined();
    expect(queue.next(true)).toBeUndefined();
  });

  it("drops pending upgrades when the batch is abandoned", () => {
    const queue = createUpgradeQueue();
    queue.replace(["second", "third"]);
    expect(queue.next(true)).toBe("second");
    queue.replace([]);
    queue.next(false);
    expect(queue.next(true)).toBeUndefined();
  });

  it("rejects partial backups even when a backup folder was created", () => {
    const result = { backupDir: "/backup", files: ["saved.json"] };
    expect(() => completeUpgradeBackup({ ...result, failed: ["read error"] })).toThrow();
    expect(() => completeUpgradeBackup({ ...result, refused: ["blocked path"] })).toThrow();
    expect(completeUpgradeBackup(result)).toBe("/backup");
    expect(completeUpgradeBackup({ backupDir: "", files: [], skipped: ["missing.json"] })).toBeNull();
  });
});
