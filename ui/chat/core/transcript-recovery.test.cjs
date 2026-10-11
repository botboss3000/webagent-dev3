const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const tick=()=>new Promise(setImmediate);
const msg=seq=>({seq,id:'message-'+seq,role:'assistant',content:'reply'});
const snapshot=(messages=[],runs=[])=>({messages,runs,last_seq:Math.max(0,...messages.map(m=>m.seq))});
async function fixture() {
 const streams=[],saved=new Map([['a',snapshot([],[{id:'run',session_id:'a',status:'running'}])],['b',snapshot()]]),published=[],notes=[],timers=new Map(),calls=[];
 let nextTimer=0,failRender=0,readOverride=null;
 class EventSource {constructor(url){this.url=url;streams.push(this);}addEventListener(){}close(){this.closed=true;}emit(item){this.onmessage({data:JSON.stringify(item)});}}
 const context=vm.createContext({document:{getElementById:()=>null},EventSource,Set,Map,setTimeout:fn=>{timers.set(++nextTimer,fn);return nextTimer;},clearTimeout:id=>timers.delete(id)});
 const module=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'client.js'),'utf8'),{context});
 await module.link(()=>new vm.SyntheticModule(['createAdministratorNotices'],function(){this.setExport('createAdministratorNotices',()=>({start(){},stop(){}}));},{context}));await module.evaluate();
 const client=module.namespace.createChatClient({request:async url=>{const id=url.split('/').at(-1);calls.push(id);return readOverride?readOverride(id):structuredClone(saved.get(id));},note:text=>notes.push(text)});
 async function open(id){await client.open(id,()=>{},(item,options)=>{if(failRender){--failRender;throw Error('render failed');}published.push({item,options});});}
 return {client,streams,saved,published,notes,timers,calls,open,failRender:n=>failRender=n,override:fn=>readOverride=fn};
}
test('terminal poll restores dropped final event exactly once',async()=>{
 const f=await fixture();await f.open('a');f.saved.set('a',snapshot([msg(3)]));f.client.updateRuns([{id:'run',session_id:'a',status:'completed'}]);await tick();
 assert.deepEqual(f.published.map(x=>x.item.seq),[3]);assert.equal(f.published[0].options.recovered,true);
 f.client.updateRuns([{id:'run',session_id:'a',status:'completed'}]);f.streams[0].emit(msg(3));await tick();assert.equal(f.published.length,1);assert.equal(f.calls.length,2);f.client.disconnect();
});
test('reconnect fills older gap below newer delivered event',async()=>{
 const f=await fixture();await f.open('a');f.streams[0].emit(msg(9));f.saved.set('a',snapshot([msg(5),msg(9)]));f.streams[0].onopen();await tick();assert.deepEqual(f.published.map(x=>x.item.seq),[9,5]);f.client.disconnect();
});
test('render failure recovers despite browser Last-Event-ID advancement',async()=>{
 const f=await fixture();await f.open('a');f.saved.set('a',snapshot([msg(2)]));f.failRender(1);f.streams[0].emit(msg(2));await tick();assert.deepEqual(f.published.map(x=>x.item.seq),[2]);assert.equal(f.published[0].options.recovered,true);f.client.disconnect();
});
test('overlapping triggers coalesce and completion mid-fetch gets fresh snapshot',async()=>{
 const f=await fixture();await f.open('a');let resolve,count=0;f.override(()=>++count===1?new Promise(r=>resolve=r):snapshot([msg(4)]));
 f.streams[0].onopen();f.client.updateRuns([{id:'run',session_id:'a',status:'completed'}]);assert.equal(count,1);resolve(snapshot());await tick();assert.equal(count,2);assert.equal(f.published.length,1);f.client.disconnect();
});
test('delayed old session response cannot publish into new session',async()=>{
 const f=await fixture();await f.open('a');let resolve;f.override(id=>id==='a'?new Promise(r=>resolve=r):snapshot());f.streams[0].onopen();await f.open('b');resolve(snapshot([msg(6)]));await tick();assert.equal(f.published.length,0);assert.equal(f.streams[0].closed,true);f.client.disconnect();
});
test('retries stop after three timers and disconnect cancels scheduled retry',async()=>{
 const f=await fixture();await f.open('a');f.override(()=>{throw Error('offline');});f.streams[0].onerror();await tick();
 for(let i=0;i<3;i++){assert.equal(f.timers.size,1);const [id,fn]=f.timers.entries().next().value;f.timers.delete(id);fn();await tick();}
 assert.equal(f.timers.size,0);assert.equal(f.notes.length,1);f.client.disconnect();
 const g=await fixture();await g.open('a');g.override(()=>{throw Error('offline');});g.streams[0].onerror();await tick();assert.equal(g.timers.size,1);g.client.disconnect();assert.equal(g.timers.size,0);
});
test('other session completion does not fetch current snapshot',async()=>{
 const f=await fixture();await f.open('a');f.client.updateRuns([{id:'other',session_id:'b',status:'completed'}]);await tick();assert.equal(f.calls.length,1);f.client.disconnect();
});
test('transcript inserts recovered gaps in order without replacing existing rows',()=>{
 const source=fs.readFileSync(path.join(__dirname,'panel.js'),'utf8'),code=source.slice(source.indexOf('function message(item)'),source.indexOf('// Base presentation'));
 const children=[],target={children,insertBefore(row,before){const index=children.indexOf(before);children.splice(index<0?children.length:index,0,row);}};
 const context=vm.createContext({$:()=>target,createMessage:item=>({dataset:{publicId:item.id,seq:String(item.seq)}}),resolveAsset:undefined,gutterRuns:new Map(),client:{},session:'a',note(){},stats:{},readObserver:{observe(){}},paintActivity(){},updateAdmission(){}});
 vm.runInContext(code,context);for(const seq of [9,5,9,2])context.message(msg(seq));assert.deepEqual(children.map(x=>Number(x.dataset.seq)),[2,5,9]);
});
