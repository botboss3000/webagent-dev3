const {test} = require('node:test');
const assert = require('node:assert/strict'), fs = require('node:fs'), vm = require('node:vm');
const source = fs.readFileSync(__dirname + '/screen-awake.js', 'utf8');
const context = vm.createContext({}); vm.runInContext(source.replace(/export /g, ''), context);
const policy = {enabled:true, mobile_only:true, after_activity_ms:120000, after_input_ms:120000, dim_after_ms:30000, dim_opacity:0.8, active:'dim', inactive:'off', allow_device_overrides:false};
// Drain the asynchronous request/release boundary without advancing the simulated clock.
const settle = async () => { for (let i=0; i<12; i++) await Promise.resolve(); };
/** Fake only browser, clock and wake-lock boundaries; assert observable requests, releases and dim state. */
function fixture({config=policy, mobile=true, supported=true, defer=false, reject=false}={}) {
  let now=0, serial=0, requests=0, dim=false, sentinels=[], pending=[], timers=new Map();
  // Retain registered handlers and emulate trusted browser input at the explicit test boundary.
  const target = () => { const handlers=new Map(); return {
    addEventListener(t,f){if(!handlers.has(t))handlers.set(t,new Set());handlers.get(t).add(f);},
    removeEventListener(t,f){handlers.get(t)?.delete(f);},
    emit(t,extra={}){for(const f of handlers.get(t)||[])f({type:t,isTrusted:true,...extra});},
    count(){return [...handlers.values()].reduce((n,s)=>n+s.size,0);}
  }; };
  const doc=target(),win=target();doc.visibilityState='visible';
  // Expose browser-owned release events and observable lease lifetime.
  function sentinel(){const s=target();s.released=false;s.releases=0;s.release=async()=>{s.releases++;s.released=true;s.emit('release');};sentinels.push(s);return s;}
  const nav=supported?{wakeLock:{request:()=>{requests++;if(reject)return Promise.reject(Error('denied'));if(defer)return new Promise(resolve=>pending.push(()=>resolve(sentinel())));return Promise.resolve(sentinel());}}}:{};
  const api=context.createScreenAwake({config,doc,nav,win,mobile:()=>mobile,clock:()=>now,
    later:(f,ms)=>{const id=++serial;timers.set(id,{f,at:now+ms});return id;},cancel:id=>timers.delete(id),setDim:v=>{dim=v;}});
  // Execute scheduled expiry boundaries in time order, including asynchronous release completion.
  async function advance(ms){const end=now+ms;for(;;){const next=[...timers].filter(([,v])=>v.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];if(!next)break;now=next[1].at;timers.delete(next[0]);next[1].f();await settle();}now=end;await settle();}
  return {api,doc,win,advance,pending,sentinels,get requests(){return requests},get dim(){return dim},get timerCount(){return timers.size}};
}
test('idle history does not acquire; active work dims then message and input restore appearance', async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{id:'r',status:'completed'}]);await settle();assert.equal(f.requests,0);
  f.api.runs([{id:'r',status:'running'}]);await settle();assert.equal(f.requests,1);
  await f.advance(29999);assert.equal(f.dim,false);await f.advance(1);assert.equal(f.dim,true);
  f.api.attention('message');assert.equal(f.dim,false);await f.advance(30000);assert.equal(f.dim,true);
  f.doc.emit('touchstart');assert.equal(f.dim,false);await f.advance(120000);assert.equal(f.sentinels[0].released,false);
});
test('post-chat deadline expires once despite repeated identical polling', async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{status:'running'}]);await settle();f.api.runs([{status:'completed'}]);
  await f.advance(60000);f.api.runs([{status:'completed'}]);await f.advance(59999);assert.equal(f.sentinels[0].released,false);
  await f.advance(1);assert.equal(f.sentinels[0].released,true);assert.equal(f.dim,false);assert.equal(f.requests,1);
});
test('independent input and chat grace retain whichever expires later', async()=>{
  const f=fixture({config:{...policy,after_activity_ms:60000,after_input_ms:120000}});f.api.select('one');f.doc.emit('touchstart');await settle();
  await f.advance(10000);f.api.runs([{status:'running'}]);await f.advance(10000);f.api.runs([{status:'completed'}]);
  await f.advance(60000);assert.equal(f.sentinels[0].released,false);await f.advance(40000);assert.equal(f.sentinels[0].released,true);
  const g=fixture({config:{...policy,after_activity_ms:120000,after_input_ms:30000}});g.api.select('one');g.api.runs([{status:'running'}]);await settle();g.api.runs([{status:'completed'}]);
  await g.advance(60000);g.doc.emit('keydown');await g.advance(59999);assert.equal(g.sentinels[0].released,false);await g.advance(1);assert.equal(g.sentinels[0].released,true);
});
test('approval and uncertain recovery expire instead of holding forever; polling does not renew', async()=>{
  for(const item of [{id:'r',status:'waiting_approval',pending:{id:'a'}},{id:'r',status:'recovery',uncertain:true},{id:'r',status:'running',uncertain:true}]){
    const f=fixture();f.api.select('one');f.api.runs([item]);await settle();await f.advance(60000);f.api.runs([item]);
    await f.advance(60000);assert.equal(f.sentinels[0].released,true);f.api.runs([item]);await settle();assert.equal(f.requests,1);
  }
});
test('foreign-session runs cannot acquire and a conversation switch releases immediately', async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{session_id:'other',status:'running'}]);await settle();assert.equal(f.requests,0);
  f.api.runs([{session_id:'one',status:'running'}]);await settle();f.api.select('two');await settle();assert.equal(f.sentinels[0].released,true);assert.equal(f.timerCount,0);
});
test('background releases; foreground reacquires active work without renewing an expired grace period', async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{status:'running'}]);await settle();f.doc.visibilityState='hidden';f.doc.emit('visibilitychange');await settle();assert.equal(f.sentinels[0].released,true);
  f.doc.visibilityState='visible';f.doc.emit('visibilitychange');await settle();assert.equal(f.requests,2);assert.equal(f.dim,false);
  f.api.runs([{status:'completed'}]);f.doc.visibilityState='hidden';f.doc.emit('visibilitychange');await f.advance(180000);
  f.doc.visibilityState='visible';f.doc.emit('visibilitychange');await settle();assert.equal(f.requests,2);
});
test('acquisition is serialized and late results after expiry, switch, disable or destroy are released', async()=>{
  for(const end of ['expiry','switch','disable','destroy','hidden']){
    const f=fixture({defer:true});f.api.select('one');f.doc.emit('touchstart');f.doc.emit('touchstart');await settle();assert.equal(f.requests,1);
    if(end==='expiry')await f.advance(120000);if(end==='switch')f.api.select('two');if(end==='disable')f.api.configure({...policy,enabled:false});if(end==='destroy')f.api.destroy();if(end==='hidden'){f.doc.visibilityState='hidden';f.doc.emit('visibilitychange');}
    f.pending.shift()();await settle();assert.equal(f.sentinels[0].released,true,end);assert.equal(f.dim,false,end);
  }
});
test('old-session pending acquisition cannot become a replacement session lock', async()=>{
  const f=fixture({defer:true});f.api.select('one');f.api.runs([{status:'running'}]);await settle();f.api.select('two');f.api.runs([{status:'running'}]);await settle();assert.equal(f.requests,1);
  f.pending.shift()();await settle();assert.equal(f.sentinels[0].released,true);assert.equal(f.requests,2);f.pending.shift()();await settle();assert.equal(f.sentinels[1].released,false);
});
test('denial and OS revocation do not loop or retry on polls; new real input may retry', async()=>{
  const denied=fixture({reject:true});denied.api.select('one');denied.api.runs([{status:'running'}]);await settle();await denied.advance(120000);denied.api.runs([{status:'running'}]);await settle();assert.equal(denied.requests,1);assert.equal(denied.dim,false);
  denied.doc.emit('touchstart');await settle();assert.equal(denied.requests,2);
  const f=fixture();f.api.select('one');f.api.runs([{status:'running'}]);await settle();await f.sentinels[0].release();f.api.runs([{status:'running'}]);await f.advance(30000);assert.equal(f.requests,1);assert.equal(f.dim,false);
  f.doc.emit('keydown');await settle();assert.equal(f.requests,2);
});
test('unsupported API, desktop and disabled policy leave normal screen appearance', async()=>{
  for(const options of [{supported:false},{mobile:false},{config:{...policy,enabled:false}},{config:null}]){
    const f=fixture(options);f.api.select('one');f.api.runs([{status:'running'}]);f.doc.emit('touchstart');await f.advance(180000);assert.equal(f.requests,0);assert.equal(f.dim,false);
  }
});
test('hover and synthetic events do not renew grace; trusted contact does', async()=>{
  const f=fixture();f.api.select('one');f.doc.emit('pointermove',{pointerType:'mouse',buttons:0});f.doc.emit('input',{isTrusted:false});await settle();assert.equal(f.requests,0);
  f.doc.emit('pointerdown',{pointerType:'touch'});await settle();await f.advance(60000);f.doc.emit('input',{isTrusted:false});await f.advance(60000);assert.equal(f.sentinels[0].released,true);
});
test('settings apply immediately using existing event times; disabling dim restores appearance', async()=>{
  const f=fixture({config:{...policy,inactive:'dim'}});f.api.select('one');f.doc.emit('touchstart');await settle();await f.advance(30000);assert.equal(f.dim,true);
  f.api.configure({...policy,dim_after_ms:0});assert.equal(f.dim,false);f.api.configure({...policy,after_input_ms:20000});await settle();assert.equal(f.sentinels[0].released,true);
});
test('BFCache and teardown withdraw locks, timers and listeners', async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{status:'running'}]);await settle();f.win.emit('pagehide');await settle();assert.equal(f.sentinels[0].released,true);
  f.win.emit('pageshow');await settle();assert.equal(f.requests,2);f.api.destroy();await settle();assert.equal(f.doc.count(),0);assert.equal(f.win.count(),0);assert.equal(f.timerCount,0);assert.equal(f.sentinels[1].released,true);
});
test('device overrides validate, preserve mobile-only policy and handle storage failure',()=>{
  const load=value=>context.deviceScreenPolicy({...policy,allow_device_overrides:true},{getItem:()=>value});
  assert.equal(load(JSON.stringify({enabled:false,after_input_ms:60000})).enabled,false);
  for(const value of ['{','[]',JSON.stringify({mobile_only:false}),JSON.stringify({after_input_ms:-1}),JSON.stringify({enabled:'yes'}),JSON.stringify({dim_opacity:1})])assert.equal(load(value).enabled,true);
  assert.equal(context.deviceScreenPolicy(policy,{getItem(){throw Error('blocked')}}).after_input_ms,120000);
  for(const value of [null,{...policy,after_activity_ms:Infinity},{...policy,after_activity_ms:0.1},{...policy,dim_opacity:-1}])assert.equal(context.screenPolicy(value),null);
});

test('reopening the same conversation preserves its last-input deadline', async()=>{
  const f=fixture();f.api.select('one');f.doc.emit('touchstart');await settle();await f.advance(30000);
  f.api.select('one');await settle();assert.equal(f.sentinels[0].released,false);assert.equal(f.requests,1);
  await f.advance(89999);assert.equal(f.sentinels[0].released,false);await f.advance(1);assert.equal(f.sentinels[0].released,true);
});

// Central policy is authoritative unless the profile explicitly permits device overrides.
test('chat profile policy merges defaults and ignores local preferences unless permitted',()=>{
  assert.equal(context.profileScreenPolicy(null),null);
  assert.equal(context.profileScreenPolicy({screen_display:{after_input_ms:60000}}).after_input_ms,60000);
  assert.equal(context.profileScreenPolicy({screen_display:{active:'unknown'}}),null);
  const stored={getItem:()=>JSON.stringify({enabled:false,active:'off',after_activity_ms:5})};
  assert.equal(context.deviceScreenPolicy(policy,stored).enabled,true);
  assert.equal(context.deviceScreenPolicy({...policy,allow_device_overrides:true},stored).enabled,false);
  const legacy={getItem:()=>JSON.stringify({after_chat_ms:60000})};
  assert.equal(context.deviceScreenPolicy({...policy,allow_device_overrides:true},legacy).after_activity_ms,60000);
});
test('active and inactive on/dim/off modes have independent wake and appearance behavior',async()=>{
  for(const active of ['on','dim','off'])for(const inactive of ['on','dim','off']){
    const f=fixture({config:{...policy,active,inactive}});f.api.select('one');f.api.runs([{status:'running'}]);await settle();
    await f.advance(30000);assert.equal(f.dim,active==='dim',active+' active dim');
    await f.advance(90000);assert.equal(f.sentinels[0].released,active==='off',active+' active hold');
    f.api.runs([{status:'completed'}]);await settle();await f.advance(30000);assert.equal(f.dim,inactive==='dim',inactive+' inactive dim');
    await f.advance(90000);assert.equal(f.sentinels.at(-1).released,inactive==='off',inactive+' inactive hold');
  }
});
test('default inactive off stays bright during grace then releases; repeated profile adoption cannot retry revoked locks',async()=>{
  const f=fixture();f.api.select('one');f.api.runs([{status:'running'}]);await settle();await f.advance(30000);assert.equal(f.dim,true);
  f.api.runs([{status:'completed'}]);assert.equal(f.dim,false);await f.advance(120000);assert.equal(f.sentinels[0].released,true);
  const g=fixture();g.api.select('one');g.api.runs([{status:'running'}]);await settle();await g.sentinels[0].release();g.api.configure({...policy});await settle();assert.equal(g.requests,1);
});
