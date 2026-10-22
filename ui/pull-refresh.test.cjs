const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'app.js'),'utf8');
function fixture(){
  const events={},nodes={},timers=[];let pending,fetches=0,prevented=false;
  const classes=new Set();
  const document={hidden:false,body:{classList:{contains:x=>classes.has(x)}},addEventListener:(t,f)=>events[t]=f};
  const context={document,window:{scrollY:0,innerHeight:900,addEventListener:(t,f)=>events[t]=f},mobileLayout:{matches:true},
    $:id=>nodes[id]??={hidden:true,style:{},setAttribute(){},classList:{contains:()=>false,add(){},remove(){}}},
    setInterval:(f,ms)=>timers.push({f,ms}),fetch:async()=>{fetches++;return pending;}};
  const start=source.indexOf('function setupHeaderPullRefresh(){'),end=source.indexOf('setupHeaderPullRefresh();',start);
  vm.runInNewContext(source.slice(start,end)+'setupHeaderPullRefresh();',context);
  return {events,document,context,timers,classes,get fetches(){return fetches},
    async config(value,ok=true){await new Promise(setImmediate);pending={ok,json:async()=>({app_ui:{pull_to_refresh:{start_zone_px:value}}})};await timers[0].f();},
    async hold(){let resolve;pending=new Promise(r=>resolve=r);const first=timers[0].f();await timers[0].f();return {first,resolve};},
    pull(y){prevented=false;events.touchstart({touches:[{clientY:y}]});events.touchmove({touches:[{clientY:y+60}],cancelable:true,preventDefault(){prevented=true}});const armed=!nodes['ptr-indicator'].hidden;events.touchcancel();return {armed,prevented};}};
}
test('live gesture settings change start band without reload and retain last good value',async()=>{
  const f=fixture();await new Promise(setImmediate);assert.equal(f.timers[0].ms,5000);
  assert.equal(f.pull(50).armed,true);assert.equal(f.pull(51).prevented,false);
  await f.config(200);assert.equal(f.pull(200).armed,true);assert.equal(f.pull(201).prevented,false);
  for(const invalid of [-1,'300',null,NaN,Infinity]){await f.config(invalid);assert.equal(f.pull(201).armed,false);assert.equal(f.pull(200).armed,true);}
  await f.config(300,false);assert.equal(f.pull(201).armed,false);
  await f.config(0);assert.equal(f.pull(0).armed,true);assert.equal(f.pull(1).prevented,false);
  f.document.hidden=true;const before=f.fetches;await f.timers[0].f();assert.equal(f.fetches,before);
  f.document.hidden=false;await f.config(50);assert.equal(f.pull(51).armed,false);
  f.classes.add('app-restricted');await f.timers[0].f();assert.equal(f.fetches,before+1);
});
test('only one settings read is in flight and desktop/scroll guards remain',async()=>{
  const f=fixture();await f.config(50);const before=f.fetches;const held=await f.hold();assert.equal(f.fetches,before+1);
  held.resolve({ok:true,json:async()=>({app_ui:{pull_to_refresh:{start_zone_px:200}}})});await held.first;
  assert.equal(f.pull(200).armed,true);f.context.mobileLayout.matches=false;assert.equal(f.pull(20).armed,false);
  f.context.mobileLayout.matches=true;f.context.window.scrollY=6;assert.equal(f.pull(20).armed,false);
});
