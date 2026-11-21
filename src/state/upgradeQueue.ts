import type { ManageBackupResult } from "../net/client";

export function createUpgradeQueue() {
  let remaining: string[] = [];
  let advanced = false;
  return {
    replace(ids: string[]) {
      remaining = [...ids];
    },
    next(done: boolean) {
      if (!done) {
        advanced = false;
        return undefined;
      }
      if (advanced) return undefined;
      const id = remaining.shift();
      if (id) advanced = true;
      return id;
    },
  };
}

export function completeUpgradeBackup(result: ManageBackupResult): string | null {
  if (result.failed?.length || result.refused?.length) {
    throw new Error("Incomplete upgrade backup");
  }
  return result.backupDir || null;
}
