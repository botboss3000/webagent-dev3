const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs'), vm = require('node:vm'), path = require('node:path');
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.dataset = {}; this.listeners = {}; this.classList = {add(){}}; }
  append(...items) { this.children.push(...items); }
  replaceChildren(...items) { this.children = items; }
  setAttribute() {}
  addEventListener(name, callback) { this.listeners[name] = callback; }
}
const flush = async () => { for (let i=0;i<5;i++) await new Promise(setImmediate); };
function fixture(mode='build_stop', affected=['job'], preflight=null) {
  const panel = new Element('main'), calls = [], warnings = [];
  const state = {confirm:true, fail:false, requested:false, hold:null};
  const source = fs.readFileSync(path.join(__dirname,'../ui/main-panel/app/builds.js'),'utf8');
  const context = {URL, location:{href:'https://dev.webagent.live/app?tab=builds'},
    document:{hidden:false,createElement:tag=>new Element(tag),createTextNode:text=>({textContent:text})},
    localStorage:{getItem(){return null;},setItem(){}},crypto:{randomUUID:()=> 'stable-id'},
    window:{confirm:message=>{warnings.push(message);return state.confirm;}},
    setInterval(){return 1;},clearInterval(){},
    fetch:async (url, options={})=>{
      calls.push({url,options});
      if(url.endsWith('/auth/me'))return {ok:true,json:async()=>({is_admin:true,csrf_token:'fixture-csrf'})};
      if(url.endsWith('/cancel')) {
        if(state.hold) await state.hold;
        if(state.fail) throw Error('disconnected');
        state.requested=true;
        return {ok:true,json:async()=>({disposition:'stop_requested'})};
      }
      return {ok:true,json:async()=>({busy:true,jobs:[{job_id:'job',status:'running',phase:'building_api',
        queue_preflight:preflight, cancellation:{available:!state.requested,requested:state.requested,mode,affected_job_ids:affected}}]})};
    }};
  vm.createContext(context);vm.runInContext(source.replace('export function','function'),context);
  const dispose=context.mountBuilds(panel);
  const find=predicate=>{const walk=node=>predicate(node)?node:node.children?.map(walk).find(Boolean);return walk(panel);};
  return {state,calls,warnings,dispose,find,button:()=>find(node=>['Stop build','Cancel queued job','Request deployment stop','Stop requested'].includes(node.textContent))};
}
test('batch deployment confirmation precedes mutation; accepted stop carries CSRF and prevents duplicates',async()=>{
  const f=fixture('deployment_stop',['job','peer']);await flush();
  f.state.confirm=false;await f.button().listeners.click();
  assert.equal(f.calls.filter(c=>c.url.endsWith('/cancel')).length,0);
  assert.match(f.warnings[0],/all 2 jobs/);assert.match(f.warnings[0],/uncertain release/);
  f.state.confirm=true;
  let release;f.state.hold=new Promise(resolve=>{release=resolve;});
  const pending=f.button().listeners.click();await flush();
  assert(f.button().disabled);release();await pending;await flush();
  const writes=f.calls.filter(c=>c.url.endsWith('/cancel'));assert.equal(writes.length,1);
  assert.equal(writes[0].options.headers['x-csrf-token'],'fixture-csrf');
  assert.equal(JSON.parse(writes[0].options.body).request_id,'stable-id');
  assert.equal(f.button().textContent,'Stop requested');assert(f.button().disabled);f.dispose();
});
test('uncertain transport preserves request identity and refreshes before retry',async()=>{
  const f=fixture('queued');await flush();assert.equal(f.button().textContent,'Cancel queued job');
  f.state.fail=true;await f.button().listeners.click();await flush();
  assert(f.find(node=>String(node.textContent||'').includes('Update status before retrying')));
  f.state.fail=false;await f.button().listeners.click();await flush();
  const writes=f.calls.filter(c=>c.url.endsWith('/cancel'));assert.equal(writes.length,2);
  assert.equal(writes[0].options.body,writes[1].options.body);f.dispose();
});

test('queue preview renders bounded conflict evidence without writes',async()=>{
  const f=fixture('queued',['job'],{status:'conflicts',truncated:true,peers:[
    {job_id:'peer/unsafe',reason:'Conflicting edits',conflict_count:1,paths:['ui/<script>.js'],paths_truncated:true}
  ]});await flush();
  assert(f.find(node=>node.textContent==='Conflicting files: ui/<script>.js (more files omitted)'));
  assert.equal(f.find(node=>node.textContent==='peer/unsafe').href,'/app?tab=builds&job_id=peer%2Funsafe');
  assert(f.find(node=>String(node.textContent||'').includes('Partial queue preview')));
  assert.equal(f.calls.filter(c=>c.options.method==='POST').length,0);f.dispose();
});
test('unavailable queue comparison is explicit',async()=>{
  const f=fixture('queued',['job'],{status:'unavailable',peers:[]});await flush();
  assert(f.find(node=>node.textContent==='Source comparison unavailable; admission will check again.'));
  f.dispose();
});
