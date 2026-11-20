// Locates private activity journal rows rendered inside chat Config: config/ui/chat/chat.json -> chat_common.show_activity_groups.
'use strict';

// Presentation only: journal IDs are never treated as legacy interaction IDs.
import {copyText} from '../../../shared/models/clipboard.js';
import {_refreshLucideIcons} from '../../core/presentation.js';
import {openChatElementFullView, closeChatElementFullView} from '../../core/full-view.js';
import {toolFailure,toolHeld} from '../../core/activity.js';

// Build display elements with textContent so labels stay inert.
function el(tag, text='', cls='') {
  const node=document.createElement(tag);node.className=cls;node.textContent=text;return node;
}
// Create an accessible icon action and stop row-level bubbling before invoking its handler.
function button(title, icon, action) {
  const node=el('button','','turn-gutter-btn');node.type='button';
  node.title=title;node.setAttribute('aria-label',title);
  node.innerHTML='<i data-lucide="'+icon+'" style="width:14px;height:14px"></i>';
  node.addEventListener('click',event=>{event.stopPropagation();return action(node);});return node;
}
// Copy a lazily read value and expose success or failure through the button label.
function copy(title, value) {
  return button(title,'copy',async node=>{
    try {await copyText(value());node.title='Copied!';node.setAttribute('aria-label','Copied!');}
    catch (_) {node.title='Copy failed';node.setAttribute('aria-label','Copy failed');}
  });
}
// Read the first present own payload field, preserving explicit null and false values.
function payloadValue(payload,keys) {
  for(const key of keys) if(payload && Object.hasOwn(payload,key))return payload[key];
}
// Format structured tool values for preformatted display; undefined becomes empty text.
const printable=value=>typeof value==='string'?value:JSON.stringify(value,null,2)??'';
/** Lines in a text body, ignoring one trailing newline; empty text has none. */
export const lineCount=text=>text?String(text).replace(/\n$/,'').split('\n').length:0;
// Project activity events into text, time, arguments/result, and pending/done/error display state.
export function normalizeRow(row) {
  const call=row.events.find(e=>e.kind==='tool_call');
  const result=[...row.events].reverse().find(e=>e.kind==='tool_result');
  return {text:typeof row.text==='string'?row.text:'',
    time:row.events.at(-1)?.created_at,
    args:payloadValue(call?.payload,['arguments','args']),
    result:payloadValue(result?.payload,['result','output',...(result?.payload?.error?['error']:[])]),
    status:!result?'running':toolHeld(result.payload)?'held':toolFailure(result.payload)!==null?'error':'done'};
}
// Read bounded broker metadata through the tool transport envelope; source text is never searched.
export function brokerDetails(result) {
  let value=result;
  for(let depth=0;depth<4;depth++) {
    if(typeof value==='string') {
      if(value.length>1024*1024)return null;
      try {value=JSON.parse(value);}catch(_){return null;}
    }
    if(!value || typeof value!=='object' || Array.isArray(value))return null;
    const context=value.broker_context;
    if(context && typeof context==='object' && typeof context.broker_id==='string') {
      const text=(entry,max=200)=>typeof entry==='string' && entry.length<=max?entry:null;
      const id=text(context.broker_id,80);if(!id || !/^[A-Za-z0-9_-]{8,80}$/.test(id))return null;
      return [['Broker ID',id],['Originating chat',text(context.provenance?.session_id,36)],
        ['Work',text(context.summary)],['Selection',text(context.selection,32)],['State',text(context.state,32)],
        ['Broker Job ID',text(value.broker_job_id??context.broker_job_id,80)],
        ['Broker Release ID',text(value.broker_release_id??context.broker_release_id,80)]].filter(([,entry])=>entry!=null);
    }
    value=value.output??value.result;
  }
  return null;
}
// Use the available parser and sanitizer, falling back to literal text if either fails.
function markdown(body,text) {
  body.replaceChildren();
  if(window.marked?.parse && window.DOMPurify?.sanitize) {
    try {
      body.innerHTML=window.DOMPurify.sanitize(window.marked.parse(text,{gfm:true,breaks:true}),{FORBID_ATTR:['style']});
      // Keep parsed links in a new browsing context without granting opener access.
      body.querySelectorAll('a[href]').forEach(a=>{a.target='_blank';a.rel='noopener noreferrer';});return;
    } catch (_) { /* Plain text is the safe fallback. */ }
  }
  body.textContent=text;
}
// Keep complete event data available in a collapsed, read-only diagnostics disclosure.
function diagnostic() {
  const node=el('details','','v2-mini-diagnostics');node.append(el('summary','Raw diagnostics'),el('pre'));return node;
}
// Tool chips: each tool has an icon and a verb; the inline chip names its target.
const TOOL_VIEWS={file_read:['file-text','Read'],file_write:['file-plus','Write'],file_edit:['pencil','Edit'],
  file_list:['folder','List'],file_search:['search','Search'],browser_read:['globe','Read page'],
  browser_input:['mouse-pointer-click','Use page'],start_spawn:['bot','Start sub-agent']};
// A short chip text: a path's file name or a URL's host and path; the full value stays in the title.
function chipText(target) {
  if(typeof target!=='string' || !target)return '';
  if(/^https?:\/\//.test(target)) {try {const url=new URL(target);return url.host+url.pathname.replace(/\/$/,'');}catch(_){return target;}}
  return target.split('/').filter(Boolean).at(-1) || target;
}
// Draw one compact row head: icon (a chevron on hover or open), label, optional chip, preview and note.
export function chipHead(head,{icon,label,chip='',title='',preview='',note=''}) {
  head.replaceChildren();
  const slot=el('span','','tc-icon');slot.setAttribute('aria-hidden','true');
  slot.innerHTML='<span class="tc-glyph"><i data-lucide="'+icon+'"></i></span><span class="tc-chev"><i data-lucide="chevron-right"></i></span>';
  head.append(slot,el('span',label,'tc-label'));
  if(chip){const node=el('span',chip,'tc-chip');node.title=title||chip;head.append(node);}
  if(preview)head.append(el('span',preview,'tc-preview'));
  if(note)head.append(el('span',note,'tc-note'));
  _refreshLucideIcons(head);
}
// Construct a tool or message row with stable control nodes; patchTranscriptRow supplies current data.
export function createTranscriptRow(row) {
  const tool=row.category==='tools',compaction=row.compaction===true,retry=row.retry===true;
  const closer=row.closer===true,modelCall=row.modelCall===true;
  const node=el('div','','v2-mini-row '+(tool?'v2-mini-tool tc-tool':compaction?'v2-mini-tool v2-mini-compaction tc-system'
    :closer?'v2-mini-tool v2-mini-closer tc-system':retry?'v2-mini-tool v2-mini-retry tc-system'
    :modelCall?'v2-mini-tool v2-mini-model tc-system':'v2-mini-message tc-message'));
  const content=el('div','','v2-mini-content');
  const footer=el('div','','turn-gutter ca-activity-entry-footer notice-gutter');
  const raw=diagnostic();
  node._parts={content,footer,raw,tool,compaction,retry,closer,modelCall};
  if(retry || compaction || closer || modelCall) {
    // A system row (provider retry, compaction summary, closing status, model call) is a compact row like a tool call;
    // expanding shows its detail behind the same left border.
    const disclosure=el('details','','v2-mini-tool-disclosure');
    const head=el('summary','','tc-row-head');const detail=el('div','','tc-detail');
    detail.append(content,footer,raw);disclosure.append(head,detail);node.append(disclosure);
    node._parts.head=head;
    footer.append(copy(retry?'Copy error':closer?'Copy status':modelCall?'Copy details':'Copy summary',()=>node._text));
  } else if(tool) {
    // One compact row; its detail (arguments, result, copy, diagnostics) sits behind a left border.
    const disclosure=el('details','','v2-mini-tool-disclosure');
    const head=el('summary','','tc-row-head');content.classList.add('tc-detail');
    disclosure.append(head,content);node.append(disclosure);
    node._parts.head=head;
    const broker=el('section','','v2-mini-broker');broker.hidden=true;
    broker.append(el('div','Broker work','v2-mini-section-head'),el('pre'));
    content.append(broker);node._parts.broker=broker;
    // Copy actions read the currently rendered values rather than a stale row snapshot.
    footer.append(copy('Copy arguments and result',()=>['args','result'].map(key=>node._parts[key].pre.textContent).filter(Boolean).join('\n\n')));
    for(const [key,title] of [['args','Arguments'],['result','Result']]) {
      const section=el('section');const label=el('div',title,'v2-mini-section-head');
      const pre=el('pre');label.append(copy('Copy '+title.toLowerCase(),()=>pre.textContent));
      section.append(label,pre);content.append(section);node._parts[key]={section,pre};
    }
    content.append(footer,raw);
  } else {
    // A message is a compact "Thinking" row; its text and actions open behind the same left border.
    const disclosure=el('details','','v2-mini-tool-disclosure');
    const head=el('summary','','tc-row-head');const detail=el('div','','tc-detail');
    detail.append(content,footer,raw);disclosure.append(head,detail);node.append(disclosure);node._parts.head=head;
    const time=el('time','','v2-mini-time');footer.append(time);node._parts.time=time;
    // Clone visible content for the independent full-view surface.
    footer.append(button('Open full view','maximize-2',b=>{
      const source=content.cloneNode(true);source.hidden=false;
      openChatElementFullView(source,b,'Full message');node._viewer=true;
    }),copy('Copy text',()=>node._text));
    // Replace any prior speech before reading this row aloud.
    if(window.speechSynthesis && window.SpeechSynthesisUtterance) footer.append(button('Read aloud','volume-2',()=>{
      window.speechSynthesis.cancel();window.speechSynthesis.speak(new window.SpeechSynthesisUtterance(node._text));
    }));
  }
  patchTranscriptRow(node,row);_refreshLucideIcons(node);return node;
}
// Skip unchanged event arrays, then update semantic fields and visible content in place.
export function patchTranscriptRow(node,row) {
  const signature=JSON.stringify(row.events);if(node.dataset.signature===signature)return;
  node.dataset.signature=signature;node.dataset.category=row.category;
  const value=normalizeRow(row),p=node._parts;node._text=value.text;
  if(p.retry) {
    chipHead(p.head,{icon:'rotate-ccw',label:row.label});node.dataset.status='retry';
    markdown(p.content,row.text || 'No provider detail.');
  } else if(p.modelCall) {
    chipHead(p.head,{icon:'cpu',label:row.label});node.dataset.status='done';
    markdown(p.content,row.text || row.label);
  } else if(p.closer) {
    chipHead(p.head,{icon:'flag',label:row.label,chip:row.model,note:row.failed?'failed':''});
    node.dataset.status=row.failed?'error':'done';
    markdown(p.content,row.text || row.label);
  } else if(p.compaction) {
    const result=row.events.find(e=>e.kind==='compaction_result');
    const status=!result?'running':result.payload?.error?'error':'done';
    // The step head names the compaction and its reason; the row shows the size change and the model.
    const parts=row.label.split(' · ').slice(1).filter(part=>!/^(automatic|requested)/.test(part));
    const model=parts.find(part=>part.startsWith('System model '));
    const size=parts.filter(part=>part!==model).join(' · ');
    chipHead(p.head,{icon:'shrink',label:size || 'Summary',chip:model?model.slice(13):'',
      note:{error:'failed',running:'running'}[status]||''});
    p.head.title=row.label;node.dataset.status=status;
    markdown(p.content,value.text || 'Waiting for the summary…');
  } else if(p.tool) {
    const name=row.events.find(e=>e.kind==='tool_call')?.payload?.tool || row.label.split(' · ')[0];
    const args=value.args && typeof value.args==='object'?value.args:{};
    const [icon,verb]=TOOL_VIEWS[name]||['wrench',String(name).replace(/_/g,' ')];
    const target=[args.path,args.url,args.query,args.selector].find(entry=>typeof entry==='string' && entry);
    const label=name==='file_write' && typeof args.content==='string'?'Write '+lineCount(args.content)+(lineCount(args.content)===1?' line':' lines'):verb;
    chipHead(p.head,{icon,label,chip:chipText(target),title:target,
      note:{error:'failed',held:'held'}[value.status]||''});
    p.head.title=row.label;
    node.dataset.status=value.status;
    const details=brokerDetails(value.result);p.broker.hidden=!details;
    p.broker.lastElementChild.textContent=details?details.map(([label,text])=>label+': '+text).join('\n'):'';
    for(const key of ['args','result']) {
      p[key].section.hidden=value[key]===undefined;
      p[key].pre.textContent=printable(value[key]);
    }
  } else {
    markdown(p.content,value.text || row.label);
    const preview=(value.text || '').replace(/\s+/g,' ').trim();
    // Agent messages read Thinking or Response; any other row (a mode change, a bare compaction notice) keeps its label.
    const [icon,label]={updates:['brain','Thinking'],responses:['message-square','Response']}[row.category]||['info',row.label];
    chipHead(p.head,{icon,label,preview});
    const date=value.time?new Date(value.time):null;
    p.time.textContent=date && Number.isFinite(date.getTime())?date.toLocaleTimeString([], {hour:'2-digit',minute:'2-digit'}):'';
    if(date && Number.isFinite(date.getTime()))p.time.dateTime=date.toISOString();
  }
  p.raw.lastElementChild.textContent=JSON.stringify(row.events,null,2);
}
// Add follow/oldest navigation and return row access plus this mount's cleanup operations.
export function mountTranscript(body) {
  const nav=el('div','','v2-mini-nav');const rows=el('div','','v2-mini-rows');
  let following=true,disposed=false;
  // Toggle following and jump to the tail when following is enabled.
  const latest=button('Follow latest','arrow-down',()=>{following=!following;if(following)body.scrollTop=body.scrollHeight;refresh();});
  // Disable following and move to the transcript start.
  nav.append(button('Jump to oldest','arrow-up',()=>{following=false;body.scrollTop=0;refresh();}),latest);
  body.append(nav,rows);_refreshLucideIcons(nav);
  // Recompute navigation visibility and the follow button's pressed state.
  function refresh() {
    if(disposed)return;nav.hidden=body.scrollHeight<=body.clientHeight+1;
    latest.classList.toggle('is-following',following);latest.setAttribute('aria-pressed',String(following));
  }
  // Scrolling away from the tail disables automatic following until the user opts back in.
  const scroll=()=>{if(body.scrollTop+body.clientHeight<body.scrollHeight-12)following=false;refresh();};
  body.addEventListener('scroll',scroll,{passive:true});
  // Keep the tail pinned when row or viewport size changes, if ResizeObserver is available.
  const observer=typeof ResizeObserver==='function'?new ResizeObserver(()=>{if(following)body.scrollTop=body.scrollHeight;refresh();}):null;
  observer?.observe(rows);observer?.observe(body);
  // Keep the tail visible when requested, then refresh the navigation affordance.
  return {rows,
    update(){if(following)body.scrollTop=body.scrollHeight;refresh();},
    // Remove this mount's scroll observer and close a full view opened from one of its rows.
    dispose(){
    disposed=true;observer?.disconnect();body.removeEventListener('scroll',scroll);
    if([...rows.querySelectorAll('.v2-mini-row')].some(n=>n._viewer))closeChatElementFullView();
  }};
}
