import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const read = (path) => fs.readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const helper = read("src/lib/window-request.ts");
const compile = (source) => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
}).outputText;
const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

function harness() {
  const frames = [], acks = [], errors = [], timers = new Map();
  let nextId = 0;
  let failAck = false;
  const context = {
    exports: {}, window: {},
    requestAnimationFrame: (fn) => { fn.id = ++nextId; frames.push(fn); return fn.id; },
    cancelAnimationFrame: (id) => { const i = frames.findIndex((fn) => fn.id === id); if (i >= 0) frames.splice(i, 1); },
    setTimeout: (fn, ms) => {
      assert.ok(ms > 0 && ms <= 500, `render fallback must be bounded, got ${ms}`);
      const id = ++nextId; timers.set(id, fn); return id;
    },
    clearTimeout: (id) => timers.delete(id),
    require: (id) => {
      if (id === "@tauri-apps/api/core") return {
        invoke: async (command, args) => {
          assert.equal(command, "window_request_ack");
          acks.push({ ...args });
          if (failAck) throw new Error("IPC disconnected");
        },
      };
      if (id === "@/lib/logger") return { logError: (...args) => errors.push(args) };
      throw new Error(`Unexpected import ${id}`);
    },
  };
  vm.runInNewContext(compile(helper), context);
  return {
    context, frames, acks, errors, timers,
    register: context.exports.registerWindowRequestHandler,
    send: (requestId, payload = "payload") => context.window.__EC_HANDLE_OPEN_REQUEST__({ requestId, payload }),
    async frame() { assert.ok(frames.length); frames.shift()(0); await tick(); },
    failAck: () => { failAck = true; },
    async timeout() { const [id, fn] = timers.entries().next().value; timers.delete(id); fn(); await tick(); },
  };
}

{
  const h = harness(), work = deferred(), seen = [];
  const cleanup = h.register(async (payload) => { seen.push(payload); await work.promise; });
  const done = h.send(41, "first");
  await tick();
  assert.deepEqual(seen, ["first"]);
  assert.equal(h.frames.length, 0);
  assert.equal(h.acks.length, 0);
  work.resolve(); await tick();
  await h.frame(); assert.equal(h.acks.length, 0);
  await h.frame(); await done;
  assert.deepEqual(h.acks, [{ requestId: 41, error: null }]);
  cleanup(); assert.equal(h.context.window.__EC_HANDLE_OPEN_REQUEST__, undefined);
  console.log("PASS callback completion and exactly two render frames before ack");
}

{
  const h = harness(), work = deferred(), seen = [];
  h.register(async (payload) => { seen.push(payload); if (payload === "first") await work.promise; });
  const first = h.send(51, "first"), second = h.send(52, "second");
  await tick(); assert.deepEqual(seen, ["first"]);
  work.resolve(); await tick();
  await h.frame(); assert.deepEqual(seen, ["first"]);
  await h.frame(); await first;
  assert.deepEqual(seen, ["first", "second"]);
  assert.deepEqual(h.acks.map((a) => a.requestId), [51]);
  await h.frame(); await h.frame(); await second;
  assert.deepEqual(h.acks, [{ requestId: 51, error: null }, { requestId: 52, error: null }]);
  console.log("PASS overlapping requests serialize through independent acks");
}

for (const asyncFailure of [false, true]) {
  const h = harness();
  h.register((payload) => {
    if (payload === "ok") return;
    if (asyncFailure) return Promise.reject(new Error("load failed"));
    throw new Error("load failed");
  });
  await h.send(61);
  assert.deepEqual(h.acks, [{ requestId: 61, error: "Error: load failed" }]);
  const next = h.send(62, "ok"); await tick();
  await h.frame(); await h.frame(); await next;
  assert.equal(h.acks[1].error, null);
}
console.log("PASS sync/async errors ack and do not poison the queue");

for (const stage of ["callback", "frame", "queued"]) {
  const h = harness(), work = deferred(), seen = [];
  const cleanup = h.register(async (payload) => { seen.push(payload); await work.promise; });
  const old = h.context.window.__EC_HANDLE_OPEN_REQUEST__;
  const first = h.send(71, "first");
  const second = h.send(72, "second");
  if (stage !== "queued") await tick();
  if (stage === "frame") { work.resolve(); await tick(); await h.frame(); }
  cleanup(); work.resolve(); await tick();
  while (h.frames.length) await h.frame();
  await Promise.all([first, second, old({ requestId: 73, payload: "stale" })]);
  assert.equal(h.acks.length, 0);
  assert.deepEqual(seen, stage === "queued" ? [] : ["first"]);
}
console.log("PASS dispose skips queued/stale callbacks and suppresses all in-flight acks");

{
  const h = harness();
  const oldCleanup = h.register(() => {});
  const newCleanup = h.register(() => {});
  const current = h.context.window.__EC_HANDLE_OPEN_REQUEST__;
  oldCleanup(); assert.equal(h.context.window.__EC_HANDLE_OPEN_REQUEST__, current);
  h.failAck();
  const done = h.send(81); await tick(); await h.frame(); await h.frame(); await done;
  assert.equal(h.errors.length, 1);
  newCleanup();
  console.log("PASS replacement ownership and IPC rejection containment");
}

// Exercise the actual screenshot effect with controlled Image and native show completion.
function screenshotHarness() {
  const h = harness(), images = [], calls = [], show = deferred();
  const canvas = { width: 0, height: 0, getContext: () => ({
    clearRect: () => calls.push("clear"), drawImage: () => calls.push("draw"),
  }) };
  const context = {
    ...h.context,
    registerWindowRequestHandler: h.register,
    canvasRef: { current: canvas }, imgRef: { current: null },
    cancelScreenshotRef: { current: null },
    setDrawing() {}, setStartPos() {}, setImagePath: (path) => calls.push(["path", path]),
    useEffect: (fn) => { context.cleanup = fn(); },
    useOcrSettings: { getState: () => ({ loadSettings: async () => {} }) },
    invoke: async (command) => {
      calls.push(command);
      if (command === "managed_window_ready") {
        assert.equal(typeof h.context.window.__EC_HANDLE_OPEN_REQUEST__, "function");
      } else if (command === "ocr_screenshot_ready") await show.promise;
      else if (command === "hide_ocr_screenshot_window") return;
      else throw new Error(command);
    },
    convertFileSrc: (path) => `asset://${path}`,
    logError: (...args) => h.errors.push(args),
    Image: class {
      width = 1920; height = 1080;
      constructor() { images.push(this); }
    },
  };
  const source = read("src/pages/OcrScreenshot.tsx");
  const effect = source.slice(source.indexOf("  useEffect(() => {"), source.indexOf("  const redraw ="));
  vm.runInNewContext(compile(effect), context);
  const closeStart = source.indexOf("  const closeWindow =");
  const closeEnd = source.indexOf("\n  };", closeStart) + "\n  };".length;
  vm.runInNewContext(compile(source.slice(closeStart, closeEnd) + "\nglobalThis.closeWindow = closeWindow;"), context);
  return { ...h, images, calls, show, cleanup: context.cleanup, canvas,
    cancel: () => context.cancelScreenshotRef.current(), close: context.closeWindow };
}

{
  const h = screenshotHarness();
  assert.deepEqual(h.calls, ["managed_window_ready"]);
  const first = h.send(91, "screen.png"), second = h.send(92, "screen.png");
  await tick(); assert.equal(h.images.length, 1); assert.equal(h.acks.length, 0);
  h.images[0].onload(); await tick();
  assert.equal(h.canvas.width, 1920);
  assert.ok(h.calls.indexOf("draw") < h.calls.indexOf("ocr_screenshot_ready"));
  assert.equal(h.frames.length, 0); assert.equal(h.acks.length, 0);
  h.show.resolve(); await tick(); await h.frame(); await h.frame(); await first;
  assert.equal(h.images.length, 2);
  assert.notEqual(h.images[0].src, h.images[1].src);
  h.images[1].onload(); await tick(); await h.frame(); await h.frame(); await second;
  assert.deepEqual(h.acks, [{ requestId: 91, error: null }, { requestId: 92, error: null }]);
  h.cleanup();
  console.log("PASS screenshot load -> draw -> native show -> render -> ack; repeated path reloads serially");
}

{
  const h = screenshotHarness();
  const failure = h.send(101, "bad.png"); await tick(); h.images[0].onerror(); await failure;
  assert.match(h.acks[0].error, /Failed to load screenshot/);
  assert.ok(!h.calls.includes("ocr_screenshot_ready"));
  const pending = h.send(102, "pending.png"); await tick();
  const staleLoad = h.images[1].onload;
  h.cleanup(); await pending;
  staleLoad(); await tick();
  assert.equal(h.images[1].onload, null);
  assert.equal(h.images[1].onerror, null);
  assert.equal(h.acks.length, 1);
  assert.ok(!h.calls.includes("draw"));
  assert.ok(!h.calls.includes("ocr_screenshot_ready"));
  console.log("PASS screenshot image error ack and unmount cancellation without stale ack");
}

for (const page of ["Settings", "TextEditor", "TranslateResult", "OcrResult", "OcrScreenshot"]) {
  const source = read(`src/pages/${page}.tsx`);
  assert.ok(source.indexOf("registerWindowRequestHandler(") < source.indexOf('invoke("managed_window_ready")'));
  assert.doesNotMatch(source, /get_pending_(translate|ocr)_text|(?:translate-result|ocr-result|ocr-screenshot)-update/);
}
console.log("PASS all window handlers register before managed readiness; legacy loaders removed");

for (const page of ["TranslateResult", "OcrResult"]) {
  const h = harness(), network = deferred(), translated = [], copied = [], states = {};
  const context = {
    registerWindowRequestHandler: h.register,
    translationRequest: { current: 0 },
    doTranslate: (text) => { translated.push(text); return network.promise; },
    useEffect: (fn) => { context.cleanup = fn(); },
    useOcrSettings: { getState: () => ({ autoCopy: true, autoTranslate: true, recordOcrCopy: true }) },
    useTranslateSettings: { getState: () => ({ enabled: true }) },
    invoke: async (command, args) => { copied.push([command, args.text, args.record]); },
    logError: (...args) => h.errors.push(args),
  };
  for (const state of ["Text", "Copied", "TranslatedCopied", "Translating", "TranslatedText", "TranslateError", "OcrRecognizing", "Editing"]) {
    context[`set${state}`] = (value) => { states[state] = value; };
  }
  const source = read(`src/pages/${page}.tsx`);
  const start = source.lastIndexOf("  useEffect(() => {", source.indexOf("const cleanup = registerWindowRequestHandler"));
  const end = source.indexOf("  }, [doTranslate]);", start) + "  }, [doTranslate]);".length;
  vm.runInNewContext(compile(source.slice(start, end)), context);
  const done = h.send(111, "recognized text"); await tick(); await h.frame(); await h.frame(); await done;
  assert.deepEqual(translated, ["recognized text"]);
  assert.equal(states.Text, "recognized text");
  assert.equal(states.TranslatedText, "");
  assert.equal(h.acks[0].error, null); // Network promise is deliberately unresolved.
  const empty = h.send(112, ""); await tick(); await h.frame(); await h.frame(); await empty;
  if (page === "OcrResult") {
    assert.equal(states.OcrRecognizing, true);
    assert.deepEqual(copied, [["write_text_to_clipboard", "recognized text", true]]);
    assert.deepEqual(translated, ["recognized text"]);
  } else assert.deepEqual(translated, ["recognized text", ""]);
  context.cleanup(); network.resolve();
}
console.log("PASS translation is fire-and-forget; OCR recognizing, auto-copy and auto-translate preserved");

{
  const h = harness();
  h.register(() => {});
  const old = h.send(121); await tick();
  h.register(() => {});
  await h.frame(); await h.frame(); await old;
  assert.equal(h.acks.length, 0);
  console.log("PASS replaced registration cannot ack even without prior cleanup");
}

{
  const h = harness();
  h.register(() => {});
  const stalled = h.send(131), next = h.send(132); await tick();
  await h.timeout(); await stalled;
  assert.equal(h.acks[0].error, null); // Native visibility verification decides success/cancellation.
  await h.frame(); await h.frame(); await next;
  assert.equal(h.acks[1].error, null);
  assert.equal(h.timers.size, 0);
  console.log("PASS throttled rAF timer releases queue for native visibility verification");
}

{
  const h = screenshotHarness();
  const loading = h.send(141, "screen.png"); await tick(); h.close(); await loading;
  assert.equal(h.images[0].onload, null);
  assert.ok(!h.calls.includes("ocr_screenshot_ready"));
  assert.match(h.acks[0].error, /cancelled/);
  h.cleanup();
  console.log("PASS OCR close during image loading cancels without drawing or showing");
}

{
  const h = screenshotHarness();
  const first = h.send(171, "old.png"), second = h.send(172, "next.png");
  await tick();
  const staleLoad = h.images[0].onload;
  h.close(); await first;
  assert.match(h.acks[0].error, /cancelled/);
  assert.equal(h.images.length, 2);
  staleLoad(); await tick();
  assert.ok(!h.calls.includes("draw"));
  assert.ok(!h.calls.includes("ocr_screenshot_ready"));
  h.images[1].onload(); h.show.resolve(); await tick();
  await h.frame(); await h.frame(); await second;
  assert.deepEqual(h.acks.map(({ requestId, error }) => [requestId, error === null]), [[171, false], [172, true]]);
  h.cleanup();
  console.log("PASS actual closeWindow cancels only active ID; stale onload is inert and queued request succeeds");
}

{
  const h = screenshotHarness();
  const showing = h.send(151, "screen.png"); await tick(); h.images[0].onload(); await tick();
  h.cancel(); h.show.resolve(); await showing;
  assert.ok(h.calls.includes("hide_ocr_screenshot_window"));
  assert.match(h.acks[0].error, /cancelled/);
  h.cleanup();
  console.log("PASS OCR close racing native show re-hides and reports cancellation");
}

{
  const h = harness();
  const cleanup = h.register(() => {});
  const first = h.send(161), second = h.send(162); await tick();
  const lateTimer = [...h.timers.values()][0];
  const lateFrame = h.frames[0];
  assert.equal(h.timers.size, 1);
  cleanup(); await Promise.all([first, second]);
  assert.equal(h.timers.size, 0);
  assert.equal(h.frames.length, 0);
  assert.equal(h.acks.length, 0);
  // Even a task already dispatched by the browser cannot acknowledge after cleanup.
  lateTimer(); lateFrame();
  await tick();
  assert.equal(h.frames.length, 0);
  assert.equal(h.acks.length, 0);
  console.log("PASS no-frame cleanup cancels timer/frame, releases queue and suppresses delayed ack");
}

{
  const h = harness();
  let displayed = "";
  h.register((payload) => { displayed = payload; });
  const newer = h.send(2, "new"), older = h.send(1, "old");
  await tick(); await h.frame(); await h.frame(); await Promise.all([newer, older]);
  assert.equal(displayed, "new");
  assert.equal(h.acks.length, 2);
  assert.deepEqual(h.acks[0], { requestId: 2, error: null });
  assert.equal(h.acks[1].requestId, 1);
  assert.equal(h.acks[1].error, "superseded");
  console.log("PASS out-of-order ID 2 then ID 1 preserves newer payload and acknowledges both");
}

for (const scenario of ["success", "failure", "missing", "invalid", "dispose"]) {
  const h = harness(), readItem = deferred(), states = {}, cleanups = [];
  let reads = 0;
  const context = {
    id: scenario === "invalid" ? 0 : 7,
    useRef: (current) => ({ current }),
    useEffect: (fn) => { cleanups.push(fn()); },
    registerWindowRequestHandler: h.register,
    invoke: (command, args) => {
      assert.equal(command, "get_clipboard_item"); assert.equal(args.id, 7);
      reads++; return readItem.promise;
    },
  };
  for (const state of ["Loading", "LoadError", "Text", "OriginalText"]) {
    context[`set${state}`] = (value) => { states[state] = value; };
  }
  const source = read("src/pages/TextEditor.tsx");
  const start = source.indexOf("  const loadResultRef =");
  const end = source.indexOf("  }), [id]);", start) + "  }), [id]);".length;
  vm.runInNewContext(compile(source.slice(start, end)), context);
  const done = h.send(181); await tick();
  if (scenario !== "invalid") {
    assert.equal(h.acks.length, 0);
    assert.equal(h.frames.length, 0); // No ack/render wait while content is still pending.
  }
  if (scenario === "dispose") cleanups.forEach((cleanup) => cleanup?.());
  if (scenario === "failure") readItem.reject(new Error("database read failed"));
  else readItem.resolve(scenario === "missing" ? null : { text_content: "editable text" });
  await tick();
  if (scenario === "success") {
    assert.equal(states.Text, "editable text");
    assert.equal(states.OriginalText, "editable text");
    await h.frame(); await h.frame();
  }
  await done;
  if (scenario === "dispose") {
    assert.equal(h.acks.length, 0); assert.equal(states.Text, undefined);
  } else {
    assert.equal(states.Loading, false);
    assert.equal(h.acks.length, 1);
    if (scenario === "success") assert.equal(h.acks[0].error, null);
    else { assert.ok(states.LoadError); assert.ok(h.acks[0].error.includes(states.LoadError)); }
    cleanups.forEach((cleanup) => cleanup?.());
  }
  assert.equal(reads, scenario === "invalid" ? 0 : 1);
  assert.match(source, /loadError \? \([\s\S]*role="alert"/);
}
console.log("PASS editor waits for content; read/missing/invalid errors reach UI and ack; dispose stays inert");
