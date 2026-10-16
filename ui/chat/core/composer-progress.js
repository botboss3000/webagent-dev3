// @code-index {"version":1,"feature":"chat-composer-progress","aliases":["composer activity progress","tool progress panel"],"purpose":"Locates the composer activity summary and current-run tool details Config: data/config/chat.json -> chat_common.active_footer.above_pill and chat_common.idle_footer.above_pill (rows and controls.activity). Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/active_footer/above_pill","/chat_common/idle_footer/above_pill"],"profile_overrides":true}],"opportunities":[]}}
'use strict';
// V2 centered progress control adapted to V3 snapshots. Never publishes chat.
import {activityRows,watchActivity} from './activity.js';
import {createTranscriptRow,patchTranscriptRow} from '../elements/mini-transcript/rows.js';
// Mount the profile-positioned progress control; its activity details are read-only.
export function mountComposerProgress({root,getProfile,isAdmin}) {
  const host=root.querySelector('#chat-above-pill');
  const wrap=document.createElement('div');wrap.className='chat-activity-wrapper';
  const panel=document.createElement('div');panel.id='chat-activity-panel';panel.className='chat-activity-panel';panel.hidden=true;panel.setAttribute('role','region');panel.setAttribute('aria-label','Tool calls this run');
  const bar=document.createElement('button');bar.type='button';bar.id='chat-activity-bar';bar.className='chat-activity-bar';bar.setAttribute('aria-controls',panel.id);bar.setAttribute('aria-expanded','false');
  const text=document.createElement('span');text.className='chat-activity-text';text.setAttribute('aria-live','polite');text.setAttribute('aria-atomic','true');
  const chevron=document.createElement('span');chevron.className='chat-activity-chevron';chevron.textContent='›';chevron.setAttribute('aria-hidden','true');bar.append(text,chevron);wrap.append(panel,bar);host.prepend(wrap);
  const css=document.createElement('link');css.rel='stylesheet';css.href=new URL('./composer-progress.css',import.meta.url).href;document.head.append(css);
  let session='',run=null,label='',started=0,stop=null,events=[],disposed=false,optimistic='',generation=0;
  // Close the details panel and remove expanded styling.
  function close(){panel.hidden=true;bar.setAttribute('aria-expanded','false');host.classList.remove('expanded');}
  // Toggle only when there are tool rows to display.
  bar.onclick=()=>{if(!panel.childElementCount)return;panel.hidden=!panel.hidden;bar.setAttribute('aria-expanded',String(!panel.hidden));host.classList.toggle('expanded',!panel.hidden);};
  // Re-read placement when profile/footer mode changes and update elapsed progress presentation.
  function render(){
    if(disposed)return;
    const profile=getProfile()||{},footer=profile[root.querySelector('#chat-input-area').classList.contains('footer-mode-active')?'active_footer':'idle_footer'];
    const cfg=footer?.above_pill;
    // Resolve the first profile row/zone that assigns the activity control.
    const placement=(cfg?.rows||[]).flatMap((row,index)=>['left','center','right'].filter(zone=>row[zone]?.includes('activity')).map(zone=>({index,zone})))[0];
    const control=cfg?.controls?.activity||{};
    const placed=cfg?.enabled!==false && !!placement && control.enabled!==false;
    wrap.dataset.headerControl='activity';wrap.dataset.controlsRow=String(placement?.index??0);
    wrap.dataset.zone=placement?.zone||'center';wrap.dataset.presentation=control.presentation||'progress-note';
    wrap.style.alignItems=({left:'flex-start',center:'center',right:'flex-end'})[wrap.dataset.zone];
    wrap.hidden=!label||!placed;host.classList.toggle('visible',!!label&&placed);bar.classList.toggle('ambient',!!label&&!run);
    const seconds=started?Math.floor((performance.now()-started)/1000):0;
    text.textContent=label+(label&&seconds>=1?' · '+seconds+'s':'');
    root.querySelector('#chat-input-row').classList.toggle('thinking',!!run);
  }
  // Reset the elapsed-time origin only when the visible label changes.
  function setLabel(value){if(value!==label){label=value;started=value?performance.now():0;}render();}
  // Reconcile current-run tool rows and choose optimistic or run-derived progress text.
  function details(){
    // Restrict displayed tool calls to this run; the broader journal can include prior runs.
    const rows=activityRows(events.filter(e=>e.run_id===run?.id)).filter(r=>r.category==='tools');
    const existing=new Map([...panel.children].map(node=>[node.dataset.rowId,node]));
    const wanted=[];for(const row of rows){const node=existing.get(row.id)||createTranscriptRow(row);node.dataset.rowId=row.id;patchTranscriptRow(node,row);wanted.push(node);}
    for(const node of [...panel.children])if(!wanted.includes(node))node.remove();
    for(const node of wanted)panel.append(node);
    bar.classList.toggle('has-tools',rows.length>0);if(!rows.length)close();
    // A call without its result is the current tool represented by the progress label.
    const pending=rows.find(r=>!r.events.some(e=>e.kind==='tool_result'));
    const phase={queued:'Queued…',waiting_approval:'Waiting for approval…',waiting_gate:'Checking response…',waiting_external:'Waiting for build…',running:'Working…'};
    setLabel(optimistic||(run?(run.status!=='running'?phase[run.status]:pending?'Calling '+pending.events[0].payload.tool+'…':'Working…'):''));
  }
  // Invalidate stale callbacks, stop the prior watch, and clear session-specific progress state.
  function reset(id=''){++generation;stop?.();stop=null;session=id;run=null;events=[];optimistic='';panel.replaceChildren();close();setLabel('');}
  // Start a fresh session watch; private activity is requested only for administrators.
  function open(id){
    reset(id);const epoch=generation;setLabel('Assessing…');
    if(id&&isAdmin())stop=watchActivity({sessionId:id,
      // Read activity pages with cancellation tied to this watch.
      request:(cursor,signal)=>fetch('/api/v3/sessions/'+encodeURIComponent(id)+'/activity?after='+cursor,{credentials:'same-origin',cache:'no-store',signal}),
      // Discard late pages after switching sessions; accepted pages refresh the detail rows.
      onChange:value=>{if(session!==id||epoch!==generation)return;events=value.events;details();},
      // Authorization or session loss clears private details; transient errors are retried by the watcher.
      onError:error=>{if(epoch!==generation)return;if([401,403,404].includes(error.status)){events=[];panel.replaceChildren();close();details();}}
    });
  }
  // Select this session's active run and refresh details from its latest run snapshot.
  function update(runs){const next=runs.filter(r=>!r.session_id||r.session_id===session).find(r=>['queued','running','waiting_approval','waiting_gate','waiting_external'].includes(r.status));if(next?.id!==run?.id)close();run=next||null;optimistic='';details();}
  const timer=setInterval(render,1000);
  const observer=new MutationObserver(render);observer.observe(root.querySelector('#chat-input-area'),{attributes:true,attributeFilter:['class']});
  return {open,update,
    // Surface an optimistic action label until the next run update or reset.
    note(value){optimistic=value;setLabel(value);},reset,
    // Stop mount-owned timers/watchers and remove its DOM and stylesheet.
    destroy(){reset();disposed=true;clearInterval(timer);observer.disconnect();bar.onclick=null;wrap.remove();css.remove();}};
}
