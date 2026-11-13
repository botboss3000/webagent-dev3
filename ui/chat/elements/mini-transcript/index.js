// Owns modular chat elements.
// Mini transcript owns journal rows, scrolling and disclosure independently of its trigger.
import {createTranscriptRow, patchTranscriptRow, mountTranscript} from './rows.js';
// Create inert text nodes for group labels and diagnostic headings.
function textNode(tag, text, className='') {
  const node=document.createElement(tag);node.className=className;node.textContent=text;
  return node;
}
// Reconcile a transcript body from keyed entries: activity rows, or rendered public notice nodes.
function reconcile(node,entries,events) {
  const viewport=node.querySelector('.v2-activity-body');
  if(!node._transcript) node._transcript=mountTranscript(viewport);
  const body=node._transcript.rows;
  const signature=JSON.stringify([events,entries.map(entry=>entry.node?[entry.id,entry.signature]:[entry.id,entry.row.events])]);
  if(body.dataset.signature===signature) return;
  body.dataset.signature=signature;
  const existing=new Map([...body.children].map(n=>[n.dataset.rowId,n]));
  const wanted=[];
  for(const entry of entries) {
    let detail=existing.get(entry.id);
    if(entry.node) {
      // A build or background notice keeps its disclosure; the live message row stays hidden.
      if(!detail || detail.dataset.noticeSignature!==entry.signature) {
        detail=document.createElement('div');detail.className='v2-mini-row v2-mini-notice';
        detail.dataset.rowId=entry.id;detail.dataset.noticeSignature=entry.signature;
        detail.append(entry.node.cloneNode(true));
      }
    } else {
      if(!detail) {detail=createTranscriptRow(entry.row);detail.dataset.rowId=entry.id;}
      patchTranscriptRow(detail,entry.row);
      detail.dataset.category=entry.row.category;
    }
    wanted.push(detail);
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
  raw._events=events;
  raw.firstElementChild.textContent='All '+events.length+' diagnostic events';
  if(raw.open) raw.lastElementChild.textContent=JSON.stringify(events,null,2);
  wanted.push(raw);
  for(const child of [...body.children]) if(!wanted.includes(child)) child.remove();
  let before=body.firstChild;
  for(const child of wanted) {
    if(child!==before) body.insertBefore(child,before);
    before=child.nextSibling;
  }
  node._transcript.update();
}

// One group's own transcript model; its rows render through the shared reconciler.
function updateRows(node,group) {
  reconcile(node,group.rows.map(row=>({id:row.id,row})),group.events);
}

/** Own a reusable transcript surface; any trigger can open, close or toggle it. */
export function createMiniTranscript() {
  const element=document.createElement('div');element.className='v2-activity-body';element.hidden=true;
  const owner={querySelector:()=>element};
  let group=null,merged=null;
  // Refresh the retained model and lazily reconcile rows only while visible.
  function update(value){group=value;merged=null;if(!element.hidden)updateRows(owner,group);}
  // Merge several owners' activity rows and notice lines into this one transcript.
  function render(entries,events){group=null;merged={entries,events};if(!element.hidden)reconcile(owner,entries,events);}
  // Opening is presentation only; the caller must already have authorized journal data.
  function open(){element.hidden=false;if(group)updateRows(owner,group);else if(merged)reconcile(owner,merged.entries,merged.events);}
  // Keep stable row DOM when temporarily collapsed.
  function close(){element.hidden=true;}
  return {element,update,render,open,close,
    // Let alternate triggers reuse exactly this transcript surface.
    toggle(){element.hidden?open():close();},
    // Release resize observers and full-view state when the owning session ends.
    dispose(){owner._transcript?.dispose();element.remove();group=null;merged=null;}};
}
