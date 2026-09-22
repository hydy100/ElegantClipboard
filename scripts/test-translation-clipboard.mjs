import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
const root = path.resolve(process.argv[2] || '.');
let failures = 0;
const source = fs.readFileSync(path.join(root, 'src/components/ClipboardItemCard.tsx'), 'utf8');
const body = source.slice(source.indexOf('  const handleTranslate ='), source.indexOf('  const handleCopyTranslation ='));
const full = 'Better Minimap 中文😀\n'.repeat(300) + 'END';
for (const scenario of ['list', 'search', 'stale', 'deleted', 'read-error', 'empty', 'double-click']) {
  const translated = [], errors = [], loading = [];
  let reads = 0;
  const context = {
    useCallback: fn => fn,
    item: { id: 17, text_content: scenario === 'stale' ? 'old' : null, preview: scenario === 'search' ? 'keyword...' : full.slice(0, 200) + '...' },
    translatingRef: { current: false },
    setTranslating: value => loading.push(value), setTranslateError: value => { if (value) errors.push(value); }, setTranslatedText() {},
    invoke: async (command, args) => {
      assert.equal(command, 'get_clipboard_item'); assert.equal(args.id, 17); reads++;
      if (scenario === 'read-error') throw new Error('read failed');
      return scenario === 'deleted' ? null : { text_content: scenario === 'empty' ? null : full };
    },
    translateText: async text => { translated.push(text); return 'translation'; },
  };
  vm.runInNewContext(ts.transpileModule(body + '\nglobalThis.run = handleTranslate;', {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText, context);
  await Promise.all(scenario === 'double-click' ? [context.run(), context.run()] : [context.run()]);
  try {
    assert.equal(reads, 1);
    if (['deleted', 'read-error', 'empty'].includes(scenario)) {
      assert.equal(translated.length, 0); assert.equal(errors.length, 1);
    } else assert.deepEqual(translated, [full]);
    assert.equal(context.translatingRef.current, false);
    assert.equal(loading.at(-1), false);
    console.log(`PASS translation ${scenario}`);
  } catch { failures++; console.log(`FAIL translation ${scenario}`); }
}
// Compile the production sequence tracker without GUI/dependency setup.
const rustSource = fs.readFileSync(path.join(root, 'src-tauri/src/clipboard/windows_monitor.rs'), 'utf8');
const tracker = rustSource.slice(rustSource.indexOf('struct SequenceTracker'), rustSource.indexOf('pub(super) fn run('));
const constants = rustSource.match(/^const .*$/gm).join('\n');
const harness = `use std::time::{Duration, Instant};
extern crate self as tracing;
#[macro_export] macro_rules! warn { ($($t:tt)*) => {} }
${constants}\n${tracker}
fn main() {
 let start = Instant::now();
 let mut state = SequenceTracker::new(10);
 let mut attempts = 0;
 for second in 0..20 {
  let now = start + Duration::from_secs(second);
  if state.should_read(11, false, now) { attempts += 1; state.finish(11, second >= 10, now); }
 }
 assert!(attempts > 5, "delayed clipboard was abandoned after five failures");
 assert!(!state.should_read(11, false, start + Duration::from_secs(30)), "successful clipboard must be acknowledged");
 assert!(state.should_read(12, false, start + Duration::from_secs(30)));
 state.finish(12, false, start + Duration::from_secs(30));
 assert!(state.should_read(13, false, start + Duration::from_secs(30)), "new copies must bypass backoff");
 assert!(!state.should_read(14, true, start + Duration::from_secs(30)));
 assert!(!state.should_read(14, false, start + Duration::from_secs(31)), "paused copies must stay skipped");
 assert!(!state.should_read(0, false, start + Duration::from_secs(31)));
 println!("PASS delayed clipboard recovery, dedup, newer sequence, pause, unavailable desktop");
}`;
const out = path.resolve('.repair/clipboard-translation/harness');
fs.mkdirSync(out, {recursive: true});
fs.writeFileSync(path.join(out, 'tracker.rs'), harness);
const compile = spawnSync('rustc', ['--edition=2024', '-Awarnings', path.join(out, 'tracker.rs'), '-o', path.join(out, 'tracker.exe')], {encoding:'utf8'});
if (compile.status !== 0) { console.log(compile.stderr); failures++; }
else {
 const result = spawnSync(path.join(out, 'tracker.exe'), [], {encoding:'utf8'});
 if (result.status === 0) process.stdout.write(result.stdout);
 else { failures++; console.log('FAIL delayed clipboard recovery: sequence abandoned after five failures'); }
}
console.log(`RESULT ${8-failures}/8 passed`);
process.exitCode = failures ? 1 : 0;
