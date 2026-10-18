// Run real hooks with deterministic React/Tauri boundaries; no desktop state is touched.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
const root = path.resolve(process.argv[2] || '.');
const tests = [];
function compile(source) { return ts.transpileModule(source, {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText; }
function source(file) { const target=path.join(root,file); return fs.readFileSync(fs.existsSync(target)?target:path.resolve(file),'utf8'); }
function hookRunner(file, exportName, mocks) {
  const slots=[], timers=new Map(), listeners=new Map(); let cursor=0, pending=[], dirty=false, result, timerId=0;
  const depsEqual=(a,b)=>a&&b&&a.length===b.length&&a.every((v,i)=>Object.is(v,b[i]));
  const react={
    useState(initial) { const i=cursor++; if(!(i in slots)) slots[i]={value:typeof initial==='function'?initial():initial}; return [slots[i].value, value=>{slots[i].value=typeof value==='function'?value(slots[i].value):value;dirty=true;}]; },
    useRef(value) { const i=cursor++; return slots[i]??=( {current:value} ); },
    useMemo(fn,deps) { const i=cursor++; if(!slots[i]||!depsEqual(slots[i].deps,deps)) slots[i]={value:fn(),deps};return slots[i].value; },
    useCallback(fn,deps) { return react.useMemo(()=>fn,deps); },
    useEffect(fn,deps) { const i=cursor++; if(!slots[i]||!depsEqual(slots[i].deps,deps)) { const old=slots[i];slots[i]={deps};pending.push(()=>{old?.cleanup?.();slots[i].cleanup=fn();}); } },
  };
  const context={exports:{},console,Set,Map,Promise,
    setTimeout:fn=>{timers.set(++timerId,fn);return timerId;},clearTimeout:id=>timers.delete(id),
    window:{addEventListener:(name,fn)=>listeners.set(name,fn),removeEventListener:name=>listeners.delete(name)},
  };
  const modules={react,...mocks}; context.require=name=>{if(!(name in modules)) throw new Error('Unmocked '+name); return modules[name];};
  const keyed={...context,exports:{}}; vm.runInNewContext(compile(source('src/hooks/useKeyedDebounce.ts')),keyed);modules['@/hooks/useKeyedDebounce']=keyed.exports;
  vm.runInNewContext(compile(source(file)),context);
  function render(...args) {cursor=0;dirty=false;result=context.exports[exportName](...args);const effects=pending;pending=[];effects.forEach(f=>f());return result;}
  return {render, get result(){return result;}, async settle(){for(let i=0;i<20;i++){await Promise.resolve();if(dirty)render();}}, async timers(){const run=[...timers.values()];timers.clear();run.forEach(f=>f());await this.settle();}, unmount(){slots.forEach(s=>s?.cleanup?.());}, listeners,context};
}
async function webdav(options={}) {
  const writes=[]; let data=options.data||{}, action=options.action;
  const h=hookRunner('src/hooks/useWebDAVSettings.ts','useWebDAVSettings',{
    '@tauri-apps/api/core':{invoke:async(cmd,args)=>{if(cmd==='get_settings_batch')return data;writes.push(args);if(action)return action(args);}},
    '@/lib/logger':{logError(){}},
  }); h.render();await h.settle();await h.timers();
  return {h,writes,setData:v=>data=v,setAction:v=>action=v};
}
tests.push(['WebDAV load does not write defaults back',async()=>{const {writes}=await webdav();assert.equal(writes.length,0);}]);
tests.push(['changing one sync type writes only that field',async()=>{const {h,writes}=await webdav();writes.length=0;h.result.setSyncTypes(new Set(['text','image','files','video']));h.render();await h.settle();assert.deepEqual(writes.map(w=>w.key),['webdav_sync_video']);}]);
tests.push(['same-field writes are serialized and undo is preserved',async()=>{const {h,writes,setAction}=await webdav();writes.length=0;let finish;setAction(()=>new Promise(resolve=>finish=resolve));h.result.setEnabled(true);h.render();h.result.setEnabled(false);h.render();assert.equal(writes.length,1);setAction(null);finish();await h.settle();assert.deepEqual(writes.map(w=>w.value),['true','false']);}]);
tests.push(['failed field remains dirty for later retry',async()=>{const {h,writes,setAction}=await webdav();writes.length=0;setAction(()=>Promise.reject(new Error('injected')));h.result.setUrl('retry-url');h.render();await h.timers();setAction(null);h.result.setUsername('new-user');h.render();await h.timers();assert.equal(writes.filter(w=>w.key==='webdav_url').length,2);}]);
tests.push(['reload cancels stale pending saves',async()=>{const {h,writes,setData}=await webdav();writes.length=0;h.result.setUrl('stale');h.render();setData({webdav_url:'from-server'});await h.result.loadSettings();await h.settle();await h.timers();assert.equal(writes.length,0);assert.equal(h.result.url,'from-server');}]);
tests.push(['tab switch flushes independent pending fields',async()=>{const {h,writes}=await webdav();writes.length=0;h.result.setUrl('u');h.result.setUsername('n');h.render();h.unmount();await h.settle();assert.deepEqual(writes.map(w=>w.key).sort(),['webdav_url','webdav_username']);}]);
tests.push(['filter and reset cancel pending searches',async()=>{
  let listener,query='';let calls=0; const store={subscribe:fn=>{listener=fn;return()=>{};}};
  const h=hookRunner('src/hooks/useSearchDebounce.ts','useSearchDebounce',{
    'lodash.debounce':{default:(fn)=>{let timer;const cb=()=>{if(timer)h.context.clearTimeout(timer);timer=h.context.setTimeout(fn);};cb.cancel=()=>h.context.clearTimeout(timer);return cb;}},
    '@/stores/clipboard':{useClipboardStore:store},
  }); const options={fetchItems:async()=>calls++,setSearchQuery:v=>query=v};
  for(const field of ['selectedCategory','selectedTagId','_resetToken']) {
    h.render(options);h.result.handleSearchChange({target:{value:'find'}});
    listener?.({[field]:1},{[field]:0});
    await h.timers();assert.equal(calls,0,field);assert.equal(query,'find');
  }
}]);
function generalHarness() {
  const text=source('src/pages/Settings.tsx'),tree=ts.createSourceFile('Settings.tsx',text,ts.ScriptTarget.Latest,true,ts.ScriptKind.TSX);let declaration,keys;
  function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText(tree)==='saveSettings')declaration=node.parent.parent.getText(tree);if(ts.isVariableDeclaration(node)&&node.name.getText(tree)==='AUTO_SAVE_KEYS')keys=node.parent.parent.getText(tree);ts.forEachChild(node,visit);}visit(tree);
  const base={max_history_count:1000,max_content_size_kb:1024,max_image_size_kb:0,max_file_size_kb:0,max_video_size_kb:0,auto_cleanup_days:30,auto_start:false,admin_launch:false};
  const writes=[];const context={exports:{},settings:base,savedSettingsRef:{current:{...base}},currentSettingsRef:{current:{...base}},savingSettingsRef:{current:false},savePendingRef:{current:false},Promise,logError(){},invoke:async(...args)=>writes.push(args)};
  vm.runInNewContext(compile((keys||'')+'\n'+declaration+'\nglobalThis.save=saveSettings;'),context);return {context,writes};
}
tests.push(['general settings save only changed fields and not admin preference',async()=>{const {context:c,writes}=generalHarness();c.currentSettingsRef.current.max_history_count=2000;c.currentSettingsRef.current.admin_launch=true;await c.save();assert.deepEqual(writes.map(w=>w[1]?.key||w[0]),['max_history_count']);await c.save();assert.equal(writes.length,1);}]);
tests.push(['general settings preserve edits and undo during save',async()=>{const {context:c,writes}=generalHarness();let finish;c.invoke=async(...args)=>{writes.push(args);if(writes.length===1)await new Promise(r=>finish=r);};c.currentSettingsRef.current={...c.currentSettingsRef.current,auto_start:true};const first=c.save();c.currentSettingsRef.current={...c.currentSettingsRef.current,auto_start:false};await c.save();finish();await first;assert.deepEqual(writes.map(w=>w[0]),['enable_autostart','disable_autostart']);}]);
tests.push(['general save failures do not roll back successful fields',async()=>{
  const {context:c,writes}=generalHarness();let fail=true;
  c.invoke=async(cmd,args)=>{writes.push([cmd,args]);if(fail&&args?.key==='max_file_size_kb')throw new Error('injected');};
  c.currentSettingsRef.current={...c.currentSettingsRef.current,max_history_count:2000,max_file_size_kb:300};
  await c.save();fail=false;await c.save();
  assert.equal(writes.filter(w=>w[1]?.key==='max_history_count').length,1);
  assert.equal(writes.filter(w=>w[1]?.key==='max_file_size_kb').length,2);
}]);
let failed=0;for(const [name,test]of tests){try{await test();console.log('PASS '+name);}catch(error){failed++;console.log('FAIL '+name+': '+error.message);}}
console.log(`RESULT ${tests.length-failed}/${tests.length} passed`);process.exitCode=failed?1:0;
