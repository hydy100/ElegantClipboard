// Exercise the actual debounce declarations and hook with deterministic timers.
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import ts from 'typescript';
const root = path.resolve(process.argv[2] || '.');
let failures = 0;
for (const [relative, name, marker, keys] of [
  ['src/components/settings/TranslateTab.tsx', 'debounced', '// debounce helpers', 'setter'],
  ['src/hooks/useWebDAVSettings.ts', 'debouncedSave', '  const debouncedSave =', 'string'],
]) {
  for (const phase of ['timers', 'unmount', 'pagehide']) {
    const timers = new Map(), effects = [], listeners = new Map(), writes = [];
    let id = 0;
    const context = { exports: {}, useRef: current => ({current}), useCallback: fn => fn,
      useEffect: fn => effects.push(fn), saveSetting: (k,v) => writes.push([k,v]),
      setTimeout: fn => { timers.set(++id,fn); return id; }, clearTimeout: id => timers.delete(id),
      window: {addEventListener: (k,fn) => listeners.set(k,fn), removeEventListener: k => listeners.delete(k)},
    };
    context.require = () => context;
    vm.createContext(context);
    const hookFile = path.join(root, 'src/hooks/useKeyedDebounce.ts');
    if (fs.existsSync(hookFile)) {
      vm.runInContext(ts.transpileModule(fs.readFileSync(hookFile,'utf8'), {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText, context);
      context.useKeyedDebounce = context.exports.useKeyedDebounce;
    }
    const source = fs.readFileSync(path.join(root,relative),'utf8');
    const start = source.indexOf(marker);
    const end = source.indexOf('  useEffect(', start);
    assert(start >= 0 && end > start);
    const setup = name === 'debouncedSave'
      ? (source.includes('const scheduleSave = useKeyedDebounce') ? 'const scheduleSave = useKeyedDebounce();' : 'const saveTimerRef = useRef(null);') : '';
    vm.runInContext(ts.transpileModule(setup + source.slice(start,end) + `\nglobalThis.schedule = ${name};`, {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText, context);
    const cleanups = effects.map(fn => fn());
    const a = keys === 'setter' ? v => writes.push(['a',v]) : 'a';
    const b = keys === 'setter' ? v => writes.push(['b',v]) : 'b';
    context.schedule(a, 'old'); context.schedule(b,'other'); context.schedule(a,'latest');
    if (phase === 'unmount') cleanups.forEach(fn => fn());
    else if (phase === 'pagehide') listeners.get('pagehide')?.();
    else { const pending=[...timers.values()];timers.clear();pending.forEach(fn=>fn()); }
    try {
      assert.deepEqual(writes.sort(), [['a','latest'],['b','other']]);
      cleanups.forEach(fn => fn());
      assert.equal(writes.length,2,'cleanup duplicated writes');
      console.log(`PASS ${name} ${phase}: both fields saved once, latest value retained`);
    } catch(e) { ++failures; console.log(`FAIL ${name} ${phase}: ${JSON.stringify(writes)}`); }
  }
}
console.log(`RESULT ${6-failures}/6 passed`);
process.exitCode = failures ? 1 : 0;
