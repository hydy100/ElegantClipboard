import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import ts from 'typescript';

const code = ts.transpileModule(fs.readFileSync('src/components/settings/PasteKeySetting.tsx','utf8'), {
  compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022,jsx:ts.JsxEmit.ReactJSX},
}).outputText;
const values=[], effects=[], requests=[];
let cursor=0;
const jsx=(type,props)=>({type,props});
const context={exports:{},require: name=>({
  react:{useState:init=>{const i=cursor++; if(!(i in values))values[i]=init;return [values[i],v=>values[i]=v];},useEffect:fn=>{if(!effects.length)effects.push(fn);}},
  'react/jsx-runtime':{jsx,jsxs:jsx},
  '@/components/ui/button':{Button:'button'},
  '@tauri-apps/api/core':{invoke:(command,args)=>new Promise((resolve,reject)=>requests.push({command,args,resolve,reject}))},
}[name])};
vm.runInNewContext(code,context);
const render=()=>{cursor=0;return context.exports.PasteKeySetting();};
const buttons=node=>{
  if(!node||typeof node!=='object')return [];
  if(Array.isArray(node))return node.flatMap(buttons);
  return node.type==='button'?[node]:buttons(node.props?.children);
};
const settle=async()=>{for(let i=0;i<6;i++)await Promise.resolve();};
assert(buttons(render()).every(b=>b.props.disabled));
const cleanup=effects[0]();
assert.equal(requests[0].args.key,'paste_key');
requests.shift().resolve('shift_insert'); await settle();
let controls=buttons(render());assert.equal(controls[1].props['aria-pressed'],true);
controls[0].props.onClick();assert(buttons(render()).every(b=>b.props.disabled));
assert.equal(requests[0].command,'set_setting');assert.equal(requests[0].args.value,'ctrl_v');
requests.shift().reject(new Error('fixture save failure'));await settle();
assert.equal(buttons(render())[1].props['aria-pressed'],true);
buttons(render())[0].props.onClick();requests.shift().resolve();await settle();
assert.equal(buttons(render())[0].props['aria-pressed'],true);
cleanup();
console.log('PASTE_SETTING_PASS: persisted selection loaded; pending save disabled; failed save preserved old value; successful save selected Ctrl+V');
