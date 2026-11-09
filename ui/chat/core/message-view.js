'use strict';
// @code-index {"version":1,"feature":"chat-message-bubbles","aliases":["chat message bubbles","message renderer","message bubble"],"purpose":"Renders persisted V3 user and assistant messages into transcript bubbles Config: ui/chat/chat.json -> chat_common.message_bubbles, chat_common.message_gutters, chat_common.user_message_truncation. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common/message_bubbles","/chat_common/message_gutters","/chat_common/user_message_truncation"],"profile_overrides":true}],"opportunities":[]}}
// Reference chat-bubble DOM and configured gutters, with V3 committed-row transport.
import {applyBubblePaint} from './bubble-paint.js';
import {currentChatProfile} from '../controls/chat-config.js';
import {_refreshLucideIcons} from './presentation.js';
import {copyText} from '../../shared/models/clipboard.js';
import {openChatElementFullView} from './full-view.js';
import {openChatImagePreview} from './image-preview.js';
import {createBubbleGutter} from './bubble-gutters.js';
import {createChatAction} from './chat-action-panel.js';
/** Creates an accessible gutter button and forwards its click to the supplied action. */
function button(title,icon,action){const node=document.createElement('button');node.type='button';node.className='turn-gutter-btn';node.title=title;node.setAttribute('aria-label',title);node.innerHTML='<i data-lucide="'+icon+'" style="width:14px;height:14px"></i>';node.addEventListener('click',action);return node;}
/** Writes plain text as text nodes and recognized HTTP(S) URLs as safe new-tab links. */
function linkify(body,text){let last=0;for(const match of text.matchAll(/https?:\/\/[^\s<>"]+/g)){body.append(document.createTextNode(text.slice(last,match.index)));const a=document.createElement('a');a.textContent=match[0];a.href=match[0];a.target='_blank';a.rel='noopener noreferrer';body.append(a);last=match.index+match[0].length;}body.append(document.createTextNode(text.slice(last)));}
/** Renders sanitized Markdown when the bundled parsers exist, otherwise falls back to linkified text. */
// A link to one of this app's Wiki articles; '' for any other link.
function wikiSlug(href){try{const url=new URL(href,location.href);const match=url.origin===location.origin&&url.pathname.match(/^\/wiki\/([a-z0-9-]{1,80})$/);return match?match[1]:'';}catch{return '';}}
// A plain click on a Wiki link asks the surface (Wiki tab or embed reader) to open it; unhandled, it stays a new-tab link.
document.addEventListener('click',event=>{const link=event.target.closest?.('a[data-chat-wiki]');if(!link||event.button!==0||event.metaKey||event.ctrlKey||event.shiftKey||event.altKey)return;const open=new CustomEvent('webagent:wiki-open',{detail:{slug:link.dataset.chatWiki},cancelable:true});document.dispatchEvent(open);if(open.defaultPrevented)event.preventDefault();});
function markdown(body,text){if(window.marked?.parse&&window.DOMPurify?.sanitize){body.className='md-body';body.innerHTML=window.DOMPurify.sanitize(window.marked.parse(text,{gfm:true,breaks:true}),{FORBID_ATTR:['style']});body.querySelectorAll('a[href]').forEach(a=>{a.target='_blank';a.rel='noopener noreferrer';const slug=wikiSlug(a.href);if(slug)a.dataset.chatWiki=slug;});body.querySelectorAll('pre').forEach(pre=>{const wrap=document.createElement('div');wrap.className='md-code-block';pre.replaceWith(wrap);wrap.append(pre);const copy=button('Copy code','copy',async()=>{try{await copyText(pre.textContent);copy.title='Copied';}catch{copy.title='Copy failed';}});copy.classList.add('md-code-copy');wrap.append(copy);});}else{body.className='bubble-body';linkify(body,text);}}
/** Adds, updates, or removes the live status note that records whether admission review sent a message. */
export function updateAdmission(bubble, admission) {
  // Panel updates target the row; admission feedback remains part of its message surface.
  bubble=bubble.querySelector('.chat-bubble')||bubble;
  let status=bubble.querySelector('.message-admission-status');
  if(!admission){status?.remove();delete bubble.dataset.admissionStatus;return;}
  if(!status){status=document.createElement('p');status.className='ac-hint message-admission-status';status.setAttribute('role','status');status.setAttribute('aria-live','polite');bubble.append(status);}
  bubble.dataset.admissionStatus=admission.status;
  // A message the agent never received gets a prominent notice, not a quiet hint.
  status.classList.toggle('is-blocked',['unacceptable','review_error'].includes(admission.status));
  status.textContent=({pending_review:'Awaiting manager review…',reviewing:'Manager is reviewing this message…',
    acceptable:'Accepted by the manager',unacceptable:'Not sent: this is outside the topics this assistant can help with. Try asking about its services instead.',
    review_error:admission.error||'Review failed. This message was not sent to the agent.'})[admission.status]||'';
}
/** Builds one published bubble; rejects malformed identity data and wires attachments, actions and user-text expansion. */
export function createMessage(item,{getRun=()=>null,client,resolveAsset,getSession=()=>null,note=()=>{},showContext=()=>{},historyAction=null}={}){
  if(!['user','assistant','system','notice'].includes(item.role)||!Number.isSafeInteger(item.seq)||item.seq<=0||typeof item.id!=='string')return null;
  // A stored run-status notice keeps its transcript position; the agent never sees these rows.
  if(item.role==='notice'){
    const notice=item.content&&typeof item.content==='object'?item.content:{};
    const row=document.createElement('article');row.className='v3-message system';row.dataset.publicId=item.id;row.dataset.seq=String(item.seq);row.dataset.noticeRunId=item.run_id;
    row.append(createChatAction({title:(notice.status==='failed'?'Run failed':'Stopped')+(notice.error?' — '+notice.error:''),message:'To continue, write a new message in the chat.'}));return row;
  }
  // Sub-agent completion notices show as compact activity rows, not user bubbles; the agent's context is unchanged.
  const notices=item.message_kind==='agent_message'&&Array.isArray(item.platform_notice?.notices)?item.platform_notice.notices.slice(0,20):null;
  if(notices?.length){
    const row=document.createElement('article');row.className='v3-message v3-subagent-notices';row.dataset.publicId=item.id;row.dataset.msgId=item.id;row.dataset.seq=String(item.seq);row.dataset.turnId=item.run_id;row.dataset.subagentNotices=String(notices.length);
    for(const notice of notices){
      const entry=document.createElement('details');entry.className='v3-subagent-row';
      const summary=document.createElement('summary');const icon=document.createElement('i');icon.dataset.lucide='git-branch';icon.setAttribute('aria-hidden','true');
      const state=notice.status==='completed'?'finished':notice.status==='waiting_approval'?'waiting for approval':notice.status==='failed'||notice.status==='cancelled'?'failed':String(notice.status||'ended');
      summary.append(icon,document.createTextNode(' '+(notice.title||'Sub-agent')+' · '+state));entry.append(summary);
      if(typeof notice.reason==='string'&&notice.reason){const reason=document.createElement('p');reason.className='v3-subagent-reason';reason.textContent='Reason: '+notice.reason;entry.append(reason);}
      const report=document.createElement('div');report.className='v3-subagent-report';
      markdown(report,typeof notice.reply==='string'&&notice.reply?notice.reply:'(no reply text)');entry.append(report);
      if(typeof notice.session_id==='string'&&/^[-A-Za-z0-9_]{1,128}$/.test(notice.session_id)){const link=document.createElement('a');link.href='/app?session_id='+encodeURIComponent(notice.session_id);link.textContent='Open sub-agent session';link.className='v3-subagent-link';entry.append(link);}
      row.append(entry);
    }
    _refreshLucideIcons(row);return row;
  }
  if(item.role==='system'){
    const row=document.createElement('article');row.className='v3-message system';row.dataset.publicId=item.id;row.dataset.seq=String(item.seq);
    const text=document.createElement('p');text.textContent=typeof item.content==='string'?item.content:JSON.stringify(item.content);row.append(text);return row;
  }
  const profile=currentChatProfile()||{},role=item.role==='user'?'user':'agent';
  let text=typeof item.content==='string'?item.content:JSON.stringify(item.content);
  const bubble=document.createElement('article');bubble.className='chat-bubble '+role+' published';bubble.dataset.publicId=item.id;bubble.dataset.msgId=item.id;bubble.dataset.seq=String(item.seq);bubble.dataset.turnId=item.run_id;
  const label=document.createElement('span');label.className='label';label.textContent=item.message_kind==='operator'?((item.author?.display_name||item.author?.user_id||item.author_id||'Human')+' (Agent Operator)'):item.message_kind==='agent_message'?(item.author_name||'Message from another agent'):role==='user'?(item.author?.display_name||item.author?.user_id||item.author_name||item.author_id||item.author_display_name||item.author_user_id||'You'):'Agent';bubble.append(label);
  const body=document.createElement('div');
  if(role==='agent'){const section=document.createElement('div');section.className='turn-section llm-section md';section.dataset.sectionIdx='0';section.dataset.msgId=item.id;markdown(body,text);section.append(body);bubble.append(section);}else{body.className='bubble-body';linkify(body,text);bubble.append(body);}
  // User uploads and the agent's tool-produced files (such as validation screenshots) share one tile row.
  if(Array.isArray(item.attachments)){
    const files=document.createElement('div');files.className='chat-attachments';
    const sid=new URL(location.href).searchParams.get('session_id');
    for(const file of item.attachments){
      if(typeof file.id!=='string'||!/^[0-9a-f-]{36}$/i.test(file.id)||!sid)continue;
      const link=document.createElement('a');link.href='/api/v3/sessions/'+encodeURIComponent(sid)+'/attachments/'+encodeURIComponent(file.id)+'/content';link.target='_blank';link.rel='noopener';
      const name=file.name||'Image attachment';
      // The API does not always flag an image, so trust the media type or file name as well; otherwise a
      // PNG renders as a filename pill and clicking it downloads instead of opening the preview.
      const isImage=file.preview===true||/^image\//i.test(file.media_type||file.mime_type||file.mime||file.type||'')||/\.(?:png|jpe?g|gif|webp|avif|bmp|svg|heic|heif)$/i.test(name);
      const image=isImage?document.createElement('img'):null;
      // Images are bare rounded tiles that open the shared viewer; other files keep their name as the link label.
      if(image){link.className='chat-attachment-image';image.alt=name;image.loading='lazy';image.decoding='async';link.append(image);link.title=name;link.setAttribute('aria-label','Preview '+name);}
      else{link.className='chat-attachment-file';link.textContent=file.name||'Attachment';link.download=file.name||'attachment';}
      let ready=!resolveAsset,assetError='';
      // The main app authorizes attachment reads with its own cookie, so the content URL is a valid image
      // source; the embedded host has no cookie and supplies resolveAsset to swap in an authorized blob URL.
      if(image&&!resolveAsset)image.src=link.href;
      if(image)link.addEventListener('click',event=>{if(event.button!==0||event.metaKey||event.ctrlKey||event.shiftKey||event.altKey)return;event.preventDefault();if(assetError){note('Attachment unavailable: '+assetError);return;}if(!ready){note('Attachment is loading; try again shortly.');return;}openChatImagePreview(image.currentSrc||image.getAttribute('src')||link.getAttribute('href'),name,link);});
      // Resolve attachment content lazily; detached rows are ignored and failed links stay non-navigable.
      if(resolveAsset){const url=link.href;
        if(!image)link.addEventListener('click',event=>{if(ready)return;event.preventDefault();note(assetError?'Attachment unavailable: '+assetError:'Attachment is loading; try again shortly.');});
        resolveAsset(url).then(blobUrl=>{if(!link.isConnected)return;ready=true;link.href=blobUrl;if(image)image.src=blobUrl;}).catch(error=>{assetError=error.message;});}
      // Agent images named by a label (not the default screenshot-<id>.png) show that label under the tile.
      const caption=image&&role==='agent'&&!/^screenshot-[0-9a-f]{8}\.png$/i.test(name)?name.replace(/\.png$/i,''):'';
      if(caption){const cell=document.createElement('figure');cell.className='chat-attachment-labeled';const text=document.createElement('figcaption');text.textContent=caption;cell.append(link,text);files.append(cell);}
      else files.append(link);
    }
    if(files.childElementCount)bubble.append(files); // Empty or filtered attachment lists must not reserve spacing.
  }
  const cfg=profile.message_bubbles||{},lane=cfg[role]||{};bubble.style.fontSize=(cfg.font_size_px||17)+'px';applyBubblePaint(bubble,lane);
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

/** Composes the shared transcript row; bubble identity owns actions while row metadata owns ordering/read tracking. */
export function createMessageRow(item,options){
  const bubble=createMessage(item,options);
  if(!bubble||item.role==='system'||item.role==='notice'||bubble.dataset.subagentNotices)return bubble;
  const row=document.createElement('article');row.className='v3-message message-row '+(item.role==='user'?'user':'agent');
  for(const key of ['publicId','msgId','seq','turnId'])row.dataset[key]=bubble.dataset[key];
  // Move the configured gutter outside the painted surface for both roles without replacing its handlers.
  row.append(bubble,bubble.querySelector('.turn-gutter'));
  return row;
}
