import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';

class Surface {
  listeners = new Map();
  addEventListener(name, fn) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(fn);
  }
  removeEventListener(name, fn) { this.listeners.get(name)?.delete(fn); }
  fire(name, extra = {}) {
    for (const fn of this.listeners.get(name) || []) fn({ type: name, buttons: 0, pointerType: 'mouse', clientX: 12, clientY: 15, target: hit, ...extra });
  }
  count() { return [...this.listeners.values()].reduce((n, set) => n + set.size, 0); }
}
class Element {
  constructor(parent = null, control = false) { this.parent = parent; this.control = control; this.isConnected = true; }
  contains(target) { return target === this || Boolean(target?.parent && this.contains(target.parent)); }
  closest() { return this.control ? this : this.parent?.closest() || null; }
  getBoundingClientRect() { return { top: 0, height: 50 }; }
}
const document = new Surface(), window = new Surface();
let overlay = false, hit;
const observers = new Set();
class Observer {
  constructor(fn) { this.fn = fn; observers.add(this); }
  observe() {}
  disconnect() { observers.delete(this); }
}
document.body = {};
document.visibilityState = 'visible';
document.querySelector = () => overlay ? {} : null;
document.elementFromPoint = () => hit;
const changeOverlay = value => { overlay = value; for (const o of observers) o.fn(); };
const context = { exports: {}, document, window, MutationObserver: Observer, Set, console };
vm.createContext(context);
const source = fs.readFileSync('src/lib/hover-preview-interaction.ts', 'utf8');
vm.runInContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, context);
const api = context.exports;
const card = new Element(), text = new Element(card), toolbar = new Element(card, true), floatingButton = new Element(null, true), otherCard = new Element();
let cancelled = 0;
const cleanup = () => { cancelled++; };
const unsubscribe = api.registerHoverPreviewCleanup(cleanup);
const move = (target = text, extra) => { hit = target; document.fire('pointermove', extra); };

assert.equal(api.canShowHoverPreview(card), false, 'no stale hover without pointer evidence');
move(); assert.equal(api.canShowHoverPreview(card), true);
assert.equal(api.canShowHoverPreview(otherCard), false, 'hit must belong to the source, not merely be in the window');
console.log('PASS genuine pointer hit admits only its source (no document.hasFocus requirement)');

for (const target of [toolbar, floatingButton]) {
  move(); const before = cancelled; move(target);
  assert(cancelled > before);
  assert.equal(api.canShowHoverPreview(card), false);
  move(target); assert.equal(cancelled, before + 1, 'no per-card repeated cleanup on every blocked move');
}
console.log('PASS toolbar and floating button cancel previews without repeated per-move cleanup');

move(); const beforeOverlay = cancelled;
changeOverlay(true);
assert(cancelled > beforeOverlay);
assert.equal(api.canShowHoverPreview(card), false);
changeOverlay(false);
assert.equal(api.canShowHoverPreview(card), false, 'closing overlay does not revive old intent');
hit = text; document.fire('pointerover');
assert.equal(api.canShowHoverPreview(card), false, 'replayed enter alone cannot reopen preview');
move(); assert.equal(api.canShowHoverPreview(card), true);
console.log('PASS modal/menu appearance cancels pending work; close requires fresh movement');

for (const name of ['pointerdown', 'pointercancel', 'contextmenu', 'dragstart', 'scroll', 'visibilitychange']) {
  move(); document.fire(name);
  assert.equal(api.canShowHoverPreview(card), false, name);
}
for (const name of ['blur', 'focus', 'resize']) {
  move(); window.fire(name); assert.equal(api.canShowHoverPreview(card), false, name);
}
move(text, { buttons: 1 }); assert.equal(api.canShowHoverPreview(card), false);
move(text, { pointerType: 'touch' }); assert.equal(api.canShowHoverPreview(card), false);
move(); hit = null; assert.equal(api.canShowHoverPreview(card), false);
move(); card.isConnected = false; assert.equal(api.canShowHoverPreview(card), false); card.isConnected = true;
move(); document.visibilityState = 'hidden'; assert.equal(api.canShowHoverPreview(card), false); document.visibilityState = 'visible';
console.log('PASS drag, cancellation, viewport changes, stale nodes and hidden views deny admission');

const unsubscribe2 = api.registerHoverPreviewCleanup(() => {});
assert.equal(observers.size, 1);
unsubscribe2(); assert.equal(observers.size, 1);
unsubscribe(); assert.equal(observers.size, 0);
assert.equal(document.count() + window.count(), 0);
console.log('PASS shared observer/listeners are reference-counted and fully disposed');

// Run the actual React hooks with fake React/timers/IPC. Cancellation must clear
// the scheduled callback; a manually released stale callback must remain inert.
for (const [file, enter, anchor, argument] of [
  ['useTextPreview.ts', 'handleTextMouseEnter', 'textPreviewAnchorRef', { itemId: 1, textContent: 'hello', preview: 'hello', isTextLikeContent: true }],
  ['useImagePreview.ts', 'handleMouseEnter', 'containerRef', 'image.png'],
  ['useVideoPreview.ts', 'handleMouseEnter', 'containerRef', 'video.mp4'],
]) {
  const effects = [], calls = [], timers = new Map(), cancellations = new Set();
  let id = 0, allowed = true;
  const stub = {
    useCallback: fn => fn, useMemo: fn => fn(), useRef: current => ({ current }), useEffect: fn => effects.push(fn), useShallow: fn => fn,
    useClipboardStore: selector => selector({ batchMode: false }),
    useUISettings: selector => selector({ textPreviewEnabled: true, imagePreviewEnabled: true, videoPreviewEnabled: true, hoverPreviewDelay: 500, previewPosition: 'auto' }),
    canShowHoverPreview: () => allowed,
    registerHoverPreviewCleanup: fn => { cancellations.add(fn); return () => cancellations.delete(fn); },
    useNonPassiveWheel: () => () => {},
    invoke: async (...args) => { calls.push(args); return null; },
    listen: async () => () => {}, emitTo: async () => {},
    getPreviewBounds: async () => ({ maxW: 600, maxH: 400, scale: 1, side: 'left', anchorX: 600, cardCenterY: 200, monY: 0, monBottom: 400 }),
    getCurrentWindow: () => ({ outerPosition: async () => ({ x: 400, y: 0 }), outerSize: async () => ({ width: 400, height: 600 }) }),
    currentMonitor: async () => ({ position: { x: 0, y: 0 }, scaleFactor: 1 }),
    acquireImagePreviewLease: () => 1, revokeImagePreviewLease: () => {}, isImagePreviewLeaseCurrent: () => true, isImagePreviewWanted: () => false,
    getCachedTextPreviewContent: () => null, setCachedTextPreviewContent: () => {},
    sampleTextPreview: () => ({ longestVisualCols: 5, lineColumns: [5], truncated: false }),
    TEXT_PREVIEW_CHAR_WIDTH: 7, TEXT_PREVIEW_HORIZONTAL_PADDING: 30, TEXT_PREVIEW_MIN_W: 200, TEXT_PREVIEW_MAX_W: 600,
    TEXT_PREVIEW_MIN_H: 100, TEXT_PREVIEW_MAX_H: 400, TEXT_PREVIEW_MIN_CHARS_PER_LINE: 10,
    logError: () => {},
  };
  const sandbox = { exports: {}, require: () => stub, console, window: { screen: { width: 1920, height: 1080 } },
    setTimeout: fn => { timers.set(++id, fn); return id; }, clearTimeout: id => timers.delete(id),
    requestAnimationFrame: () => 1, cancelAnimationFrame: () => {},
  };
  vm.createContext(sandbox);
  vm.runInContext(ts.transpileModule(fs.readFileSync(`src/hooks/${file}`, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, sandbox);
  const hook = sandbox.exports[file.replace('.ts', '')](argument);
  const cleanups = effects.map(fn => fn());
  const node = new Element();
  if (typeof hook[anchor] === 'function') hook[anchor](node); else hook[anchor].current = node;
  hook[enter](); assert.equal(timers.size, 1, file);
  hook[enter](); assert.equal(timers.size, 1, 'mousemove does not restart the hover delay');
  const stale = [...timers.values()][0];
  allowed = false; for (const cancel of cancellations) cancel();
  assert.equal(timers.size, 0); await stale();
  assert(!calls.some(([name]) => name.startsWith('show_')), 'cancelled delayed callback emitted a show');
  allowed = true; hook[enter](); assert.equal(timers.size, 1);
  const pending = [...timers.values()][0]; timers.clear();
  const inFlight = pending();
  allowed = false; for (const cancel of cancellations) cancel();
  await inFlight;
  await new Promise(resolve => setImmediate(resolve));
  assert(!calls.some(([name]) => name.startsWith('show_')), 'async content/bounds result revived a cancelled preview');
  allowed = true; hook[enter](); assert.equal(timers.size, 1);
  for (const fn of cleanups) fn?.();
  assert.equal(cancellations.size, 0);
  console.log(`PASS ${file}: pending callback cancelled, late callback inert, fresh hover resumes, cleanup unregisters`);
}
console.log('RESULT 8/8 groups passed');
