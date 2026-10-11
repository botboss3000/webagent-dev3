const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const tick=()=>new Promise(setImmediate);
const msg=seq=>({seq,id:'message-'+seq,role:'assistant',content:'reply'});
const snapshot=(messages=[],runs=[])=>({messages,runs,last_seq:Math.max(0,...messages.map(m=>m.seq))});
async function fixture() {
 const streams=[],saved=new Map([['a',snapshot([],[{id:'run',session_id:'a',status:'running'}])],['b',snapshot()]]),published=[],notes=[],timers=new Map(),calls=[],queues=[],metadata=[];
 let nextTimer=0,failRender=0,readOverride=null,metadataHook=()=>{};
 class EventSource {constructor(url){this.url=url;streams.push(this);}addEventListener(name,fn){(this.listeners??={})[name]=fn;}close(){this.closed=true;}emit(item){this.onmessage({data:JSON.stringify(item)});}}
 const context=vm.createContext({document:{getElementById:()=>null},EventSource,Set,Map,setTimeout:fn=>{timers.set(++nextTimer,fn);return nextTimer;},clearTimeout:id=>timers.delete(id)});
 const module=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'client.js'),'utf8'),{context});
 await module.link(()=>new vm.SyntheticModule(['createAdministratorNotices'],function(){this.setExport('createAdministratorNotices',()=>({start(){},stop(){}}));},{context}));await module.evaluate();
 const client=module.namespace.createChatClient({request:async url=>{const id=url.split('/').at(-1);calls.push(id);return readOverride?readOverride(id):structuredClone(saved.get(id));},note:text=>notes.push(text)});
 async function open(id){await client.open(id,()=>{},(item,options)=>{if(failRender){--failRender;throw Error('render failed');}published.push({item,options});},summary=>{metadata.push(summary);metadataHook(summary);},data=>queues.push(data));}
 return {client,streams,saved,published,notes,timers,calls,queues,metadata,metadataHook:fn=>metadataHook=fn,open,failRender:n=>failRender=n,override:fn=>readOverride=fn};
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
 const context=vm.createContext({$:()=>target,createMessage:item=>({dataset:{publicId:item.id,seq:String(item.seq)}}),resolveAsset:undefined,gutterRuns:new Map(),client:{},session:'a',historyLastSeq:0,historyAction:null,note(){},stats:{},readObserver:{observe(){}},paintActivity(){},updateAdmission(){}});
 vm.runInContext(code,context);for(const seq of [9,5,9,2])context.message(msg(seq));assert.deepEqual(children.map(x=>Number(x.dataset.seq)),[2,5,9]);
});

test('named queue events refresh delivery state without duplicating public messages',async()=>{
 const f=await fixture();await f.open('a');f.saved.set('a',{...snapshot([msg(7)]),queue:[{id:'submission',status:'pending'}],queue_state:{revision:1}});
 f.streams[0].listeners.queue_updated();await tick();assert.equal(f.queues.length,1);assert.equal(f.queues[0].queue[0].status,'pending');assert.equal(f.published.length,1);
 f.saved.set('a',{...snapshot([msg(7)]),queue:[{id:'submission',status:'cancelled'}],queue_state:{revision:2}});f.streams[0].listeners.queue_updated();await tick();assert.equal(f.queues[1].queue[0].status,'cancelled');assert.equal(f.published.length,1);
 await f.open('b');f.streams[0].listeners.queue_updated();await tick();assert.equal(f.queues.length,2);f.client.disconnect();
});

test('queue recovery detects history rewind before publication and waits for reopened history',async()=>{
 const f=await fixture();await f.open('a');f.saved.set('a',{...snapshot([msg(2)]),session:{id:'a',history_revision:1},queue:[],queue_state:{revision:8}});
 f.streams[0].listeners.queue_updated();await tick();assert.equal(f.metadata[0].history_revision,1);assert.equal(f.published.length,0);assert.equal(f.queues.length,0);
 f.streams[0].emit(msg(9));assert.equal(f.published.length,0);
 // A response captured before Undo cannot reintroduce removed messages while reopening is deferred.
 f.saved.set('a',snapshot([msg(10)]));f.streams[0].onopen();await tick();assert.equal(f.published.length,0);
 f.saved.set('a',{...snapshot([msg(2)]),session:{id:'a',history_revision:1}});await f.open('a');f.saved.set('a',{...snapshot([msg(2),msg(11)]),session:{id:'a',history_revision:1},queue:[]});f.streams.at(-1).listeners.queue_updated();await tick();assert.deepEqual(f.published.map(x=>x.item.seq),[11]);assert.equal(f.queues.length,1);f.client.disconnect();
});
test('queue rewind callback opening another session invalidates the old recovery',async()=>{
 const f=await fixture();await f.open('a');f.metadataHook(()=>{void f.open('b');});f.saved.set('a',{...snapshot([msg(5)]),session:{id:'a',history_revision:1}});f.streams[0].listeners.queue_updated();await tick();assert.equal(f.streams[0].closed,true);assert.equal(f.published.length,0);assert.equal(f.queues.length,0);f.client.disconnect();
});
test('panel snapshot merge refuses different history revisions and other sessions',()=>{
 const source=fs.readFileSync(path.join(__dirname,'panel.js'),'utf8'),start=source.indexOf('function syncSnapshot(data)'),code=source.slice(start,source.indexOf('/** Confirms rewind',start));
 const published=[],updates=[],changes=[];
 const context=vm.createContext({session:'a',historyRevision:2,historyLastSeq:4,historyChanged:s=>changes.push(s),message:item=>published.push(item),followups:{update:data=>updates.push(data)},runs(){}});vm.runInContext(code,context);
 assert.equal(context.syncSnapshot({session:{id:'a',history_revision:1},messages:[msg(8)]}),false);assert.equal(context.syncSnapshot({session:{id:'b',history_revision:2},messages:[msg(9)]}),false);assert.equal(published.length,0);assert.equal(updates.length,0);
 assert.equal(context.syncSnapshot({session:{id:'a',history_revision:2},last_seq:7,messages:[msg(7)],queue:[]}),true);assert.equal(context.historyLastSeq,7);assert.equal(published.length,1);assert.equal(updates.length,1);
});
