const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
test('boot pixel/percent widths, drag, limits and disabled layouts',()=>{
  const events={},styles={},attrs={};let width=1200,enabled=true,resize,capture=null;
  const handle={getBoundingClientRect:()=>({width:14,left:650,top:0}),setAttribute:(k,v)=>attrs[k]=v,
    style:{setProperty(){}},classList:{add(){},remove(){}},addEventListener:(k,v)=>events[k]=v,
    setPointerCapture:id=>capture=id,hasPointerCapture:id=>capture===id,releasePointerCapture:()=>capture=null,focus(){}};
  const stage={getBoundingClientRect:()=>({width,right:width}),style:{setProperty:(k,v)=>styles[k]=v}};
  const context={document:{getElementById:id=>id==='stage'?stage:id==='chat-resize-handle'?handle:{getBoundingClientRect:()=>({width:parseFloat(styles['--chat-panel-width'])})},body:{classList:handle.classList}},window:{addEventListener(){}},
    ResizeObserver:class{constructor(fn){resize=fn}observe(){}},MutationObserver:class{observe(){}}};
  vm.createContext(context);
  const source=fs.readFileSync(__dirname+'/chat-resize.js','utf8').replace('export function','function');
  vm.runInContext(source+';globalThis.mount=mountChatResize;',context);
  const api=context.mount({enabled:()=>enabled});
  const size=()=>parseFloat(styles['--chat-panel-width']);
  api.configure({width:420,unit:'px'});assert.equal(size(),420);
  width=1500;resize();assert.equal(size(),420);
  api.configure({width:30,unit:'%'});assert(Math.abs(size()-1486*.3)<.001);
  width=1200;resize();assert(Math.abs(size()-1186*.3)<.001);
  api.configure({width:0,unit:'px'});assert(Math.abs(size()-1186*.45)<.001);
  api.configure({width:99999,unit:'px'});assert.equal(size(),906);
  for(const geometry of [{width:-1,unit:'px'},{width:101,unit:'%'},{width:20,unit:'vw'}])assert.throws(()=>api.configure(geometry));
  const event={button:0,isPrimary:true,pointerId:7,clientX:655,clientY:100,preventDefault(){}};
  events.pointerdown(event);events.pointermove({...event,clientX:555});assert.equal(size(),636);
  events.pointercancel(event);assert.equal(capture,null);
  events.keydown({key:'Home',preventDefault(){}});assert.equal(size(),240);
  events.keydown({key:'ArrowLeft',shiftKey:true,preventDefault(){}});assert.equal(size(),290);
  enabled=false;const before=size();events.keydown({key:'End',preventDefault(){}});assert.equal(size(),before);
});
