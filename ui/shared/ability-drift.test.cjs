const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
async function setup(responses){
 const context=vm.createContext({Error,globalThis:{}});
 const module=new vm.SourceTextModule(fs.readFileSync(__dirname+'/ability-drift.js','utf8'),{context});await module.link(()=>{});await module.evaluate();
 const calls=[],shown=[];let tick=null;
 const drift=module.namespace.createAbilityDrift({request:async(path,method='GET',body)=>{calls.push(JSON.parse(JSON.stringify({path,method,body})));const next=responses.shift();if(next instanceof Error)throw next;return typeof next==='function'?next():next;},render:value=>shown.push(value&&value.digest),uuid:()=>'press-0001',setIntervalFn:fn=>{tick=fn;return 1;},clearIntervalFn:()=>{tick=null;}});
 return {drift,calls,shown,tick:async()=>{tick();await new Promise(setImmediate);},settle:()=>new Promise(setImmediate)};
}
const change={digest:'d1',show:true,notices:[{change:'changed_code',ability:'Core/files',tool:null,detail:'Code changed'}]};
test('shows the change set, then hides it when the server says so',async()=>{const f=await setup([change,{...change,show:false}]);f.drift.start();await f.settle();await f.tick();assert.deepEqual(f.shown,['d1',null]);});
test('dismiss hides at once and records the checkbox choice for this digest',async()=>{const f=await setup([change,{}]);f.drift.start();await f.settle();await f.drift.dismiss(true);assert.deepEqual(f.shown,['d1',null]);assert.deepEqual(f.calls[1],{path:'/admin/ability-drift/dismiss',method:'POST',body:{digest:'d1',remind_next_login:true}});});
test('a refused dismissal shows the current change set again',async()=>{const conflict=Object.assign(Error('changed'),{status:409});const f=await setup([change,conflict,{...change,digest:'d2'}]);f.drift.start();await f.settle();await f.drift.dismiss(false);assert.deepEqual(f.shown,['d1',null,'d2']);});
test('a poll that finishes after dismissal cannot bring the popup back',async()=>{let finish;const f=await setup([change,()=>new Promise(resolve=>finish=resolve),{}]);f.drift.start();await f.settle();const late=f.tick();await f.drift.dismiss(false);finish(change);await late;await f.settle();assert.deepEqual(f.shown,['d1',null]);});
test('stop clears the popup and the timer',async()=>{const f=await setup([change]);f.drift.start();await f.settle();f.drift.stop();assert.deepEqual(f.shown,['d1',null]);});
test('rebuild sends the chosen mode with one request id per press',async()=>{const f=await setup([change,{job_id:'j1',status:'closed'}]);f.drift.start();await f.settle();const reply=await f.drift.rebuild('stable');assert.equal(reply.job_id,'j1');assert.deepEqual(f.calls[1],{path:'/admin/ability-drift/rebuild',method:'POST',body:{mode:'stable',request_id:'press-0001'}});});
test('re-polling the same change set keeps the confirmation, a new one replaces it',async()=>{
 const context=vm.createContext({Error,globalThis:{}});
 const module=new vm.SourceTextModule(fs.readFileSync(__dirname+'/ability-drift.js','utf8'),{context});await module.link(()=>{});await module.evaluate();
 const node=()=>{const n={children:[],hidden:false,textContent:'',attributes:{},className:'',type:'',title:'',checked:false,
  append(...items){this.children.push(...items);},replaceChildren(){this.children=[];},setAttribute(k,v){this.attributes[k]=v;},addEventListener(){},
  querySelector(){return null;},remove(){},replaceWith(){}};return n;};
 const view=module.namespace.abilityDriftView({createElement:node},()=>{});
 const value={digest:'d1',notices:[{change:'changed_code',ability:'Core/x',tool:null,detail:'Code changed'}]};
 view.render(value);const marker={};view.element.children.push(marker);
 view.render(value);assert.ok(view.element.children.includes(marker));
 view.render({...value,digest:'d2'});assert.ok(!view.element.children.includes(marker));
 view.render(null);assert.equal(view.element.hidden,true);view.render(value);assert.equal(view.element.hidden,false);
});
