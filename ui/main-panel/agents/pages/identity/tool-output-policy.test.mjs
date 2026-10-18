import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source = await fs.readFile(new URL('./tool-output-policy.js', import.meta.url), 'utf8');
const {renderToolOutputPolicy} = await import('data:text/javascript;base64,' + Buffer.from(source.replace(/import \{requestTouchVibration\} from '[^']+';/, 'const requestTouchVibration=()=>{};')).toString('base64'));

// A small DOM fixture exercises the actual field/save implementation without replacing its logic.
class Element {
  constructor(tag) { this.tagName=tag; this.children=[]; this.listeners={}; this.style={}; this.dataset={}; this.attributes={}; }
  append(...items) { this.children.push(...items); }
  setAttribute(name, value) { this.attributes[name]=value; }
  addEventListener(name, callback) { this.listeners[name]=callback; }
  insertBefore(item, before) { this.children.splice(this.children.indexOf(before),0,item); }
}
function fixture(agent, persist) {
  globalThis.document = {createElement: tag => new Element(tag)};
  const host=new Element('main');
  renderToolOutputPolicy(host,agent,persist);
  const all=[];
  const visit=node=>{all.push(node);for(const child of node.children||[])visit(child);};visit(host);
  return {host,all,enabled:all.find(node=>node.id==='identity-tool-result-deduplication-enabled'),
    status:all.find(node=>node.attributes.role==='status')};
}
function agent(canManage=true) {
  return {can_manage:canManage,tool_result_deduplication:{enabled:true},
    tool_output_policy:{enabled:true,scope:'code_index',tools:[],lifetime_calls:6,
      ordinary_chars:24000,promoted_chars:24000,refresh_on_recall:true,promote_on_edit:true},
    tool_output_eligible_tools:['file_read']};
}

test('identity saves only the independent toggle and preserves disabled experimental settings',async()=>{
  const original=agent();const before=structuredClone(original);const patches=[];
  const f=fixture(original,async patch=>{patches.push(patch);return {...original,tool_result_deduplication:patch.tool_result_deduplication};});
  assert.equal(f.enabled.checked,true);assert.equal(f.enabled.disabled,false);
  const experimental=f.all.find(node=>node.tagName==='fieldset');assert.equal(experimental.disabled,true);
  assert.equal(f.all.find(node=>node.id==='identity-tool-output-enabled').checked,false);
  assert.ok(f.all.some(node=>node.textContent?.includes('Under development · Experimental — disabled')));
  f.enabled.checked=false;await f.enabled.listeners.change();
  assert.deepEqual(patches,[{tool_result_deduplication:{enabled:false}}]);
  assert.deepEqual(original,before);assert.equal(f.enabled.checked,false);
});

test('failed revision-fenced save restores checked state and reports reload guidance',async()=>{
  const f=fixture(agent(),async()=>{throw new Error('stale revision');});
  f.enabled.checked=false;await f.enabled.listeners.change();
  assert.equal(f.enabled.checked,true);assert.equal(f.enabled.disabled,false);
  assert.match(f.status.textContent,/Reload this page before saving/);
});

test('read-only and unavailable policies cannot be edited',()=>{
  assert.equal(fixture(agent(false),()=>{}).enabled.disabled,true);
  const absent=agent();delete absent.tool_result_deduplication;
  const f=fixture(absent,()=>{});assert.equal(f.enabled.disabled,true);
  assert.match(f.status.textContent,/unavailable/);
});

test('save remains disabled until the revisioned persistence operation settles',async()=>{
  let resolve;const pending=new Promise(r=>resolve=r);
  const f=fixture(agent(),()=>pending);f.enabled.checked=false;
  const save=f.enabled.listeners.change();assert.equal(f.enabled.disabled,true);
  resolve({...agent(),tool_result_deduplication:{enabled:false}});await save;
  assert.equal(f.enabled.disabled,false);assert.equal(f.enabled.checked,false);
});


// Exercise the actual tool-panel state/save logic; substitute only DOM/menu and save-feedback collaborators.
const controls = await fs.readFile(new URL('../../../../shared/ability-row-controls.js', import.meta.url), 'utf8');
const seam=controls.indexOf('let floatingMenuCleanup = null;');assert.ok(seam>0);
const toolSource=controls.slice(0,seam).replace(/import \{requestTouchVibration\} from '[^']+';/, 'const requestTouchVibration=()=>{};').replace("import { _markSaving, _flashSaveCheck } from './models/dom-utils.js';",
  'const _markSaving=()=>{}; const _flashSaveCheck=()=>{};')+`
let captured=null;
function openFloatingMenu(factory){captured=factory;}
function closeFloatingMenu(){captured=null;}
export function menuSnapshot(){return captured?.()||null;}
export function clearMenu(){captured=null;}
export {_buildToolStateControl};`;
const toolModule=await import('data:text/javascript;base64,'+Buffer.from(toolSource).toString('base64'));
function toolFixture(onChange,canEdit=true,metadata={result_deduplication:true}) {
  globalThis.document={getElementById:()=>null,createElement:tag=>{
    const node=new Element(tag);node.classList={add(){}};node.removeAttribute=()=>{};
    node.getBoundingClientRect=()=>({bottom:20,right:300});return node;
  }};
  toolModule.clearMenu();
  const calls=[];
  const tri=toolModule._buildToolStateControl({name:'file_read',permMode:'auto',mode:'visible',...metadata,
    execution:{mode:'foreground',max_concurrency:1,on_run_end:'stop'}},{canEdit,
    onResultDeduplicationChange:onChange,onPermissionChange:()=>calls.push('permission'),
    onVisibilityChange:()=>calls.push('visibility'),onExecutionChange:()=>calls.push('execution')});
  tri.listeners.click({stopPropagation(){},clientY:0});
  return {tri,calls,menu:()=>toolModule.menuSnapshot()};
}

test('tool checkbox saves inclusion without changing permission, visibility or execution',async()=>{
  const included=[];const f=toolFixture(async(name,value)=>included.push({name,value}));
  let item=f.menu().find(item=>item.label==='Suppress identical results');
  assert.equal(item.checkbox,true);assert.equal(item.checked,true);
  await item.action();item=f.menu().find(item=>item.label==='Suppress identical results');
  assert.equal(item.checked,false);assert.deepEqual(included,[{name:'file_read',value:false}]);assert.deepEqual(f.calls,[]);
});

test('tool checkbox restores saved inclusion after stale revision failure',async()=>{
  const f=toolFixture(async()=>{throw new Error('stale revision');});
  assert.equal(await f.menu().find(item=>item.label==='Suppress identical results').action(),false);
  assert.equal(f.menu().find(item=>item.label==='Suppress identical results').checked,true);assert.deepEqual(f.calls,[]);
});

test('read-only tool controls cannot open the editing panel',()=>{
  const f=toolFixture(async()=>{throw new Error('must not save');},false);assert.equal(f.menu(),null);
});

test('pending backend preserves actual saved retention values and prevents all writes',async()=>{
  const absent=agent();delete absent.tool_result_deduplication;let writes=0;
  const f=fixture(absent,async()=>{writes++;});
  assert.equal(f.all.find(node=>node.id==='identity-tool-output-enabled').checked,true);
  assert.ok(f.all.some(node=>node.textContent?.includes('running lifetime/budget policy is unchanged')));
  assert.equal(f.all.some(node=>node.textContent?.includes('Experimental — disabled')),false);
  f.enabled.checked=true;await f.enabled.listeners.change();
  assert.equal(writes,0);assert.equal(f.enabled.disabled,true);assert.equal(f.enabled.checked,false);
});

test('read-only identity handlers cannot bypass manager authority',async()=>{
  let writes=0;const f=fixture(agent(false),async()=>{writes++;});
  f.enabled.checked=false;await f.enabled.listeners.change();
  assert.equal(writes,0);assert.equal(f.enabled.checked,true);assert.equal(f.enabled.disabled,true);
});

test('older and malformed tool catalogs never offer unsupported inclusion writes',()=>{
  for(const metadata of [{},{result_deduplication:'true'},{result_deduplication:null}]) {
    const f=toolFixture(async()=>{throw new Error('must not save');},true,metadata);
    assert.equal(f.menu().some(item=>item.checkbox),false);
  }
});
