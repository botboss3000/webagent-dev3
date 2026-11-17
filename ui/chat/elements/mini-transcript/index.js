// Owns modular chat elements.
// Mini transcript owns journal rows, scrolling and disclosure independently of its trigger.
import {createTranscriptRow, patchTranscriptRow, mountTranscript, chipHead} from './rows.js';
// Create inert text nodes for group labels and diagnostic headings.
function textNode(tag, text, className='') {
  const node=document.createElement(tag);node.className=className;node.textContent=text;
  return node;
}
// System rows (compaction, retry, mode change, notices) are never part of a model step:
// each gets its own block styled like a step, titled by its kind.
const SYSTEM_TITLES={compactions:'Compaction',retries:'Retry',modes:'Mode change'};
// A compaction's reason (automatic or requested) joins its block head; other kinds have none.
const systemMeta=row=>row.category==='compactions'?row.label.split(' · ').find(part=>/^(automatic|requested)/.test(part))||'':'';
// Write a system block head (kind, reason, failed or running) and put its one row on the rail.
function paintSystem(step,title,meta,row) {
  const {head,calls:list}=step._parts;head.replaceChildren(textNode('span',title,'tc-step-num'));
  if(meta) head.append(textNode('span',meta,'tc-step-count'));
  const status=row.dataset.status;
  if(status==='error') head.append(textNode('span','failed','tc-step-failed'));
  if(status==='running') head.append(textNode('span','running','tc-step-running'));
  list.setAttribute('aria-label',title);list.hidden=false;step.dataset.kind='system';
  if(step.children.length!==2 || step.children[0]!==head || step.children[1]!==list) step.replaceChildren(head,list);
  if(list.children.length!==1 || list.firstChild!==row) list.replaceChildren(row);
}
// The model turn (agent loop) that wrote a row, as run:turn, or '' when the journal has none.
function rowTurn(row) {
  const event=row.events.find(e=>e.kind!=='tool_result' && e.turn!=null);
  return event?event.run_id+':'+event.turn:'';
}
// One model step = one agent loop: its message row, if any, followed by the tool calls it issued together.
// A message or a new model turn opens a step; tool rows join it until the next message, turn or notice.
function stepContainer(existing,id) {
  let step=existing.get(id);
  if(!step) {
    step=document.createElement('div');step.className='tc-step';step.dataset.rowId=id;
    const head=textNode('div','','tc-step-head');
    const calls=textNode('div','','tc-step-calls');calls.setAttribute('role','group');
    step.append(head,calls);step._parts={head,calls};
  }
  return step;
}
// Write the step head: its number, how many calls it made, and failed or running counts.
function paintStep(step,number,message,calls) {
  const {head,calls:list}=step._parts;head.replaceChildren();
  head.append(textNode('span','Step '+number,'tc-step-num'));
  if(calls.length) head.append(textNode('span',calls.length+(calls.length===1?' tool call':' tool calls'),'tc-step-count'));
  const failed=calls.filter(n=>n.dataset.status==='error').length;
  const running=calls.filter(n=>n.dataset.status==='running').length;
  if(failed) head.append(textNode('span',failed+' failed','tc-step-failed'));
  if(running) head.append(textNode('span',running+' running','tc-step-running'));
  list.setAttribute('aria-label','Step '+number+' tool calls');
  list.hidden=!calls.length;
  step.dataset.status=failed?'error':running?'running':'done';
  const children=[head,...(message?[message]:[]),list];
  if(children.some((child,index)=>step.children[index]!==child) || step.children.length!==children.length) step.replaceChildren(...children);
  if(calls.some((child,index)=>list.children[index]!==child) || list.children.length!==calls.length) list.replaceChildren(...calls);
}
// Reconcile a transcript body from keyed entries: activity rows, or rendered public notice nodes.
function reconcile(node,entries,events) {
  const viewport=node.querySelector('.v2-activity-body');
  if(!node._transcript) node._transcript=mountTranscript(viewport);
  const body=node._transcript.rows;
  const signature=JSON.stringify([events,entries.map(entry=>entry.node?[entry.id,entry.signature]:[entry.id,entry.row.events])]);
  if(body.dataset.signature===signature) return;
  body.dataset.signature=signature;
  // Rows may sit inside step containers, so look them up anywhere in the body.
  const existing=new Map([...body.querySelectorAll('[data-row-id]')].map(n=>[n.dataset.rowId,n]));
  const wanted=[];let step=null,number=0;
  // Close the current step: paint its head and keep it in the transcript.
  const flush=()=>{if(step){paintStep(step.node,step.number,step.message,step.calls);step=null;}};
  for(const entry of entries) {
    let detail=existing.get(entry.id);
    if(entry.node) {
      // A build or background notice keeps its disclosure; the live message row stays hidden.
      if(!detail || detail.dataset.noticeSignature!==entry.signature) {
        detail=document.createElement('div');detail.className='v2-mini-row v2-mini-notice';
        detail.dataset.rowId=entry.id;detail.dataset.noticeSignature=entry.signature;
        detail.append(entry.node.cloneNode(true));
      }
      flush();const block=stepContainer(existing,'system:'+entry.id);
      paintSystem(block,'Notice','',detail);wanted.push(block);continue;
    }
    if(!detail) {detail=createTranscriptRow(entry.row);detail.dataset.rowId=entry.id;}
    patchTranscriptRow(detail,entry.row);
    detail.dataset.category=entry.row.category;
    const category=entry.row.category;
    if(category!=='tools' && category!=='updates' && category!=='responses') {
      flush();const block=stepContainer(existing,'system:'+entry.id);
      paintSystem(block,SYSTEM_TITLES[category]||'System',systemMeta(entry.row),detail);wanted.push(block);continue;
    }
    const turn=rowTurn(entry.row);
    if(category==='tools') {
      // A call with no message before it, or from a later model turn, starts its own step.
      if(step && turn && step.turn && turn!==step.turn) flush();
      if(!step) {number++;step={node:stepContainer(existing,'step:'+entry.id),number,message:null,calls:[],turn};wanted.push(step.node);}
      step.turn||=turn;
      step.calls.push(detail);
    } else {
      flush();number++;
      step={node:stepContainer(existing,'step:'+entry.id),number,message:detail,calls:[],turn};wanted.push(step.node);
    }
  }
  flush();
  // The group's full event log closes the transcript as a system block, styled like a notice.
  let raw=existing.get('raw');
  if(!raw) {
    raw=document.createElement('div');raw.className='v2-mini-row tc-system tc-raw';raw.dataset.rowId='raw';
    const disclosure=document.createElement('details');disclosure.className='v2-mini-tool-disclosure';
    const head=textNode('summary','','tc-row-head');chipHead(head,{icon:'braces',label:'Raw event log'});
    const pre=textNode('pre','','tc-detail');
    disclosure.append(head,pre);raw.append(disclosure);raw._pre=pre;raw._disclosure=disclosure;
    // Populate raw JSON only while the disclosure is open.
    disclosure.addEventListener('toggle',()=>{
      pre.textContent=disclosure.open?JSON.stringify(raw._events,null,2):'';
    });
  }
  raw._events=events;
  if(raw._disclosure.open) raw._pre.textContent=JSON.stringify(events,null,2);
  const diagnostics=stepContainer(existing,'system:raw');
  paintSystem(diagnostics,'Diagnostics',events.length+(events.length===1?' event':' events'),raw);
  wanted.push(diagnostics);
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
