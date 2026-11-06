const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const msg=seq=>({id:'m'+seq,seq});
function fixture(profile={},saved=new Map()) {
  const rows=[],listeners={};
  const inner={get children(){return rows;},querySelectorAll:()=>rows};
  const scroller={scrollTop:0,clientHeight:400,get scrollHeight(){return rows.length*100;},getBoundingClientRect:()=>({top:0}),
    addEventListener(name,fn){listeners[name]=fn;}};
  const row=item=>{const node={dataset:{publicId:item.id,seq:String(item.seq)},remove(){rows.splice(rows.indexOf(node),1);},getBoundingClientRect:()=>({bottom:rows.indexOf(node)*100+100-scroller.scrollTop})};return node;};
  const render=item=>{if(rows.some(node=>node.dataset.publicId===item.id))return;const at=rows.findIndex(node=>Number(node.dataset.seq)>item.seq);rows.splice(at<0?rows.length:at,0,row(item));};
  let paints=0;const storage={getItem:key=>saved.get(key)??null,setItem:(key,value)=>saved.set(key,value),removeItem:key=>saved.delete(key)};
  const context={AbortController,setTimeout:fn=>{listeners.timer=fn;return 1;},clearTimeout(){},Number,Math,String};
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(__dirname+'/transcript-window.js','utf8').replace('export function','function'),context);
  const api=context.mountTranscriptWindow({scroller,inner,getProfile:()=>({transcript_window:{messages_above:3,messages_below:2,load_more_step:2,load_threshold_px:150,...profile}}),render,paint:()=>paints++,storage});
  return {api,rows,scroller,saved,scroll:top=>{scroller.scrollTop=top;listeners.scroll();},flushSave:()=>listeners.timer(),paints:()=>paints,seqs:()=>rows.map(node=>Number(node.dataset.seq))};
}
const history=n=>Array.from({length:n},(_,i)=>msg(i+1));
test('opens on the newest messages and repaints activity once',()=>{
  const f=fixture();assert.equal(f.api.open('s',history(20)),null);assert.deepEqual(f.seqs(),[18,19,20]);assert.equal(f.paints(),1);
});
test('disabled window draws every message',()=>{
  const f=fixture({enabled:false});f.api.open('s',history(8));assert.equal(f.rows.length,8);
});
test('restores the saved position with rows above and below',()=>{
  const f=fixture({},new Map([['webagent.chat.position.s','10']]));const row=f.api.open('s',history(20));
  assert.deepEqual(f.seqs(),[7,8,9,10,11,12]);assert.equal(row.dataset.seq,'10');
});
test('remember_position false ignores a saved position',()=>{
  const f=fixture({remember_position:false},new Map([['webagent.chat.position.s','10']]));assert.equal(f.api.open('s',history(20)),null);assert.deepEqual(f.seqs(),[18,19,20]);
});
test('scrolling up loads older rows, keeps the offset, and trims the far side',()=>{
  const f=fixture();f.api.open('s',history(30));f.scroll(100);assert.deepEqual(f.seqs(),[26,27,28,29,30]);assert.equal(f.scroller.scrollTop,300);
  for(let i=0;i<4;i++)f.scroll(100);assert.equal(f.rows.length,9);assert.equal(f.seqs()[0],18);
});
test('scrolling down loads newer rows when opened mid-history',()=>{
  const f=fixture({},new Map([['webagent.chat.position.s','10']]));f.api.open('s',history(20));f.scroll(f.scroller.scrollHeight-400);assert.deepEqual(f.seqs().slice(-2),[13,14]);
});
test('live messages render at the tail but wait while the reader is in older history',()=>{
  const f=fixture();f.api.open('s',history(10));assert.equal(f.api.accept(msg(11)),true);assert.equal(f.api.accept(msg(2)),false);assert.equal(f.api.accept(msg(11)),true);
  const g=fixture({},new Map([['webagent.chat.position.s','3']]));g.api.open('s',history(20));assert.equal(g.api.accept(msg(21)),false);assert.equal(g.api.accept(msg(4)),true);
});
test('saves the first visible message and clears it at the latest message',()=>{
  const f=fixture({load_threshold_px:10},new Map([['webagent.chat.position.s','10']]));f.api.open('s',history(20));f.scroll(150);f.flushSave();assert.equal(f.saved.get('webagent.chat.position.s'),'8');
  const g=fixture({load_threshold_px:10});g.api.open('s',history(10));g.scroll(g.scroller.scrollHeight-400);g.flushSave();assert.equal(g.saved.has('webagent.chat.position.s'),false);
});
test('rows that arrive before open wait for the window instead of drawing everything',()=>{
  const f=fixture();for(const item of history(12))assert.equal(f.api.accept(item),false);assert.equal(f.rows.length,0);
  f.api.open('s',history(10));assert.deepEqual(f.seqs(),[10,11,12]);
});
