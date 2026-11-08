// One event-driven controller per WebView; no per-card observers or polling.
const OVERLAYS = '[role="dialog"][data-state="open"], [role="alertdialog"][data-state="open"], [role="menu"][data-state="open"], [role="listbox"][data-state="open"], [data-preview-blocker]';
const CONTROLS = '[data-action-toolbar], [data-scroll-top-btn], [data-drag-handle], button, a, input, textarea, select, [role="button"], [role="slider"]';

export function createHoverPreviewController(doc: Document, host: Window, Observer: typeof MutationObserver) {
  const cleanups = new Set<() => void>();
  let point: { x: number; y: number } | null = null;
  let requiresMove = false;
  let overlayOpen = false;
  let stop: (() => void) | undefined;

  const cancel = () => {
    point = null;
    requiresMove = true;
    for (const cleanup of cleanups) cleanup();
  };
  const hasOverlay = () => doc.querySelector(OVERLAYS) !== null;
  const isControl = (element: Element | null) => Boolean(element?.closest(CONTROLS));
  const pointer = (event: Event) => {
    const e = event as PointerEvent;
    if (e.buttons !== 0 || e.pointerType === "touch" || hasOverlay() || isControl(e.target as Element)) {
      // Already cancelled sources have no work left; don't visit every card on
      // every mouse event while moving around a button or an open menu.
      if (!requiresMove || point !== null) cancel();
      return;
    }
    point = { x: e.clientX, y: e.clientY };
    if (e.type === "pointermove") requiresMove = false;
  };
  const leave = (event: Event) => {
    if ((event as PointerEvent).relatedTarget === null) cancel();
  };

  const start = () => {
    overlayOpen = hasOverlay();
    const observer = new Observer(() => {
      const next = hasOverlay();
      if (next !== overlayOpen) { overlayOpen = next; cancel(); }
    });
    observer.observe(doc.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["data-state", "open", "role", "data-preview-blocker"] });
    const documentEvents: [string, EventListener][] = [
      ["pointermove", pointer], ["pointerover", pointer], ["pointerout", leave],
      ["pointerdown", cancel], ["pointercancel", cancel], ["contextmenu", cancel],
      ["dragstart", cancel], ["scroll", cancel], ["visibilitychange", cancel],
    ];
    for (const [name, handler] of documentEvents) doc.addEventListener(name, handler, true);
    for (const name of ["blur", "focus", "resize"]) host.addEventListener(name, cancel);
    stop = () => {
      observer.disconnect();
      for (const [name, handler] of documentEvents) doc.removeEventListener(name, handler, true);
      for (const name of ["blur", "focus", "resize"]) host.removeEventListener(name, cancel);
      point = null; requiresMove = false; overlayOpen = false;
      stop = undefined;
    };
  };
  return {
    cancel,
    register(cleanup: () => void) {
      cleanups.add(cleanup);
      if (!stop) start();
      return () => { cleanups.delete(cleanup); if (!cleanups.size) stop?.(); };
    },
    canShow(anchor: Element | null) {
      // The main Tauri window can intentionally be non-focusable. Do not use
      // document.hasFocus() here; real pointer hit-testing is the admission rule.
      if (!anchor?.isConnected || doc.visibilityState === "hidden" || requiresMove || !point || hasOverlay()) return false;
      const hit = doc.elementFromPoint(point.x, point.y);
      return hit !== null && anchor.contains(hit) && !isControl(hit);
    },
  };
}

let controller: ReturnType<typeof createHoverPreviewController> | undefined;
function current() {
  return controller ??= createHoverPreviewController(document, window, MutationObserver);
}
export const registerHoverPreviewCleanup = (cleanup: () => void) => current().register(cleanup);
export const canShowHoverPreview = (anchor: Element | null) => current().canShow(anchor);
export const cancelHoverPreviews = () => current().cancel();
