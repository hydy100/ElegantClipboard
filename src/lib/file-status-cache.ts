import { invoke } from "@tauri-apps/api/core";

export interface ItemFileStatus {
  all_exist?: boolean;
  resolved_paths?: string[];
  checks?: Record<string, { exists: boolean; is_dir: boolean }>;
  too_large?: boolean;
}

const CACHE_TTL_MS = 30_000;

const cache = new Map<number, { status: ItemFileStatus; expiresAt: number }>();
const pending = new Map<number, Array<(status: ItemFileStatus) => void>>();
const rejected = new Map<number, Array<(error: unknown) => void>>();
let flushScheduled = false;

function scheduleFlush(): void {
  if (flushScheduled) return;
  flushScheduled = true;
  queueMicrotask(() => {
    flushScheduled = false;
    void flushPending();
  });
}

async function flushPending(): Promise<void> {
  const ids = Array.from(pending.keys());
  if (ids.length === 0) return;

  try {
    const result = await invoke<Record<string, ItemFileStatus>>(
      "batch_get_item_file_status",
      { ids },
    );
    const now = Date.now();
    for (const id of ids) {
      const status = result[String(id)] ?? {};
      cache.set(id, { status, expiresAt: now + CACHE_TTL_MS });
      const waiters = pending.get(id) ?? [];
      pending.delete(id);
      rejected.delete(id);
      waiters.forEach((resolve) => resolve(status));
    }
  } catch (error) {
    for (const id of ids) {
      const errors = rejected.get(id) ?? [];
      const waiters = pending.get(id) ?? [];
      pending.delete(id);
      rejected.delete(id);
      errors.forEach((reject) => reject(error));
      waiters.forEach((resolve) => resolve({}));
    }
  }

  if (pending.size > 0) scheduleFlush();
}

/** Coalesces status checks from visible cards into one IPC call and keeps a short TTL cache. */
export function getItemFileStatus(id: number): Promise<ItemFileStatus> {
  const cached = cache.get(id);
  if (cached && cached.expiresAt > Date.now()) {
    return Promise.resolve(cached.status);
  }

  return new Promise<ItemFileStatus>((resolve, reject) => {
    const waiters = pending.get(id) ?? [];
    waiters.push(resolve);
    pending.set(id, waiters);
    const errors = rejected.get(id) ?? [];
    errors.push(reject);
    rejected.set(id, errors);
    scheduleFlush();
  });
}

export function invalidateItemFileStatus(id?: number): void {
  if (id === undefined) {
    cache.clear();
  } else {
    cache.delete(id);
  }
}
