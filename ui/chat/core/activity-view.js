// @code-index {"version":1,"feature":"chat-tool-activity","aliases":["session activity groups","activity journal view"],"purpose":"Locates the administrator-gated session activity projection in chat Config: data/config/chat.json -> chat_common.show_activity_groups. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/show_activity_groups"],"profile_overrides":true}],"opportunities":[]}}
import {activityGroups, watchActivity} from './activity.js';
import {createTranscriptRow, patchTranscriptRow, mountTranscript} from './activity-transcript.js';

// Own one session-scoped private journal watch and project groups beside public transcript anchors.
export function createActivityView({app,isAdmin,chatUiFlag}) {
let session=null, stop=null, journal=null, publicState=null, container=null, error='';
const nodes=new Map();
// Install the activity stylesheet once for this document.
function styles() {
  if(document.getElementById('v2-activity-styles')) return;
  const link=document.createElement('link');link.id='v2-activity-styles';
  link.rel='stylesheet';link.href=new URL('./activity.css',import.meta.url).href;
  document.head.appendChild(link);
}
// Create inert text nodes for group labels and diagnostic headings.
function textNode(tag, text, className='') {
  const node=document.createElement(tag);node.className=className;node.textContent=text;
  return node;
}
// Reconcile an expanded group's keyed rows and raw-event disclosure without replacing stable rows.
function updateRows(node,group) {
  const viewport=node.querySelector('.v2-activity-body');
  if(!node._transcript) node._transcript=mountTranscript(viewport);
  const body=node._transcript.rows;
  const signature=JSON.stringify(group.events);
  if(body.dataset.signature===signature) return;
  body.dataset.signature=signature;
  const existing=new Map([...body.children].map(n=>[n.dataset.rowId,n]));
  const wanted=[];
  for(const row of group.rows) {
    let detail=existing.get(row.id);
    if(!detail) {detail=createTranscriptRow(row);detail.dataset.rowId=row.id;}
    patchTranscriptRow(detail,row);
    detail.dataset.category=row.category;wanted.push(detail);
  }
  let raw=existing.get('raw');
  if(!raw) {
    raw=document.createElement('details');raw.dataset.rowId='raw';
    raw.appendChild(textNode('summary',''));
    raw.appendChild(textNode('pre',''));
    // Populate raw JSON only while the disclosure is open.
    raw.addEventListener('toggle',()=>{
      raw.lastElementChild.textContent=raw.open?JSON.stringify(raw._events,null,2):'';
    });
  }
  raw._events=group.events;
  raw.firstElementChild.textContent='All '+group.events.length+' diagnostic events';
  if(raw.open) raw.lastElementChild.textContent=JSON.stringify(group.events,null,2);
  wanted.push(raw);
  for(const child of [...body.children]) if(!wanted.includes(child)) child.remove();
  let before=body.firstChild;
  for(const child of wanted) {
    if(child!==before) body.insertBefore(child,before);
    before=child.nextSibling;
  }
  node._transcript.update();
}
// Place groups after matching public-message anchors and refresh labels from private journal state.
function paint() {
  if(!journal || !publicState || !container || session!==app.currentSessionId) return;
  const groups=activityGroups(journal,publicState.runs), keep=new Set(), tails=new Map();
  const progressActive=document.querySelector('#chat-above-pill.visible')!==null;
  document.querySelector('#session-dropdown-icon')?.classList.toggle('activity-running',progressActive);
  const lastGroup=groups[groups.length-1];
  for(const group of groups) {
    const boundary=[...container.children].find(n=>n.dataset.publicId===group.boundary_item_id);
    if(!boundary) continue; // The corresponding public page has not loaded.
    keep.add(group.id);
    let node=nodes.get(group.id);
    if(!node) {
      node=document.createElement('details');node.className='v2-activity-group';
      node.dataset.activityGroupId=group.id;
      node.appendChild(textNode('summary','Activity'));
      node.appendChild(textNode('div','','v2-activity-body'));
      // Lazily build row DOM only when the activity group is expanded.
      node.addEventListener('toggle',()=>{if(node.open && node._group) updateRows(node,node._group);});
      nodes.set(group.id,node);
    }
    node._group=group;
    // Summarize nonzero category counts without changing the underlying journal rows.
    const parts=Object.entries(group.counts).filter(([,count])=>count)
      .map(([kind,count])=>count+' '+(count===1?kind.replace(/s$/,''):kind));
    const phase={running:'Working…',queued:'Queued…',waiting_external:'Waiting for build…',finalizing:'Checking response…',
      stopping:'Stopping…',completed:'Completed',failed:'Failed',cancelled:'Stopped',
      finished:'Finished'}[group.state] || group.state;
    const progress=group.kind==='compaction' && ['queued','running','finalizing'].includes(group.state)
      ? (group.state==='queued' ? 'Compaction queued…' : 'Compacting…') : phase;
    node.firstElementChild.textContent='Activity'+(parts.length?' · '+parts.join(' · '):'')+
      ' · '+(error || progress)+(group.boundary_quality==='legacy_unverified'?' · Legacy boundary unverified':'');
    node.setAttribute('aria-label','Activity group');
    node.classList.toggle('v2-activity-group-glimmer',progressActive && group===lastGroup);
    if(node.open) updateRows(node,group);
    const anchor=tails.get(group.boundary_item_id) || boundary;
    if(anchor.nextSibling!==node) container.insertBefore(node,anchor.nextSibling);
    tails.set(group.boundary_item_id,node);
  }
  for(const [id,node] of nodes) if(!keep.has(id)) {node._transcript?.dispose();node.remove();nodes.delete(id);}
  app._reLandAnchor?.();
}
// Stop the current watch and dispose rendered groups before changing session or policy.
function disconnectActivity() {
  stop?.();stop=null;
  document.querySelector('#session-dropdown-icon')?.classList.remove('activity-running');
  for(const node of nodes.values()) {node._transcript?.dispose();node.remove();}
  nodes.clear();session=null;journal=null;publicState=null;container=null;error='';
}
// Enforce revocation, feature-flag, and administrator gates before watching this session.
function renderActivity(state,target) {
  if(state.revoked || !chatUiFlag('show_activity_groups',false) || !isAdmin()) {
    disconnectActivity();return;
  }
  if(session!==state.sessionId) {
    disconnectActivity();session=state.sessionId;styles();
  }
  container=target;publicState=state;
  if(!stop) {
    const id=session;
    // Fence callbacks to this session; transient watch failures remain visible while polling retries.
    stop=watchActivity({sessionId:id,
      // Request private pages with same-origin credentials and abort support.
      request:(cursor,signal)=>fetch(('/api/v3/sessions/'+encodeURIComponent(id)+'/activity?after='+cursor),
        {credentials: "same-origin",signal,cache:'no-store'}),
      // Publish only while this view still owns the session and reflect remaining pagination.
      onChange:(value,more)=>{
        if(session!==id) return;
        journal=value;error=more?'Loading activity…':'';paint();
      },
      // Authorization or missing-session failures disconnect; other errors leave a retry status.
      onError:failure=>{
        if(session!==id) return;
        if([401,403,404].includes(failure.status)) {disconnectActivity();return;}
        error='Activity connection lost; retrying…';paint();
      }});
  }
  paint();
}
// The panel owns render/disconnect and calls disconnect on session change or teardown.
return {render:renderActivity,disconnect:disconnectActivity};
}
