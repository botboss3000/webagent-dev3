// @code-index {"version":1,"feature":"chat-tool-activity","aliases":["session activity groups","activity journal view"],"purpose":"Coordinates chat elements and authorized private activity. Config: data/config/chat.json -> chat_common.chat_elements and show_activity_groups, merged with desktop/mobile/widget profiles.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/show_activity_groups","/chat_common/chat_elements"],"profile_overrides":true}],"opportunities":[]}}
import {activityGroups, watchActivity} from '../core/activity.js';
import {createMiniTranscript} from './mini-transcript/index.js';
import {createActivityGroup} from './activity-group/index.js';
import {createTypingEllipsis} from './typing-ellipsis/index.js';

// Own session-scoped chat elements and a separately authorized private journal watch.
export function createChatElements({app,isAdmin,chatUiFlag}) {
let session=null, stop=null, journal=null, publicState=null, container=null, error='';
const nodes=new Map();
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
    let entry=nodes.get(group.id);
    if(!entry){entry=createActivityGroup({transcript:createMiniTranscript()});nodes.set(group.id,entry);}
    const node=entry.element;
    entry.update(group,{error,active:progressActive && group===lastGroup});
    const anchor=tails.get(group.boundary_item_id) || boundary;
    if(anchor.nextSibling!==node) container.insertBefore(node,anchor.nextSibling);
    tails.set(group.boundary_item_id,node);
  }
  for(const [id,entry] of nodes) if(!keep.has(id)) {entry.dispose();nodes.delete(id);}
  if(ellipsis && container.lastElementChild!==ellipsis.element)container.append(ellipsis.element);
  app._reLandAnchor?.();
}
// Stop the current watch and dispose rendered groups before changing session or policy.
function disconnectActivity() {
  stop?.();stop=null;
  document.querySelector('#session-dropdown-icon')?.classList.remove('activity-running');
  for(const entry of nodes.values())entry.dispose();
  ellipsis?.dispose();ellipsis=null;
  nodes.clear();session=null;journal=null;publicState=null;container=null;error='';
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
    stop?.();stop=null;journal=null;
    for(const entry of nodes.values())entry.dispose();nodes.clear();
    document.querySelector('#session-dropdown-icon')?.classList.remove('activity-running');return;
  }
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
