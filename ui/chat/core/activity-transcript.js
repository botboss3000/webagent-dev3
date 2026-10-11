// @code-index {"version":1,"feature":"chat-tool-activity","aliases":["chat activity transcript","tool activity rows"],"purpose":"Locates private activity journal rows rendered inside chat","span_lines":80}
'use strict';

// Presentation only: journal IDs are never treated as legacy interaction IDs.
import {copyText} from '../../shared/models/clipboard.js';
import {_refreshLucideIcons} from './presentation.js';
import {openChatElementFullView, closeChatElementFullView} from './full-view.js';

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
// Project activity events into text, time, arguments/result, and pending/done/error display state.
export function normalizeRow(row) {
  const call=row.events.find(e=>e.kind==='tool_call');
  const result=[...row.events].reverse().find(e=>e.kind==='tool_result');
  return {text:typeof row.text==='string'?row.text:'',
    time:row.events.at(-1)?.created_at,
    args:payloadValue(call?.payload,['arguments','args']),
    result:payloadValue(result?.payload,['result','output',...(result?.payload?.error?['error']:[])]),
    status:result?(result.payload?.error?'error':'done'):'running'};
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
// Construct a tool or message row with stable control nodes; patchTranscriptRow supplies current data.
export function createTranscriptRow(row) {
  const tool=row.category==='tools';
  const node=el('div','','v2-mini-row '+(tool?'v2-mini-tool':'v2-mini-message'));
  const content=el('div','','v2-mini-content');
  const footer=el('div','','turn-gutter ca-activity-entry-footer notice-gutter');
  const raw=diagnostic();
  node._parts={content,footer,raw,tool};
  if(tool) {
    const disclosure=el('details','','v2-mini-tool-disclosure');
    const head=el('summary');disclosure.append(head,content);node.append(disclosure,footer,raw);
    node._parts.head=head;
    // Copy actions read the currently rendered values rather than a stale row snapshot.
    footer.append(copy('Copy arguments and result',()=>content.textContent));
    for(const [key,title] of [['args','Arguments'],['result','Result']]) {
      const section=el('section');const label=el('div',title,'v2-mini-section-head');
      const pre=el('pre');label.append(copy('Copy '+title.toLowerCase(),()=>pre.textContent));
      section.append(label,pre);content.append(section);node._parts[key]={section,pre};
    }
  } else {
    const time=el('time','','v2-mini-time');footer.append(time);node._parts.time=time;
    // Collapse only the message body and keep the transcript controls available.
    footer.append(button('Collapse message','chevron-down',b=>{
      content.hidden=!content.hidden;b.title=content.hidden?'Expand message':'Collapse message';
      b.setAttribute('aria-label',b.title);b.setAttribute('aria-expanded',String(!content.hidden));
    }),
    // Clone visible content for the independent full-view surface.
    button('Open full view','maximize-2',b=>{
      const source=content.cloneNode(true);source.hidden=false;
      openChatElementFullView(source,b,'Full message');node._viewer=true;
    }),copy('Copy text',()=>node._text));
    // Replace any prior speech before reading this row aloud.
    if(window.speechSynthesis && window.SpeechSynthesisUtterance) footer.append(button('Read aloud','volume-2',()=>{
      window.speechSynthesis.cancel();window.speechSynthesis.speak(new window.SpeechSynthesisUtterance(node._text));
    }));
    node.append(content,footer,raw);
  }
  patchTranscriptRow(node,row);_refreshLucideIcons(node);return node;
}
// Skip unchanged event arrays, then update semantic fields and visible content in place.
export function patchTranscriptRow(node,row) {
  const signature=JSON.stringify(row.events);if(node.dataset.signature===signature)return;
  node.dataset.signature=signature;node.dataset.category=row.category;
  const value=normalizeRow(row),p=node._parts;node._text=value.text;
  if(p.tool) {
    p.head.textContent=(value.status==='done'?'✓ ':value.status==='error'?'! ':'◷ ')+row.label;
    node.dataset.status=value.status;
    for(const key of ['args','result']) {
      p[key].section.hidden=value[key]===undefined;
      p[key].pre.textContent=printable(value[key]);
    }
  } else {
    markdown(p.content,value.text || row.label);
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
    if([...rows.children].some(n=>n._viewer))closeChatElementFullView();
  }};
}
