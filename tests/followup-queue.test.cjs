const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class E {
 constructor(tag='div'){this.tag=tag;this.children=[];this.dataset={};this.attributes={};this.hidden=false;this.disabled=false;this.textContent='';}
 append(...items){for(const item of items){item.parent=this;this.children.push(item);}}
 replaceChildren(...items){this.children=[];this.append(...items);}
 setAttribute(k,v){this.attributes[k]=v;}
 querySelectorAll(s){return this.children.flatMap(c=>[...((s==='[data-public-id]'&&c.dataset.publicId)||(s==='button'&&c.tag==='button')?[c]:[]),...c.querySelectorAll(s)]);}
 contains(node){return node===this||this.children.some(child=>child.contains(node));}
 focus(){if(this.document)this.document.activeElement=this;}
 get childNodes(){return this.children;}
 querySelector(s){return this.children.find(c=>s==='button'?c.tag==='button':(c.className||'').split(' ').includes(s.slice(1)))||this.children.map(c=>c.querySelector(s)).find(Boolean)||null;}
 remove(){if(this.parent)this.parent.children=this.parent.children.filter(c=>c!==this);}
}
const flatten=n=>[n.textContent,...n.children.map(flatten)].join(' ');
const tick=()=>new Promise(setImmediate);
async function fixture(canWrite=()=>true){
 const root=new E(),row=new E(),host=new E();row.dataset.publicId='user:a';row.dataset.seq='7';root.append(row,host);
 const document={activeElement:null,createElement:tag=>{const node=new E(tag);node.document=document;return node;}};
 const context=vm.createContext({document,Set,Number});
 const mod=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'../ui/chat/core/followup-queue.js'),'utf8'),{context});await mod.link(()=>new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'../ui/chat/core/chat-action-panel.js'),'utf8'),{context}));await mod.evaluate();
 let session='one',data={queue:[{id:'a',message_id:'user:a',revision:2,choice:'steer',status:'pending'}],queue_state:{revision:3,held:false}};
 const calls=[],errors=[],keys=new Map();let write=null;
 const client={snapshot:async()=>data,delivery:async(...args)=>{calls.push(args);if(write)return write();data={queue:[{...data.queue[0],choice:args[2].choice,revision:3}],queue_state:{revision:4}};},resumeQueue:async(...args)=>calls.push(args)};
 const queue=mod.namespace.mountFollowupQueue({root,client,getSession:()=>session,canWrite,guard:fn=>async()=>{try{await fn();}catch(e){errors.push(e.message);}},note:()=>{},keyId:k=>{if(!keys.has(k))keys.set(k,'request-'+keys.size);return keys.get(k);},forgetKey:k=>keys.delete(k),onSnapshot:d=>queue.update(d)});
 queue.update(data);
 return {queue,row,host,calls,errors,document,buttons:()=>row.querySelector('.followup-delivery-actions').children,setData:d=>data=d,setWrite:fn=>write=fn,session:s=>session=s};
}
test('send now forces the pending steer with revision and request identity',async()=>{
 const f=await fixture();assert.match(flatten(f.row),/next step/);assert.deepEqual(f.buttons().map(b=>b.textContent),['Send now','Cancel']);await f.buttons()[0].onclick();
 assert.equal(f.calls[0][0],'one');assert.equal(f.calls[0][1],'a');assert.equal(f.calls[0][2].expected_revision,2);assert.equal(f.calls[0][2].choice,'now');assert.ok(f.calls[0][2].client_request_id);
 f.queue.update({queue:[{id:'a',message_id:'user:a',revision:3,status:'pending',choice:'steer',send_now:true}],queue_state:{revision:5}});
 assert.match(flatten(f.row),/Sending now/);assert.deepEqual(f.buttons().map(b=>b.textContent),['Cancel']);assert.equal(f.queue.pending,true);
 f.queue.update({queue:[{id:'a',message_id:'user:a',status:'delivered',choice:'steer'}],queue_state:{revision:6}});assert.equal(f.row.querySelector('.followup-delivery-actions'),null);assert.match(flatten(f.row),/Delivered/);
});
test('held queue offers resume and cancel but not send now',async()=>{
 const f=await fixture();f.queue.update({queue:[{id:'a',message_id:'user:a',revision:2,status:'pending',choice:'steer'}],queue_state:{revision:4,held:true,reason:'Stopped'}});
 assert.match(flatten(f.row),/Held: Stopped/);assert.deepEqual(f.buttons().map(b=>b.textContent),['Resume','Cancel']);assert.equal(f.host.children.length,0);
 await f.buttons()[0].onclick();assert.equal(f.calls[0][0],'one');assert.ok(f.calls[0][1].client_request_id);assert.equal(f.calls[0][1].expected_revision,4);
});
test('stale snapshots and late choice results cannot overwrite a new session',async()=>{
 const f=await fixture();let resolve;f.setWrite(()=>new Promise(r=>resolve=r));const pending=f.buttons()[0].onclick();await tick();assert.ok(f.buttons().every(b=>b.disabled));
 f.queue.reset();f.session('two');f.queue.update({queue:[{id:'a',message_id:'user:a',choice:'steer',status:'held'}],queue_state:{revision:10,held:true,reason:'New session'}});resolve();await pending;
 assert.match(flatten(f.row),/Held: New session/);
 f.queue.update({queue:[],queue_state:{revision:9}});assert.match(flatten(f.row),/Held/);
});
test('cancelled entries remain visible with no actions and uncertain writes reuse request ID',async()=>{
 const f=await fixture();f.setWrite(()=>{throw Error('connection lost');});await f.buttons()[1].onclick();await f.buttons()[1].onclick();assert.equal(f.calls[0][2].client_request_id,f.calls[1][2].client_request_id);assert.equal(f.calls[0][2].choice,'cancel');
 f.queue.update({queue:[{id:'a',message_id:'user:a',status:'cancelled',choice:'cancel'}],queue_state:{revision:5}});assert.match(flatten(f.row),/Cancelled/);assert.equal(f.row.querySelector('.followup-delivery-actions'),null);assert.equal(f.queue.pending,false);
});
// The shared access projection permits queue mutation only to its canonical owner.
test('read-only queue projection keeps status visible and blocks every mutation',async()=>{
 let allowed=false;const f=await fixture(()=>allowed);
 assert.match(flatten(f.row),/next step/);assert.ok(f.buttons().every(b=>b.disabled));
 await f.buttons()[0].onclick();assert.equal(f.calls.length,0);assert.match(f.errors[0],/not available/);
 const held={queue:[{id:'a',message_id:'user:a',revision:2,choice:'steer',status:'held'}],queue_state:{revision:4,held:true,reason:'Stopped'}};
 f.queue.update(held);const resume=f.buttons()[0];assert.equal(resume.textContent,'Resume');assert.ok(resume.disabled);
 await resume.onclick();assert.equal(f.calls.length,0);
 allowed=true;f.queue.update(held);assert.ok(f.buttons().every(b=>!b.disabled));
});
