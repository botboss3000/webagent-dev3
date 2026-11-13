// Coordinates chat elements and authorized private activity.
import {activityRows} from '../core/activity.js';
import {createMiniTranscript} from './mini-transcript/index.js';
import {createActivityGroup,activityLabel} from './activity-group/index.js';
import {createTypingEllipsis} from './typing-ellipsis/index.js';

// UI preview captures: `/app?session_id=<id>&ui_sample=activity` replaces the session's activity with one
// fixed sample group after its first message, so screenshots never depend on live work. Static text only;
// no journal is read.
const sample=new URLSearchParams(location.search).get('ui_sample')==='activity';
const SAMPLE_EVENTS=[
  ['agent_step_end',{asst_id:'s1',content:'Reading the activity group styles before changing the layout.'}],
  ['tool_call',{tool:'file_read',tool_call_id:'c1'}],['tool_result',{tool:'file_read',tool_call_id:'c1',output:'{"status":"success"}'}],
  ['agent_step_end',{asst_id:'s2',content:'Writing the step rail variant.'}],
  ['tool_call',{tool:'file_write',tool_call_id:'c2'}],
  ['tool_result',{tool:'file_write',tool_call_id:'c2',output:'{"status":"error","output":"Path is outside the session workspace"}'}],
  ['tool_call',{tool:'file_write',tool_call_id:'c3'}],['tool_result',{tool:'file_write',tool_call_id:'c3',output:'{"status":"success"}'}],
  ['agent_step_end',{asst_id:'s3',content:'Capturing desktop and phone screenshots.'}],
  ['tool_call',{tool:'browser_input',tool_call_id:'c4'}],['tool_result',{tool:'browser_input',tool_call_id:'c4',output:'{"status":"success"}'}],
].map(([kind,payload],index)=>({session_id:'sample',run_id:'sample',event_id:'sample-'+(index+1),seq:index+1,kind,payload}));
let sampleShown=false;
async function sampleGet(url) {
  if(url.includes('/logs?'))return {events:SAMPLE_EVENTS,cursor:SAMPLE_EVENTS.length,has_more:false};
  const first=new URL(url,location.origin).searchParams.get('boundaries')?.split(',')[0];
  if(!first || sampleShown)return {segments:[],cursor:0};
  sampleShown=true;
  return {segments:[{id:'sample',run_id:'sample',boundary_item_id:first,first_seq:1,last_seq:SAMPLE_EVENTS.length,
    closed:true,latest:false,last_of_run:true,counts:{tools:4,updates:3,responses:0,modes:0,compactions:0}}],cursor:0};
}
// Own session-scoped chat elements and a separately authorized private activity view.
// Group summaries (counts, flags) load only for drawn messages; a group's events load only
// when it is expanded, so the private journal is never downloaded wholesale.
export function createChatElements({app,isAdmin,chatUiFlag}) {
let session=null, publicState=null, container=null, error='', enabled=false;
let summaries=new Map(), requested=new Set(), cursor=null, timer=0, busy=false, generation=0;
const nodes=new Map(), clusters=new Map();
let ellipsis=null;
// Load each element-owned stylesheet once across session changes.
function styles() {
  for(const name of ['activity-group','mini-transcript','typing-ellipsis']) {
    const id='chat-element-style-'+name;
    if(document.getElementById(id))continue;
    const link=document.createElement('link');link.id=id;link.rel='stylesheet';
    link.href=new URL('./'+name+'/style.css',import.meta.url).href;document.head.append(link);
  }
}
const base=id=>'/api/v3/sessions/'+encodeURIComponent(id);
const get=(url,signal)=>sample?sampleGet(url):fetch(url,{credentials:'same-origin',cache:'no-store',signal}).then(response=>{
  if(response.ok)return response.json();
  const failure=new Error('Activity unavailable ('+response.status+')');failure.status=response.status;throw failure;});
// Derives display state from the server's segment flags and the matching run snapshot.
function displayed(summary,runs) {
  const run=runs.find(r=>r.id===summary.run_id);
  const finalOutcome=summary.last_of_run && ['completed','failed','cancelled'].includes(run?.state);
  // A stop is superseded once the conversation resumes in any later segment.
  const resumed=run?.state==='cancelled' && !summary.latest;
  return {...summary,kind:run?.kind,rows:summary.rows||[],events:summary.events||[],
    state:resumed || (summary.closed && !finalOutcome)?'finished':run?.state || 'queued'};
}
// Folds summary segments in; a newer latest segment closes the others, and a later segment of the same run ends `last_of_run`.
function merge(segments) {
  for(const segment of segments) {
    for(const other of summaries.values()) {
      if(other.id===segment.id)continue;
      if(segment.latest){other.latest=false;other.closed=true;}
      if(segment.last_of_run && other.run_id===segment.run_id)other.last_of_run=false;
    }
    const old=summaries.get(segment.id);
    summaries.set(segment.id,{...segment,events:old?.events,rows:old?.rows});
    if(old && nodes.get(segment.id)?.element.open && old.last_seq!==segment.last_seq)void loadEvents(segment.id);
  }
}
// Loads one group's events from its run log, bounded to the group's sequence range.
async function loadEvents(id) {
  const summary=summaries.get(id), epoch=generation;
  if(!summary)return;
  try {
    const events=[];let after=summary.first_seq-1;
    for(;;) {
      const page=await get(base(session)+'/runs/'+encodeURIComponent(summary.run_id)+'/logs?after='+after);
      if(epoch!==generation)return;
      events.push(...page.events.filter(event=>event.seq<=summary.last_seq));
      if(!page.has_more || page.cursor>=summary.last_seq || page.cursor===after)break;
      after=page.cursor;
    }
    const current=summaries.get(id);
    if(current){current.events=events;current.rows=activityRows(events);}
  } catch(failure) {if(epoch!==generation)return;error='Activity details unavailable';}
  paint();
}
// Fetches summaries for newly drawn messages, then groups that changed since the cursor.
async function refresh() {
  if(busy || !session)return;
  busy=true;const epoch=generation,id=session;
  try {
    const drawn=[...container?.children||[]].map(node=>node.dataset.publicId).filter(value=>value && !requested.has(value));
    // An empty window still needs a cursor for live changes.
    if(cursor===null && !drawn.length){const page=await get(base(id)+'/activity?view=summary&boundaries=');if(epoch!==generation)return;cursor=page.cursor;}
    for(let index=0;index<drawn.length;index+=100) {
      const batch=drawn.slice(index,index+100);
      const page=await get(base(id)+'/activity?view=summary&boundaries='+encodeURIComponent(batch.join(',')));
      if(epoch!==generation)return;
      batch.forEach(value=>requested.add(value));merge(page.segments);
      // Groups drawn now are current as of this page; later polls only need newer changes.
      if(cursor===null)cursor=page.cursor;
    }
    if(cursor!==null) {
      const page=await get(base(id)+'/activity?view=summary&after='+cursor);
      if(epoch!==generation)return;
      merge(page.segments);cursor=page.cursor;
    }
    error='';
  } catch(failure) {
    if(epoch!==generation)return;
    if([401,403,404].includes(failure.status)){disconnectActivity();return;}
    error='Activity connection lost; retrying…';
  } finally {if(epoch===generation)busy=false;}
  paint();
}
function schedule(){clearTimeout(timer);if(enabled)timer=setTimeout(()=>{void refresh().finally(schedule);},2000);}
// Place groups after matching public-message anchors and refresh labels from summary state.
function paint() {
  if(!enabled || !publicState || !container || session!==app.currentSessionId) return;
  const progressActive=document.querySelector('#chat-above-pill.visible')!==null;
  document.querySelector('#session-dropdown-icon')?.classList.toggle('activity-running',progressActive);
  const rows=new Map([...container.children].map(n=>[n.dataset.publicId,n]));
  const ordered=[...summaries.values()].sort((a,b)=>a.first_seq-b.first_seq);
  const lastGroup=ordered.at(-1), keep=new Set(), tails=new Map(), shown=new Map();
  for(const summary of ordered) {
    const boundary=rows.get(summary.boundary_item_id);
    if(!boundary) continue; // Its message is outside the drawn window.
    keep.add(summary.id);
    let entry=nodes.get(summary.id);
    if(!entry) {
      const transcript=createMiniTranscript(),id=summary.id;
      // Expanding is the only path that downloads a group's events.
      entry=createActivityGroup({transcript:{...transcript,open(){transcript.open();if(!summaries.get(id)?.events)void loadEvents(id);}}});
      nodes.set(id,entry);
    }
    const node=entry.element,group=displayed(summary,publicState.runs||[]);
    entry.update(group,{error,active:progressActive && summary===lastGroup});shown.set(node,group);
    const anchor=tails.get(summary.boundary_item_id) || boundary;
    if(anchor.nextSibling!==node) container.insertBefore(node,anchor.nextSibling);
    tails.set(summary.boundary_item_id,node);
  }
  for(const [id,entry] of nodes) if(!keep.has(id)) {entry.dispose();nodes.delete(id);}
  cluster(shown,progressActive);
  if(ellipsis && container.lastElementChild!==ellipsis.element)container.append(ellipsis.element);
  app._reLandAnchor?.();
  // Newly drawn messages (window scroll, live replies) fetch their summaries right away.
  if([...rows.keys()].some(value=>value && !requested.has(value)))void refresh();
}
// Folds each unbroken run of activity groups and background notice rows (until the agent's next
// visible message) under one summary; members stay in place and show when it is expanded.
function cluster(shown,active) {
  const used=new Set();let span=[];
  const build=members=>{
    const key=members[0].dataset.publicId||members[0].dataset.activityGroupId;used.add(key);
    let element=clusters.get(key);
    if(!element){
      const transcript=createMiniTranscript();
      // One disclosure and one scroller: members render inside this transcript, never beside it.
      element=createActivityGroup({transcript:{...transcript,
        open(){transcript.open();for(const id of element._memberIds||[])if(!summaries.get(id)?.events)void loadEvents(id);}}}).element;
      element.classList.add('v3-activity-cluster');element._transcript=transcript;
      element.addEventListener('toggle',paint);clusters.set(key,element);
    }
    if(members[0].previousSibling!==element)container.insertBefore(element,members[0]);
    const groups=members.filter(node=>shown.has(node)).map(node=>shown.get(node)),counts={};
    for(const group of groups)for(const [kind,count] of Object.entries(group.counts||{}))counts[kind]=(counts[kind]||0)+count;
    // Background notice rows count by kind: sub-agents, builds and background calls.
    const kinds={};
    for(const node of members){
      let parsed={};try{parsed=JSON.parse(node.dataset.noticeKinds||'{}');}catch{}
      if(!node.dataset.noticeKinds&&node.dataset.subagentNotices)parsed={'sub-agent':Number(node.dataset.subagentNotices)};
      for(const [kind,count] of Object.entries(parsed))kinds[kind]=(kinds[kind]||0)+count;
    }
    // Spawn calls already count their sub agents; notice rows report the same ones.
    const reported=kinds['sub-agent']||0;delete kinds['sub-agent'];
    counts.subagents=counts.subagents||reported;
    element.firstElementChild.textContent=activityLabel({...(groups.at(-1)||{state:'completed'}),counts},
      {error,extra:Object.entries(kinds).map(([kind,count])=>count+' '+kind+(count===1?'':'s'))});
    element.setAttribute('aria-label','Activity group');
    element.classList.toggle('v2-activity-group-glimmer',active && members.some(node=>node.classList.contains('v2-activity-group-glimmer')));
    // Merge every member's rows, or its build and background notice lines, in document order.
    const entries=[],events=[];
    for(const node of members){
      const group=shown.get(node);
      if(group){
        for(const row of group.rows)entries.push({id:(node.dataset.activityGroupId||'')+':'+row.id,row});
        events.push(...group.events);
      } else {
        [...node.children].forEach((child,index)=>entries.push({id:node.dataset.publicId+':'+index,node:child,signature:child.textContent}));
      }
    }
    element._memberIds=members.filter(node=>shown.has(node)).map(node=>node.dataset.activityGroupId);
    element._transcript.render(entries,events);
    // Members stay in place as anchors and counts; this transcript is their only visible surface.
    for(const node of members)node.classList.add('v3-cluster-member','v3-cluster-hidden');
  };
  const flush=()=>{
    const groups=span.filter(node=>shown.has(node)).length;
    if(span.length>1 && (groups>1 || span.some(node=>node.dataset.subagentNotices)))build(span);
    else for(const node of span)node.classList.remove('v3-cluster-member','v3-cluster-hidden');
    span=[];
  };
  for(const node of [...container.children]) {
    if(node.classList.contains('v3-activity-cluster'))continue;
    if(shown.has(node) || node.dataset.subagentNotices)span.push(node);else flush();
  }
  flush();
  for(const [key,element] of clusters) if(!used.has(key)){element._transcript?.dispose();element.remove();clusters.delete(key);}
}
// Stop polling and drop every private summary and group, for a policy change or a new session.
function stopActivity() {
  ++generation;enabled=false;busy=false;clearTimeout(timer);
  summaries=new Map();requested=new Set();cursor=null;sampleShown=false;
  for(const entry of nodes.values())entry.dispose();nodes.clear();
  for(const element of clusters.values()){element._transcript?.dispose();element.remove();}clusters.clear();
  for(const node of container?.querySelectorAll('.v3-cluster-member')||[])node.classList.remove('v3-cluster-member','v3-cluster-hidden');
  document.querySelector('#session-dropdown-icon')?.classList.remove('activity-running');
}
// Stop the current watch and dispose rendered groups before changing session or policy.
function disconnectActivity() {
  stopActivity();
  ellipsis?.dispose();ellipsis=null;
  session=null;publicState=null;container=null;error='';
}
// Enforce revocation, feature-flag, and administrator gates before watching this session.
function renderActivity(state,target) {
  if(state.revoked) {
    disconnectActivity();return;
  }
  if(session!==state.sessionId) {
    disconnectActivity();session=state.sessionId;styles();
  }
  container=target;publicState=state;
  const config=chatUiFlag('chat_elements',{});
  if(!ellipsis){ellipsis=createTypingEllipsis();container.append(ellipsis.element);}
  ellipsis.update((state.runs||[]).filter(run=>!run.session_id||run.session_id===session),{enabled:config?.typing_ellipsis?.enabled!==false});
  // Preserve the tail position as public messages and activity groups arrive.
  if(container.lastElementChild!==ellipsis.element)container.append(ellipsis.element);
  if(!chatUiFlag('show_activity_groups',false) || config?.activity_group?.enabled===false || !isAdmin()) {
    stopActivity();return;
  }
  if(!enabled){enabled=true;void refresh().finally(schedule);}
  paint();
}
// The panel owns render/disconnect and calls disconnect on session change or teardown.
return {render:renderActivity,disconnect:disconnectActivity};
}
