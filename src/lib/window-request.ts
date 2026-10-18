import { invoke } from "@tauri-apps/api/core";
import { logError } from "@/lib/logger";

interface WindowOpenRequest {
  requestId: number;
  payload: string;
}

declare global {
  interface Window {
    __EC_HANDLE_OPEN_REQUEST__?: (request: WindowOpenRequest) => Promise<void>;
  }
}

/** Install before managed_window_ready; completion includes payload work and paint. */
export function registerWindowRequestHandler(
  callback: (payload: string) => void | Promise<void>,
): () => void {
  let disposed = false;
  let pending = Promise.resolve();
  let latestRequestId = 0;
  let cancelRenderWait: (() => void) | undefined;
  const isCurrent = () => !disposed && window.__EC_HANDLE_OPEN_REQUEST__ === handler;
  const handle = async ({ requestId, payload }: WindowOpenRequest, superseded: boolean): Promise<void> => {
    if (!isCurrent()) return;
    let error: string | null = null;
    try {
      if (superseded) throw new Error("superseded");
      await callback(payload);
      if (!isCurrent()) return;
      await new Promise<void>((resolve) => {
        let frame = 0;
        let settled = false;
        const finish = () => {
          if (settled) return;
          settled = true;
          clearTimeout(timeout);
          cancelAnimationFrame(frame);
          cancelRenderWait = undefined;
          resolve();
        };
        // Hidden/occluded WebViews may suspend rAF. Native ack remains the
        // authority on visibility; do not turn a user hide into watchdog recovery.
        const timeout = setTimeout(finish, 300);
        cancelRenderWait = finish;
        frame = requestAnimationFrame(() => {
          if (settled) return;
          frame = requestAnimationFrame(finish);
        });
      });
    } catch (cause) {
      error = superseded ? "superseded" : String(cause);
    }
    if (!isCurrent()) return;
    // eval callers need not consume the returned promise, including on IPC failure.
    try {
      await invoke("window_request_ack", { requestId, error });
    } catch (cause) {
      logError("Failed to acknowledge window open request:", cause);
    }
  };
  // Keep payload application, rendering, and ack atomic relative to the next ID.
  const handler = (request: WindowOpenRequest): Promise<void> => {
    const superseded = request.requestId < latestRequestId;
    latestRequestId = Math.max(latestRequestId, request.requestId);
    pending = pending.then(() => handle(request, superseded));
    return pending;
  };
  window.__EC_HANDLE_OPEN_REQUEST__ = handler;
  return () => {
    disposed = true;
    cancelRenderWait?.();
    if (window.__EC_HANDLE_OPEN_REQUEST__ === handler) {
      delete window.__EC_HANDLE_OPEN_REQUEST__;
    }
  };
}
