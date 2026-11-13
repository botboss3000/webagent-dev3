'use strict';
// Renders only a window of a session's messages around the reader's saved position or the latest message, loading and dropping rows as the reader scrolls.
/** Keeps the transcript DOM to a bounded slice of the session's messages. render(item) draws one row; paint() runs once after a batch. */
export function mountTranscriptWindow({scroller,inner,getProfile,render,paint,storage=globalThis.localStorage}) {
  const abort=new AbortController();
  let id=null,all=[],start=0,end=0,busy=false,saveTimer=0,landing=null,landTimers=[];
  // Rows that arrive (for example from a queue refresh) before open() are merged into its window.
  let early=new Map();
  /** Reads the live profile; a missing or disabled section renders every message as before. */
  const config=()=>{const value=getProfile()?.transcript_window||{};const n=(key,fallback)=>Number.isInteger(value[key])&&value[key]>0?value[key]:fallback;
    return {enabled:value.enabled!==false,above:n('messages_above',30),below:n('messages_below',20),step:n('load_more_step',20),threshold:n('load_threshold_px',600),remember:value.remember_position!==false};};
  const key=()=>'webagent.chat.position.'+id;
  function readSaved(){try{const seq=Number(storage?.getItem(key()));return Number.isFinite(seq)&&seq>0?seq:0;}catch{return 0;}}
  function writeSaved(seq){try{if(seq)storage?.setItem(key(),String(seq));else storage?.removeItem(key());}catch{/* Position memory is optional. */}}
  const rowFor=item=>[...inner.children].find(row=>row.dataset.publicId===item.id);
  /** Draws all[from,to) without per-row activity repaints, then repaints once. */
  function draw(from,to){for(let i=from;i<to;i++)render(all[i]);paint();}
  /** Removes all[from,to) rows; paint() then drops their orphaned activity groups. */
  function drop(from,to){for(let i=from;i<to;i++)rowFor(all[i])?.remove();}
  /** Keeps the first visible row in place while rows above it change; correct whether or not the browser's own scroll anchoring already adjusted. */
  function keepOffset(change){const top=scroller.getBoundingClientRect().top,ref=[...inner.querySelectorAll('[data-public-id][data-seq]')].find(node=>node.getBoundingClientRect().bottom>top),before=ref?.getBoundingClientRect().top;
    change();if(ref?.isConnected)scroller.scrollTop+=ref.getBoundingClientRect().top-before;}
  /** Stops re-landing a restored position once the reader moves. */
  function stopLanding(){landing=null;landTimers.forEach(clearTimeout);landTimers=[];}
  /** Opens a session: draws the window around the saved position, or the newest messages. Returns the row to land on, if any. */
  function open(sessionId,messages){
    const merged=new Map([...early.values(),...messages].map(item=>[item.id,item]));early=new Map();
    id=sessionId;all=[...merged.values()].sort((a,b)=>a.seq-b.seq);const c=config();
    if(!c.enabled){start=0;end=all.length;draw(start,end);return null;}
    const saved=c.remember?readSaved():0,index=saved?all.findIndex(item=>item.seq>=saved):-1;
    if(index>=0){start=Math.max(0,index-c.above);end=Math.min(all.length,index+c.below+1);}
    else{end=all.length;start=Math.max(0,end-c.above);}
    draw(start,end);
    return index>=0&&end<all.length?rowFor(all[index])||null:null;
  }
  /** Admits a live or recovered message; returns whether it belongs in the drawn window. */
  function accept(item){
    if(!id){early.set(item.id,item);return false;}
    const at=all.findIndex(row=>row.id===item.id);
    if(at>=0){all[at]=item;return at>=start&&at<end;}
    let index=all.length;while(index>0&&all[index-1].seq>item.seq)index--;
    const atTail=end===all.length;all.splice(index,0,item);
    if(index<start){start++;end++;return false;}
    if(index<end||atTail){end++;return true;}
    return false;
  }
  /** Extends the window toward the reader and trims the far side to its configured size. */
  function update(){
    if(!id||busy)return;const c=config();if(!c.enabled)return;
    const limit=c.above+c.below+2*c.step;busy=true;
    try{
      if(scroller.scrollTop<c.threshold&&start>0){
        const from=Math.max(0,start-c.step);keepOffset(()=>draw(from,start));start=from;
        if(end-start>limit){const to=start+limit;drop(to,end);end=to;paint();}
      }else if(scroller.scrollHeight-scroller.clientHeight-scroller.scrollTop<c.threshold&&end<all.length){
        const to=Math.min(all.length,end+c.step);draw(end,to);end=to;
        if(end-start>limit){const from=end-limit;keepOffset(()=>{drop(start,from);paint();});start=from;}
      }
    }finally{busy=false;}
  }
  /** Saves the first visible message, or clears the position when the reader is at the latest message. */
  function save(){
    if(!id||landing||!config().remember)return;
    const atTail=end===all.length&&scroller.scrollHeight-scroller.clientHeight-scroller.scrollTop<=config().threshold/4;
    if(atTail){writeSaved(0);return;}
    const top=scroller.getBoundingClientRect().top;
    const row=[...inner.querySelectorAll('[data-public-id][data-seq]')].find(node=>node.getBoundingClientRect().bottom>top);
    writeSaved(row?Number(row.dataset.seq):0);
  }
  for(const type of ['wheel','touchstart','pointerdown','keydown'])scroller.addEventListener(type,stopLanding,{passive:true,signal:abort.signal});
  scroller.addEventListener('scroll',()=>{update();clearTimeout(saveTimer);saveTimer=setTimeout(save,250);},{passive:true,signal:abort.signal});
  return {open,accept,update,
    /** Lands a restored row at the top and re-lands it while late layout (images, activity groups) settles, until the reader scrolls. */
    land(row){stopLanding();landing=row;const go=()=>{if(landing===row&&row.isConnected)row.scrollIntoView({block:'start'});};go();landTimers=[100,300,700,1200].map(ms=>setTimeout(go,ms));landTimers.push(setTimeout(stopLanding,1300));},
    /** Forgets the session; the next open starts a fresh window. */
    reset(){stopLanding();id=null;all=[];early=new Map();start=end=0;clearTimeout(saveTimer);},
    destroy(){stopLanding();clearTimeout(saveTimer);abort.abort();}};
}
