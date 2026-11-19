// Owns modular chat elements.
// Activity group owns its compact label and disclosure; mini-transcript owns the body.
import {_refreshLucideIcons} from '../../core/presentation.js';
import {toolFailure} from '../../core/activity.js';
import {lineCount} from '../mini-transcript/rows.js';
import {pushBackHandler} from '../../../shared/history-back/history-back.js';
/** Compact group label: nonzero counts, any extra parts (such as sub-agent notices), then status. */
export function activityLabel(group,{error='',extra=[]}={}) {
    // Summarize nonzero category counts without changing the underlying journal rows.
    const nouns={retries:['retry','retries'],subagents:['sub-agent','sub-agents']};
    // Tools and sub agents lead, whatever order the summary returned them in.
    const leading=['tools','subagents'];
    const entries=[...leading.filter(kind=>group.counts[kind]).map(kind=>[kind,group.counts[kind]]),
      ...Object.entries(group.counts).filter(([kind,count])=>count&&!leading.includes(kind))];
    const parts=entries
      .map(([kind,count])=>{const noun=nouns[kind]||[kind.replace(/s$/,''),kind];
        return count+' '+noun[count===1?0:1];});
    const phase={running:'Working…',queued:'Queued…',waiting_external:'Waiting for build…',finalizing:'Checking response…',
      stopping:'Stopping…',completed:'Completed',failed:'Failed',cancelled:'Stopped',
      finished:'Finished'}[group.state] || group.state;
    // Loaded rows decide; before the group is expanded its server summary says whether it is retrying.
    const last=group.rows?.at(-1);
    const retrying=last?(last.retry?(last.autoResume?'recovering':'retry'):null):group.retrying;
    const progress=group.state==='queued' && retrying ? (retrying==='recovering' ? 'Recovering…' : 'Retrying connection…')
      : group.kind==='compaction' && ['queued','running','finalizing'].includes(group.state)
      ? (group.state==='queued' ? 'Compaction queued…' : 'Compacting…') : phase;
    // Normal completion needs no status suffix; retain active and exceptional states.
    const status=error || (['completed','finished'].includes(group.state)?'':progress);
    return [...parts,...extra,status,
      group.boundary_quality==='legacy_unverified'?'Legacy boundary unverified':''].filter(Boolean).join(' · ');
}
// Tool chips header counts: "8 tool calls, 3 messages", then any other nonzero kinds.
export function chipCounts(counts={}) {
  const plural=(count,one,many=one+'s')=>count+' '+(count===1?one:many);
  const messages=(counts.updates||0)+(counts.responses||0);
  const nouns={subagents:['sub-agent'],retries:['retry','retries'],compactions:['compaction'],modes:['mode change']};
  return [counts.tools?plural(counts.tools,'tool call'):'',messages?plural(messages,'message'):'',
    ...Object.entries(nouns).filter(([kind])=>counts[kind]).map(([kind,noun])=>plural(counts[kind],...noun))]
    .filter(Boolean).join(', ');
}
/** Paint a header's count and status spans, keeping its chevron; extra parts follow the counts. Returns whether it has text. */
/** Model calls and their combined cache share, for a group head: "2 model calls · 99.7% cached". */
export function modelCallsText(counts={},cache={}) {
  const calls=counts.models||0;
  if(!calls)return '';
  const share=cache.input>0?' · '+(Math.floor(1000*(cache.cached||0)/cache.input)/10)+'% cached':'';
  return calls+(calls===1?' model call':' model calls')+share;
}
export function paintActivityHead(summary,group,{error='',extra=[]}={}) {
  const text=[chipCounts(group.counts),modelCallsText(group.counts,group.cache),...extra].filter(Boolean).join(', ')
    ,state=activityLabel({...group,counts:{}},{error});
  const status=summary.querySelector('.tc-head-status'),label=summary.querySelector('.tc-head-label');
  // Write only changed text and tone, so an idle repaint leaves the header untouched.
  const statusText=state?(text?' · ':'')+state:'',tone=error || group.state==='failed'?'danger':'';
  if(label.textContent!==text)label.textContent=text;
  if(status.textContent!==statusText)status.textContent=statusText;
  if(status.dataset.tone!==tone)status.dataset.tone=tone;
  return Boolean(text || state);
}
// Split a text body into display lines, ignoring one trailing newline.
const bodyLines=text=>lineCount(text)?String(text).replace(/\n$/,'').split('\n'):[];
// Estimate each edited file's added and removed lines from successful write/edit call arguments.
// The journal has no real diff, so a write counts its whole content as added.
function fileDiffs(events) {
  const results=new Map(),files=new Map();
  for(const event of events||[])if(event.kind==='tool_result')results.set(event.payload?.tool_call_id,event.payload);
  for(const event of events||[]) {
    const call=event.payload||{};
    if(event.kind!=='tool_call' || !['file_write','file_edit'].includes(call.tool))continue;
    const result=results.get(call.tool_call_id),args=call.arguments||call.args||{};
    if(!result || toolFailure(result)!==null || typeof args.path!=='string')continue;
    const file=files.get(args.path)||{path:args.path,added:0,removed:0,lines:[]};
    const removed=call.tool==='file_edit'?bodyLines(args.old_text):[];
    const added=bodyLines(call.tool==='file_edit'?args.new_text:args.content);
    file.removed+=removed.length;file.added+=added.length;
    file.lines.push(...removed.map(line=>['-',line]),...added.map(line=>['+',line]));
    files.set(args.path,file);
  }
  // Chips show the file name, plus its folder when two edited files share a name.
  const list=[...files.values()],base=path=>path.split('/').at(-1);
  for(const file of list)file.name=list.filter(other=>base(other.path)===base(file.path)).length>1
    ?file.path.split('/').slice(-2).join('/'):base(file.path);
  return list;
}
// One body-mounted diff popover is shared by every chip; it is fixed, clamped and flips above.
let pop=null,popAnchor=null,hideTimer=0;
const MAX_POP_LINES=14;
function hidePop() {
  clearTimeout(hideTimer);if(pop)pop.hidden=true;
  popAnchor?.setAttribute('aria-expanded','false');popAnchor?.removeAttribute('aria-describedby');popAnchor=null;
}
const scheduleHide=()=>{clearTimeout(hideTimer);hideTimer=setTimeout(hidePop,140);};
function showPop(chip,file) {
  if(!pop) {
    pop=document.createElement('div');pop.className='tc-diff-pop';pop.id='tc-diff-pop';pop.setAttribute('role','tooltip');
    pop.addEventListener('pointerenter',()=>clearTimeout(hideTimer));
    pop.addEventListener('pointerleave',event=>{if(event.pointerType==='mouse')scheduleHide();});
    document.addEventListener('pointerdown',event=>{if(popAnchor && !pop.contains(event.target) && !popAnchor.contains(event.target))hidePop();},true);
    document.addEventListener('keydown',event=>{if(event.key==='Escape' && popAnchor){const anchor=popAnchor;hidePop();anchor.focus();}});
    window.addEventListener('resize',hidePop);
    document.addEventListener('scroll',event=>{if(popAnchor && !pop.contains(event.target))hidePop();},true);
    document.body.append(pop);
  }
  clearTimeout(hideTimer);
  if(popAnchor && popAnchor!==chip)popAnchor.setAttribute('aria-expanded','false');
  const head=document.createElement('div');head.className='tc-pop-head';
  const path=document.createElement('span');path.className='tc-pop-path';path.textContent=file.path;
  const add=document.createElement('span');add.className='tc-add';add.textContent='+'+file.added;
  const del=document.createElement('span');del.className='tc-del';del.textContent='−'+file.removed;
  head.append(path,add,del);
  const lines=document.createElement('div');lines.className='tc-pop-lines';
  for(const [sign,text] of file.lines.slice(0,MAX_POP_LINES)) {
    const line=document.createElement('div');line.className='tc-line '+(sign==='+'?'tc-line-add':'tc-line-del');
    const mark=document.createElement('span');mark.className='tc-sign';mark.textContent=sign==='+'?'+':'−';
    line.append(mark,document.createTextNode(text||' '));lines.append(line);
  }
  if(file.lines.length>MAX_POP_LINES) {
    const more=document.createElement('div');more.className='tc-line-more';
    more.textContent='… '+(file.lines.length-MAX_POP_LINES)+' more lines';lines.append(more);
  }
  pop.replaceChildren(head,lines);pop.hidden=false;
  popAnchor=chip;chip.setAttribute('aria-expanded','true');chip.setAttribute('aria-describedby',pop.id);
  const box=chip.getBoundingClientRect(),width=pop.offsetWidth,height=pop.offsetHeight;
  let top=box.bottom+6;
  if(top+height>innerHeight-8 && box.top-6-height>=8)top=box.top-6-height;
  pop.style.left=Math.max(8,Math.min(box.left,innerWidth-width-8))+'px';
  pop.style.top=Math.max(8,top)+'px';
}
// Build one file-diff chip: name, green added and red removed counts; hover, focus or tap opens its diff.
function diffChip(file) {
  const chip=document.createElement('button');chip.type='button';chip.className='tc-diff';
  chip.setAttribute('aria-expanded','false');
  chip.setAttribute('aria-label',file.path+': '+file.added+' lines added, '+file.removed+' removed (estimated)');
  chip.innerHTML='<span class="tc-diff-icon" aria-hidden="true"><i data-lucide="file-diff"></i></span>';
  const name=document.createElement('span');name.className='tc-diff-name';name.textContent=file.name;
  const add=document.createElement('span');add.className='tc-add';add.textContent='+'+file.added;
  const del=document.createElement('span');del.className='tc-del';del.textContent='−'+file.removed;
  chip.append(name,add,del);
  chip.addEventListener('pointerenter',event=>{if(event.pointerType==='mouse')showPop(chip,file);});
  chip.addEventListener('pointerleave',event=>{if(event.pointerType==='mouse')scheduleHide();});
  chip.addEventListener('focus',()=>{if(chip.matches(':focus-visible'))showPop(chip,file);});
  chip.addEventListener('blur',()=>{if(popAnchor===chip)scheduleHide();});
  // Mouse users already see it on hover; touch and keyboard activation toggle it.
  chip.addEventListener('click',event=>{
    if(event.pointerType==='mouse')return;
    if(popAnchor===chip)hidePop();else showPop(chip,file);
  });
  return chip;
}
const MAX_DIFF_CHIPS=3;
// Redraw the diff chip strip only when the estimated files change or "+N more" is toggled.
function renderDiffs(strip,events) {
  const files=fileDiffs(events);
  const signature=JSON.stringify(files.map(file=>[file.path,file.added,file.removed]))+':'+!!strip._expanded;
  if(strip._signature===signature)return;
  strip._signature=signature;strip.hidden=!files.length;
  if(popAnchor && strip.contains(popAnchor))hidePop();
  const label=document.createElement('span');label.className='tc-diffs-label';
  label.textContent='Edited '+files.length+' file'+(files.length===1?'':'s');
  const shown=strip._expanded?files:files.slice(0,MAX_DIFF_CHIPS);
  const children=[label,...shown.map(diffChip)];
  if(files.length>MAX_DIFF_CHIPS) {
    const more=document.createElement('button');more.type='button';more.className='tc-more';
    more.textContent=strip._expanded?'Show less':'+'+(files.length-MAX_DIFF_CHIPS)+' more';
    more.setAttribute('aria-expanded',String(!!strip._expanded));
    more.addEventListener('click',()=>{strip._expanded=!strip._expanded;strip._signature='';renderDiffs(strip,strip._events);
      strip.querySelector('.tc-more')?.focus();});
    children.push(more);
  }
  strip.replaceChildren(...children);_refreshLucideIcons(strip);
}
/** Connect a persistent activity disclosure to an independently owned transcript. */
export function createActivityGroup({transcript}) {
  const node=document.createElement('details');node.className='v2-activity-group tc-group';
  const summary=document.createElement('summary');summary.className='tc-head';
  const chevron=document.createElement('span');chevron.className='tc-head-chev';chevron.setAttribute('aria-hidden','true');
  chevron.innerHTML='<i data-lucide="chevron-right"></i>';
  const counts=document.createElement('span');counts.className='tc-head-label';
  const status=document.createElement('span');status.className='tc-head-status';
  summary.append(chevron,counts,status);_refreshLucideIcons(summary);
  const diffs=document.createElement('div');diffs.className='tc-diffs';diffs.hidden=true;
  node.append(summary,transcript.element,diffs);
  // Native details supplies pointer and keyboard disclosure semantics.
  // Every open layer owns its own history entry, so each press pops a real entry: a tool
  // call first, then the group. Nothing is pushed while a press is handled, because
  // Android Chrome ignores a push made inside the popstate it is already committing.
  let groupBack=null;
  const toolBacks=new Map();
  const claimTool=row=>{
    if(toolBacks.has(row))return;
    toolBacks.set(row,pushBackHandler(()=>{
      // The press consumed this entry, so closing the row must not drop it again.
      toolBacks.delete(row);
      if(!row.isConnected||!row.open)return false;
      row.open=false;
      return true;
    }));
  };
  const releaseTool=row=>{
    const release=toolBacks.get(row);
    if(!release)return;
    toolBacks.delete(row);
    release();
  };
  // Each call claims the press while it is open and drops its entry when it closes.
  node.addEventListener('toggle',event=>{
    const row=event.target;
    if(!row.matches?.('.v2-mini-tool-disclosure'))return;
    if(row.open)claimTool(row);else releaseTool(row);
  },true);
  const closeGroup=()=>{
    groupBack=null;
    if(!node.isConnected)return false;
    node.open=false;
    return true;
  };
  const toggle=()=>{
    if(node.open){
      if(!groupBack)groupBack=pushBackHandler(closeGroup);
      transcript.open();
      return;
    }
    // A call left open inside a closing group must not keep its entry.
    for(const row of [...toolBacks.keys()])releaseTool(row);
    const release=groupBack;groupBack=null;release?.();
    transcript.close();popAnchor && diffs.contains(popAnchor) && hidePop();
  };
  node.addEventListener('toggle',toggle);
  // Refresh counts/status without replacing the focused summary or open transcript.
  function update(group,{error='',active=false}={}) {
    if(node.dataset.activityGroupId!==group.id)node.dataset.activityGroupId=group.id;
    // A finished group with no rows and no status has nothing to disclose, such as a superseded stop.
    const hidden=!paintActivityHead(summary,group,{error});if(node.hidden!==hidden)node.hidden=hidden;
    if(node.getAttribute('aria-label')!=='Activity group')node.setAttribute('aria-label','Activity group');
    node.classList.toggle('v2-activity-group-glimmer',active);
    transcript.update(group);
    diffs._events=group.events;renderDiffs(diffs,group.events);
  }
  return {element:node,update,
    // Remove the listener and all transcript-owned resources on session teardown.
    // Teardown drops every claim this group still holds, so the next session starts clean.
    dispose(){for(const row of [...toolBacks.keys()])releaseTool(row);const release=groupBack;groupBack=null;release?.();if(popAnchor && diffs.contains(popAnchor))hidePop();node.removeEventListener('toggle',toggle);transcript.dispose();node.remove();}};
}
