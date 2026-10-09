const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function fixture(profile={}) {
  const timers=new Map(),frames=new Map();let serial=0,rows=[],calls=[];
  const listeners={};
  const scroller={scrollTop:0,scrollHeight:2000,clientHeight:400,
    getBoundingClientRect:()=>({top:0}),
    scrollTo({top,behavior}){this.scrollTop=top;if(behavior!=='instant')calls.push(top);},
    addEventListener(name,fn){listeners[name]=fn;}};
  const inner={querySelectorAll:()=>rows,style:{getPropertyValue:()=>0,setProperty(){},removeProperty(){}}};
  const row=(id,top)=>({dataset:{publicId:id},getBoundingClientRect:()=>({top:top-scroller.scrollTop+62})});
  rows=[row('one',500),row('two',1000)];
  const context={AbortController,performance:{now:()=>10},matchMedia:()=>({matches:false}),
    ResizeObserver:class{observe(){}disconnect(){}},
    setTimeout(fn){const id=++serial;timers.set(id,fn);return id;},clearTimeout:id=>timers.delete(id),
    requestAnimationFrame(fn){const id=++serial;frames.set(id,fn);return id;},cancelAnimationFrame:id=>frames.delete(id),
    setInterval:()=>0,clearInterval(){}};
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(__dirname+'/transcript-anchor.js','utf8').replace('export function','function'),context);
  const api=context.mountTranscriptAnchor({scroller,inner,header:{getBoundingClientRect:()=>({bottom:0})},getProfile:()=>({user_turn_anchor:profile})});
  const flush=map=>{const tasks=[...map.values()];map.clear();tasks.forEach(fn=>fn());};
  function move(top,settle=true){listeners.wheel();scroller.scrollTop=top;listeners.scroll();if(settle)flush(timers);}
  return {api,move,calls,scroller,flushFrames:()=>flush(frames),finish:()=>listeners.scrollend(),reproject:()=>{rows=[row('one',500),row('two',1000)];}};
}
test('snap once, ignore nearby movement, rearm at 100px in either direction',()=>{
  for(const sign of [-1,1]){
    const f=fixture();f.move(500+sign*20);assert.deepEqual(f.calls,[500]);f.finish();
    f.move(500+sign*10);assert.equal(f.calls.length,1);
    f.move(500+sign*99);f.move(500+sign*15);assert.equal(f.calls.length,1);
    f.move(500+sign*100,false);f.move(500+sign*21);assert.equal(f.calls.length,1);
    f.move(500+sign*20);assert.deepEqual(f.calls,[500,500]);f.finish();
    f.move(500+sign*10);assert.equal(f.calls.length,2);
  }
});
test('send/open landing consumes the anchor, including after row reprojection',()=>{
  const f=fixture();f.api.focus();f.flushFrames();assert.equal(f.scroller.scrollTop,1000);
  f.reproject();f.move(1010);assert.equal(f.calls.length,0);
  f.move(1100,false);f.move(1010);assert.deepEqual(f.calls,[1000]);
});
test('cooldowns are per bubble and reset between conversations',()=>{
  const f=fixture();f.move(510);f.finish();f.move(1010);assert.deepEqual(f.calls,[500,1000]);f.finish();
  f.api.reset();f.move(1010);assert.deepEqual(f.calls,[500,1000,1000]);
});
test('custom device margin and rearm distance are read live',()=>{
  const profile={snap_distance_px:10,snap_rearm_distance_px:150};
  const f=fixture(profile);f.move(515);assert.equal(f.calls.length,0);f.move(510);f.finish();
  f.move(600);f.move(510);assert.equal(f.calls.length,1);
  f.move(650,false);f.move(510);assert.equal(f.calls.length,2);f.finish();
  profile.snap_distance_px=30;f.move(700,false);f.move(525);assert.equal(f.calls.length,3);
});
test('disabled snap and overlapping boundaries do not repeatedly capture',()=>{
  const disabled=fixture({snap_enabled:false});disabled.move(510);assert.equal(disabled.calls.length,0);
  const f=fixture({snap_distance_px:20,snap_rearm_distance_px:5});f.move(510);f.finish();f.move(510);assert.equal(f.calls.length,1);
});
