const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../ui/chat/core/administrator-notices.js'),'utf8');
function load(){const ctx={};vm.createContext(ctx);vm.runInContext(fs.readFileSync(path.join(__dirname,'../ui/chat/core/notification-panel.js'),'utf8').replaceAll('export function','function')+'\n'+source.replace(/^import .*;\n/m,'').replaceAll('export function','function'),ctx);return ctx;}
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};};
const flush=async()=>{for(let i=0;i<4;i++)await new Promise(setImmediate);};
const notice=(sha='a')=>({notice:{snapshot_sha256:sha.repeat(64),dirty:true,ahead:0,observed_at:123,notification:'Repository changed. Review before editing.'}});
function timers(){const pending=new Map();let serial=0;return {
 setIntervalFn(fn,ms){assert.equal(ms,10000);pending.set(++serial,fn);return serial;},
 clearIntervalFn(id){pending.delete(id);},
 tick(){for(const fn of pending.values())fn();},size(){return pending.size;}};}
test('current-session poll is single-flight, deduplicated, revocation clears only its region',async()=>{
 const events=[],requests=[],timer=timers();let response=notice(),hold=null;
 const helper=load().createAdministratorNotices({...timer,render:text=>events.push(text),
 request:async url=>{requests.push(url);return hold?hold.promise:response;}});
 helper.start('own session');await flush();assert.equal(requests[0],'/sessions/own%20session/administrator/notices');
 assert.equal(events.at(-1),response.notice.notification);const count=events.length;
 timer.tick();await flush();assert.equal(events.length,count);
 hold=deferred();timer.tick();timer.tick();timer.tick();assert.equal(requests.length,3);
 hold.resolve({notice:null});await flush();assert.equal(events.at(-1),'');
 response=notice('b');hold=null;timer.tick();await flush();assert.equal(events.at(-1),response.notice.notification);
 helper.stop();assert.equal(timer.size(),0);assert.equal(events.at(-1),'');
});
test('delayed switched/disconnected replies cannot publish and replacement polling waits',async()=>{
 const timer=timers(),events=[],calls=[],old=deferred(),fresh=deferred();
 const helper=load().createAdministratorNotices({...timer,render:text=>events.push(text),
 request:url=>{calls.push(url);return calls.length===1?old.promise:fresh.promise;}});
 helper.start('old');helper.start('new');timer.tick();assert.equal(calls.length,1);
 old.resolve(notice('a'));await flush();assert.equal(calls.length,2);assert(calls[1].includes('/new/'));
 assert.equal(events.at(-1),'');helper.stop();fresh.resolve(notice('b'));await flush();
 assert.equal(events.at(-1),'');assert.equal(timer.size(),0);
});
test('invalid/failed live notice withdraws prior FYI without unsafe text',async()=>{
 const timer=timers(),events=[];let response=notice();
 const helper=load().createAdministratorNotices({...timer,render:text=>events.push(text),
 request:async()=>{if(response instanceof Error)throw response;return response;}});
 helper.start('s');await flush();response={notice:{...notice().notice,notification:'x'.repeat(151)}};
 timer.tick();await flush();assert.equal(events.at(-1),'');
 response=notice('b');timer.tick();await flush();response=new Error('private transport detail');
 timer.tick();await flush();assert.equal(events.at(-1),'');assert(!events.some(x=>x.includes('private')));
 helper.stop();
});
class Element {
 constructor(tag='div'){this.tag=tag;this.children=[];this.textContent='';}
 setAttribute(){}append(...items){for(const item of items){item.parentNode=this;this.children.push(item);}}
 prepend(item){item.parentNode=this;this.children.unshift(item);}replaceChildren(){this.children=[];}
 querySelector(s){return this.children.find(c=>s==='button'?c.tag==='button':(c.className||'').split(' ').includes(s.slice(1)))||this.children.map(c=>c.querySelector(s)).find(Boolean)||null;}
}
const flatten=node=>[node.textContent,...node.children.map(flatten)].join(' ');
test('shared repository card never overwrites chat connection errors or uses HTML',()=>{
 const error={textContent:'Connection interrupted'},parent=new Element();
 const document={getElementById:id=>id==='notification-panel'?parent:error,createElement:tag=>new Element(tag)};
 const render=load().repositoryNoticeRenderer(document);render('<script>literal</script>');
 assert.match(flatten(parent),/<script>literal<\/script>/);assert.match(parent.children[0].className,/chat-notification/);
 assert.equal(error.textContent,'Connection interrupted');render('');assert(parent.children[0].hidden);assert.equal(error.textContent,'Connection interrupted');assert.equal(parent.children.length,1);
});
test('chat client clears timer and stale EventSource epoch while preserving note callbacks',async()=>{
 const ctx=load(),notes=[],timer=timers(),sources=[];class Events{constructor(){sources.push(this);}addEventListener(){}close(){this.closed=true;}}
 const classes=new Set(),button={title:'Toggle chat panel',classList:{toggle:(name,on)=>on?classes.add(name):classes.delete(name)}};
 ctx.document={getElementById:id=>id==='chat-toggle-btn'?button:null};
 ctx.EventSource=Events;
 ctx.createAdministratorNotices=args=>load().createAdministratorNotices({...args,...timer,render:()=>{}});
 const clientSource=fs.readFileSync(path.join(__dirname,'../ui/chat/core/client.js'),'utf8')
 .replace(/^import .*;\n/m,'').replace('export function','function');
 vm.runInContext(clientSource,ctx);
 const client=ctx.createChatClient({request:async url=>url.endsWith('/administrator/notices')?{notice:null}:{last_seq:0,messages:[],runs:[]},
 note:text=>notes.push(text),EventSourceClass:Events});
 await client.open('first',()=>{},()=>{});assert(classes.has('connection-connecting'));sources[0].onopen();assert(!classes.has('connection-connecting'));
 sources[0].onerror();assert(classes.has('connection-connecting'));assert.equal(notes.length,0);
 await client.open('second',()=>{},()=>{});sources[0].onopen();assert(classes.has('connection-connecting'));
 sources[1].onopen();assert(!classes.has('connection-connecting'));assert.equal(button.title,'Toggle chat panel');
 assert.equal(timer.size(),1);client.disconnect();assert(!classes.has('connection-connecting'));assert.equal(timer.size(),0);
});

const buildNotice=()=>({id:'b'.repeat(64),task_id:'retained-task',status:'stale_open',notification:'Investigate retained work.',retained_bytes:1024});
test('build reminders survive absent repository notice, deduplicate and review exact owning session',async()=>{
 const timer=timers(),renders=[],calls=[];let response={notice:null,build_notices:[buildNotice()]};
 const helper=load().createAdministratorNotices({...timer,render:()=>{},renderBuilds:(rows,review)=>renders.push({rows,review}),
 request:async(url,method)=>{calls.push({url,method});if(method==='POST')response={notice:null,build_notices:[]};return response;}});
 helper.start('owner');await flush();assert.equal(renders.at(-1).rows.length,1);
 const count=renders.length;timer.tick();await flush();assert.equal(renders.length,count);
 await renders.at(-1).review('b'.repeat(64));await flush();
 assert.equal(calls.filter(c=>c.method==='POST')[0].url,'/sessions/owner/administrator/notices/'+'b'.repeat(64)+'/review');
 assert.equal(renders.at(-1).rows.length,0);helper.stop();
});
test('switched sessions and unavailable broker withdraw stale build notices and review callbacks',async()=>{
 const timer=timers(),renders=[],calls=[];let response={notice:null,build_notices:[buildNotice()]};
 const helper=load().createAdministratorNotices({...timer,render:()=>{},renderBuilds:(rows,review)=>renders.push({rows,review}),
 request:async(url,method)=>{calls.push({url,method});return response;}});
 helper.start('first');await flush();const staleReview=renders.at(-1).review;
 helper.start('second');await flush();await staleReview('b'.repeat(64));
 assert.equal(calls.filter(c=>c.method==='POST').length,0);
 response={notice:null,build_notices:[buildNotice()],build_notices_unavailable:true};
 timer.tick();await flush();assert.equal(renders.at(-1).rows.length,0);helper.stop();
});
test('build reminder shared card renders literal text and reviews only its revision',async()=>{
 const parent=new Element(),doc={getElementById:()=>parent,createElement:tag=>new Element(tag)},reviewed=[];
 const render=load().buildNoticeRenderer(doc);render([{...buildNotice(),notification:'<script>literal</script>'}],async id=>reviewed.push(id));
 const region=parent.children[0],row=region.children[0];assert.match(flatten(row),/<script>literal<\/script>/);assert.equal(row.className,'chat-notification');
 await row.querySelector('button').onclick();assert.deepEqual(reviewed,['b'.repeat(64)]);render([],null);assert.equal(region.hidden,true);
});
