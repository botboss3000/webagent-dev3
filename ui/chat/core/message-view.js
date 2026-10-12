'use strict';
// @code-index {"version":1,"feature":"chat-message-bubbles","aliases":["chat message bubbles","message renderer","message bubble"],"purpose":"Renders persisted V3 user and assistant messages into transcript bubbles Config: data/config/chat.json -> chat_common.message_bubbles, chat_common.message_gutters, chat_common.user_message_truncation. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/message_bubbles","/chat_common/message_gutters","/chat_common/user_message_truncation"],"profile_overrides":true}],"opportunities":[]}}
// Reference chat-bubble DOM and configured gutters, with V3 committed-row transport.
import {applyBubblePaint} from './bubble-paint.js';
import {currentChatProfile} from '../controls/chat-config.js';
import {_refreshLucideIcons} from './presentation.js';
import {copyText} from '../../shared/models/clipboard.js';
import {openChatElementFullView} from './full-view.js';
import {createBubbleGutter} from './bubble-gutters.js';
/** Creates an accessible gutter button and forwards its click to the supplied action. */
function button(title,icon,action){const node=document.createElement('button');node.type='button';node.className='turn-gutter-btn';node.title=title;node.setAttribute('aria-label',title);node.innerHTML='<i data-lucide="'+icon+'" style="width:14px;height:14px"></i>';node.addEventListener('click',action);return node;}
/** Writes plain text as text nodes and recognized HTTP(S) URLs as safe new-tab links. */
function linkify(body,text){let last=0;for(const match of text.matchAll(/https?:\/\/[^\s<>"]+/g)){body.append(document.createTextNode(text.slice(last,match.index)));const a=document.createElement('a');a.textContent=match[0];a.href=match[0];a.target='_blank';a.rel='noopener noreferrer';body.append(a);last=match.index+match[0].length;}body.append(document.createTextNode(text.slice(last)));}
/** Renders sanitized Markdown when the bundled parsers exist, otherwise falls back to linkified text. */
function markdown(body,text){if(window.marked?.parse&&window.DOMPurify?.sanitize){body.className='md-body';body.innerHTML=window.DOMPurify.sanitize(window.marked.parse(text,{gfm:true,breaks:true}),{FORBID_ATTR:['style']});body.querySelectorAll('a[href]').forEach(a=>{a.target='_blank';a.rel='noopener noreferrer';});body.querySelectorAll('pre').forEach(pre=>{const wrap=document.createElement('div');wrap.className='md-code-block';pre.replaceWith(wrap);wrap.append(pre);const copy=button('Copy code','copy',async()=>{try{await copyText(pre.textContent);copy.title='Copied';}catch{copy.title='Copy failed';}});copy.classList.add('md-code-copy');wrap.append(copy);});}else{body.className='bubble-body';linkify(body,text);}}
/** Adds, updates, or removes the live status note that records whether admission review sent a message. */
export function updateAdmission(bubble, admission) {
  let status=bubble.querySelector('.message-admission-status');
  if(!admission){status?.remove();delete bubble.dataset.admissionStatus;return;}
  if(!status){status=document.createElement('p');status.className='ac-hint message-admission-status';status.setAttribute('role','status');status.setAttribute('aria-live','polite');bubble.append(status);}
  bubble.dataset.admissionStatus=admission.status;
  status.textContent=({pending_review:'Awaiting manager review…',reviewing:'Manager is reviewing this message…',
    acceptable:'Accepted by the manager',unacceptable:'Not sent to the agent: this message does not meet its acceptance criteria.',
    review_error:admission.error||'Review failed. This message was not sent to the agent.'})[admission.status]||'';
}
/** Builds one published transcript row; rejects malformed role/sequence/ID data and wires attachments, gutters, and user-text expansion. */
export function createMessage(item,{getRun=()=>null,client,resolveAsset,getSession=()=>null,note=()=>{},showContext=()=>{},historyAction=null}={}){
  if(!['user','assistant','system'].includes(item.role)||!Number.isSafeInteger(item.seq)||item.seq<=0||typeof item.id!=='string')return null;
  if(item.role==='system'){
    const row=document.createElement('article');row.className='v3-message system';row.dataset.publicId=item.id;row.dataset.seq=String(item.seq);
    const text=document.createElement('p');text.textContent=typeof item.content==='string'?item.content:JSON.stringify(item.content);row.append(text);return row;
  }
  const profile=currentChatProfile()||{},role=item.role==='user'?'user':'agent';
  let text=typeof item.content==='string'?item.content:JSON.stringify(item.content);
  const bubble=document.createElement('article');bubble.className='chat-bubble '+role+' published';bubble.dataset.publicId=item.id;bubble.dataset.msgId=item.id;bubble.dataset.seq=String(item.seq);bubble.dataset.turnId=item.run_id;
  const label=document.createElement('span');label.className='label';label.textContent=role==='user'?(item.author?.display_name||item.author?.user_id||item.author_name||item.author_id||item.author_display_name||item.author_user_id||'You'):'Agent';bubble.append(label);
  const body=document.createElement('div');
  if(role==='agent'){const section=document.createElement('div');section.className='turn-section llm-section md';section.dataset.sectionIdx='0';section.dataset.msgId=item.id;markdown(body,text);section.append(body);bubble.append(section);}else{body.className='bubble-body';linkify(body,text);bubble.append(body);}
  if(role==='user'&&Array.isArray(item.attachments)){
    const files=document.createElement('div');files.className='chat-attachments';
    const sid=new URL(location.href).searchParams.get('session_id');
    for(const file of item.attachments){
      if(typeof file.id!=='string'||!/^[0-9a-f-]{36}$/i.test(file.id)||!sid)continue;
      const link=document.createElement('a');link.href='/api/v3/sessions/'+encodeURIComponent(sid)+'/attachments/'+encodeURIComponent(file.id)+'/content';link.textContent=file.name||'Attachment';link.download=file.name||'attachment';link.target='_blank';link.rel='noopener';
      let image;
      if(file.preview===true){image=document.createElement('img');if(!resolveAsset)image.src=link.href;image.alt=file.name||'Image attachment';image.loading='lazy';link.prepend(image);}
      // Resolve attachment content lazily; detached rows are ignored and failed links stay non-navigable.
      if(resolveAsset){const url=link.href;let ready=false;link.onclick=event=>{if(!ready){event.preventDefault();note('Attachment is loading; try again shortly.');}};
        resolveAsset(url).then(blobUrl=>{if(!link.isConnected)return;ready=true;link.href=blobUrl;if(image)image.src=blobUrl;}).catch(error=>{link.onclick=event=>{event.preventDefault();note('Attachment unavailable: '+error.message);};});}
      files.append(link);
    }
    bubble.append(files);
  }
  const cfg=profile.message_bubbles||{},lane=cfg[role]||{};bubble.style.fontSize=(cfg.font_size_px||17)+'px';bubble.style.padding=(cfg.padding_y_px??10)+'px '+(cfg.padding_x_px??14)+'px';if(role==='agent')bubble.style.paddingLeft=(cfg.agent_padding_left_px??4)+'px';applyBubblePaint(bubble,lane);bubble.style.borderRadius=(lane.radius_px??14)+'px';bubble.style.lineHeight=String(lane.line_height||1.5);
  const gutter=createBubbleGutter({item,body,bubble,getText:/* Supplies the current rendered text for gutter copy actions. */()=>text,getRun,showContext,historyAction,refreshMessage:/* Reload the committed row, fenced to its still-selected session. */async()=>{
    const sid=getSession();
    if(!sid||!client)throw Error('Conversation unavailable');
    const snapshot=await client.snapshot(sid);
    if(sid!==getSession()||!bubble.isConnected)return;
    const fresh=snapshot.messages.find(row=>row.id===item.id);
    if(!fresh)throw Error('Message unavailable');
    Object.assign(item,fresh);text=typeof fresh.content==='string'?fresh.content:JSON.stringify(fresh.content);
    body.replaceChildren();if(role==='agent')markdown(body,text);else linkify(body,text);
    note('Message refreshed');
  }});
  bubble.append(gutter);updateAdmission(bubble,item.admission);_refreshLucideIcons(bubble);
  // Measure after layout, then expose expansion only when the configured line clamp hides content.
  if(role==='user')requestAnimationFrame(()=>{
    const cfg=profile.user_message_truncation||{},maxLines=Math.max(1,Math.min(100,Number(cfg.max_lines)||3));if(cfg.enabled===false||!bubble.isConnected)return;const lineHeight=parseFloat(getComputedStyle(body).lineHeight)||20;if(body.scrollHeight<=lineHeight*maxLines+1)return;bubble.style.setProperty('--user-message-max-lines',String(maxLines));bubble.classList.add('user-message-clamped');const toggle=document.createElement('button');toggle.type='button';toggle.className='user-message-expand';toggle.textContent=cfg.expand_label||'See more';toggle.setAttribute('aria-expanded','false');toggle.onclick=()=>{const expanded=bubble.classList.toggle('user-message-expanded');toggle.textContent=expanded?(cfg.collapse_label||'Show less'):(cfg.expand_label||'See more');toggle.setAttribute('aria-expanded',String(expanded));};body.after(toggle);
  });
  return bubble;
}
