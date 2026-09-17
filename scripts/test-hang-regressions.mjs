// Runs actual TypeScript modules with deterministic Tauri/React boundary mocks.
// Optional source root allows the identical tests to run against a source backup.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';

const root = path.resolve(process.argv[2] || '.');
function load(relative, mocks, globals = {}) {
  const file = path.join(root, relative);
  const js = ts.transpileModule(fs.readFileSync(file, 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const exports = {};
  const context = { exports, require: name => {
    if (!(name in mocks)) throw new Error(`Unmocked import: ${name}`);
    return mocks[name];
  }, setTimeout, clearTimeout, console, ...globals };
  vm.runInNewContext(js, context, { filename: file });
  return exports;
}
const flightPath = path.join(root, 'src/lib/single-flight.ts');
const flight = fs.existsSync(flightPath) ? load('src/lib/single-flight.ts', {}) : null;
const tests = [];
tests.push(['window refresh is independent of a hung filesystem check', async () => {
  const effects = [], listeners = new Map();
  let refreshes = 0;
  const invoke = command => command === 'refresh_files_validity'
    ? new Promise(() => {}) : Promise.resolve(false);
  const { useWindowLifecycle } = load('src/hooks/useWindowLifecycle.ts', {
    react: { useCallback: f => f, useEffect: f => effects.push(f), useRef: v => ({ current: v }), useState: v => [v, () => {}] },
    '@tauri-apps/api/core': { invoke },
    '@tauri-apps/api/event': { listen: (name, fn) => { listeners.set(name, fn); return Promise.resolve(() => listeners.delete(name)); } },
    '@/hooks/useInputFocus': { focusWindowImmediately: () => Promise.resolve() },
    '@/lib/logger': { logError() {} },
    '@/lib/single-flight': flight,
    '@/stores/clipboard': { useClipboardStore: { getState: () => ({ batchMode: false }) } },
    '@/stores/ui-settings': { useUISettings: { getState: () => ({ keyboardNavigation: false }), persist: { rehydrate() {} } } },
  }, { document: { documentElement: { dataset: {} }, addEventListener() {}, removeEventListener() {} }, window: { addEventListener() {}, removeEventListener() {} } });
  useWindowLifecycle({
    autoResetState: false, searchAutoClear: false, searchAutoFocus: false, cardDensity: 'normal',
    tagsViewOpen: false, selectedCategory: null, inputRef: { current: null },
    fetchItems: async () => { ++refreshes; }, refresh: async () => { ++refreshes; },
    resetView: async () => {}, setBatchMode() {}, setSearchQuery() {}, setTagsViewOpen() {}, dismissOverlays: () => false,
  });
  const cleanup = effects.map(f => f()).filter(Boolean);
  try {
    listeners.get('window-shown')();
    await Promise.resolve();
    assert.equal(refreshes, 1, 'list waited on refresh_files_validity');
  } finally { cleanup.forEach(f => f()); }
}]);
tests.push(['first translation waits for settings instead of reporting disabled', async () => {
  let state;
  state = { loaded: false, enabled: false, languageMode: 'auto', proxyMode: 'none',
    loadSettings: async () => { state = { ...state, loaded: true, enabled: true, provider: 'microsoft' }; } };
  const { translateText } = load('src/lib/translate.ts', {
    '@tauri-apps/api/core': { invoke: async () => 'translated fixture' },
    '@/lib/logger': { logError() {} }, '@/lib/single-flight': flight,
    '@/stores/translate-settings': { useTranslateSettings: { getState: () => state } },
  });
  assert.equal(await translateText('selected fixture'), 'translated fixture');
}]);
if (flight) {
  tests.push(['single-flight coalesces overlap, retries failures, and does not cache results', async () => {
    let calls = 0, finish;
    const run = flight.singleFlight(() => { ++calls; return new Promise(resolve => { finish = resolve; }); });
    const first = run(), second = run();
    assert.equal(first, second);
    await Promise.resolve();
    assert.equal(calls, 1);
    finish(3);
    assert.equal(await first, 3);
    const third = run();
    await Promise.resolve();
    assert.equal(calls, 2);
    finish(4);
    assert.equal(await third, 4);
    let failedCalls = 0;
    const fail = flight.singleFlight(async () => { ++failedCalls; throw new Error('fixture failure'); });
    await assert.rejects(fail());
    await assert.rejects(fail());
    assert.equal(failedCalls, 2);
  }]);
}
let failed = 0;
for (const [name, test] of tests) {
  try { await test(); console.log(`PASS ${name}`); }
  catch (error) { ++failed; console.log(`FAIL ${name}: ${error.message}`); }
}
console.log(`${tests.length - failed} passed; ${failed} failed`);
process.exitCode = failed ? 1 : 0;
