// @code-index {"version":1,"feature":"chat-elements","aliases":["chat elements","activity group","typing ellipsis","mini transcript"],"purpose":"Owns modular chat elements. Config: ui/chat/chat.json -> chat_common.chat_elements and show_activity_groups, merged with desktop/mobile/widget profiles.","span_lines":63,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common/chat_elements","/chat_common/show_activity_groups"],"profile_overrides":true}],"opportunities":[]}}
// Mini transcript owns journal rows, scrolling and disclosure independently of its trigger.
import {createTranscriptRow, patchTranscriptRow, mountTranscript} from './rows.js';
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

/** Own a reusable transcript surface; any trigger can open, close or toggle it. */
export function createMiniTranscript() {
  const element=document.createElement('div');element.className='v2-activity-body';element.hidden=true;
  const owner={querySelector:()=>element};
  let group=null;
  // Refresh the retained model and lazily reconcile rows only while visible.
  function update(value){group=value;if(!element.hidden)updateRows(owner,group);}
  // Opening is presentation only; the caller must already have authorized journal data.
  function open(){element.hidden=false;if(group)updateRows(owner,group);}
  // Keep stable row DOM when temporarily collapsed.
  function close(){element.hidden=true;}
  return {element,update,open,close,
    // Let alternate triggers reuse exactly this transcript surface.
    toggle(){element.hidden?open():close();},
    // Release resize observers and full-view state when the owning session ends.
    dispose(){owner._transcript?.dispose();element.remove();group=null;}};
}
