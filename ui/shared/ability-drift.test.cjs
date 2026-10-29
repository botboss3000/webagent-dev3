const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
async function setup(responses){
 const context=vm.createContext({Error,globalThis:{}});
 const module=new vm.SourceTextModule(fs.readFileSync(__dirname+'/ability-drift.js','utf8'),{context});await module.link(()=>{});await module.evaluate();
 const calls=[],shown=[];let tick=null;
 const drift=module.namespace.createAbilityDrift({request:async(path,method='GET',body)=>{calls.push(JSON.parse(JSON.stringify({path,method,body})));const next=responses.shift();if(next instanceof Error)throw next;return typeof next==='function'?next():next;},render:value=>shown.push(value&&value.digest),setIntervalFn:fn=>{tick=fn;return 1;},clearIntervalFn:()=>{tick=null;}});
 return {drift,calls,shown,tick:async()=>{tick();await new Promise(setImmediate);},settle:()=>new Promise(setImmediate)};
}
const change={digest:'d1',show:true,notices:[{change:'changed_code',ability:'Core/files',tool:null,detail:'Code changed'}]};
test('shows the change set, then hides it when the server says so',async()=>{const f=await setup([change,{...change,show:false}]);f.drift.start();await f.settle();await f.tick();assert.deepEqual(f.shown,['d1',null]);});
test('dismiss hides at once and records the checkbox choice for this digest',async()=>{const f=await setup([change,{}]);f.drift.start();await f.settle();await f.drift.dismiss(true);assert.deepEqual(f.shown,['d1',null]);assert.deepEqual(f.calls[1],{path:'/admin/ability-drift/dismiss',method:'POST',body:{digest:'d1',remind_next_login:true}});});
test('a refused dismissal shows the current change set again',async()=>{const conflict=Object.assign(Error('changed'),{status:409});const f=await setup([change,conflict,{...change,digest:'d2'}]);f.drift.start();await f.settle();await f.drift.dismiss(false);assert.deepEqual(f.shown,['d1',null,'d2']);});
test('a poll that finishes after dismissal cannot bring the popup back',async()=>{let finish;const f=await setup([change,()=>new Promise(resolve=>finish=resolve),{}]);f.drift.start();await f.settle();const late=f.tick();await f.drift.dismiss(false);finish(change);await late;await f.settle();assert.deepEqual(f.shown,['d1',null]);});
test('stop clears the popup and the timer',async()=>{const f=await setup([change]);f.drift.start();await f.settle();f.drift.stop();assert.deepEqual(f.shown,['d1',null]);});
