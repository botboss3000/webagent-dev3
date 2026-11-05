// @code-index {"version":1,"feature":"chat-run-notifications","aliases":["run approval","mode confirmation"],"purpose":"Locates guarded mode confirmations and pending run-approval cards in the composer","span_lines":80,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider notice labels and presentation settings; approval and cancellation authority remain enforced."]}}
// One composer surface for confirmations issued by chat controls and the runtime.
// Mount composer confirmation slots; decisions remain caller-owned guarded callbacks.
// All chat notices use these card and action primitives, including notices owned by other controllers.
export function createChatActionButton({label,kind='neutral',run,title,disabled=false},{documentObject=globalThis.document,guard=fn=>fn}={}) {
  const button=documentObject.createElement('button');button.type='button';
  button.className='chat-action-action chat-action-'+kind;button.textContent=label;
  button.disabled=disabled;if(title)button.title=title;button.onclick=guard(run);return button;
}
// Raw code surface: a capped, internally scrolling <pre> with a Copy control pinned to its top-right corner.
// The control only reads text that is already rendered; it never changes what an action does.
export function createCodeBlock(text,{documentObject=globalThis.document}={}) {
  const wrap=documentObject.createElement('div');wrap.className='chat-action-code';
  const pre=documentObject.createElement('pre');pre.textContent=text;
  const copy=documentObject.createElement('button');copy.type='button';copy.className='chat-action-copy';
  const label=value=>{copy.textContent=value;copy.title=value==='Copy'?'Copy code':value;copy.setAttribute('aria-label',copy.title);};label('Copy');
  let reset=null;
  copy.onclick=async event=>{
    event?.preventDefault?.();event?.stopPropagation?.();
    let ok=true;try{await writeClipboard(pre.textContent,documentObject);}catch{ok=false;}
    label(ok?'Copied':'Copy failed');clearTimeout(reset);reset=setTimeout(()=>label('Copy'),1600);
  };
  wrap.append(pre,copy);return {wrap,pre,copy};
}
// Clipboard API first; the hidden-textarea fallback covers non-secure origins and older mobile browsers.
async function writeClipboard(text,documentObject) {
  const clipboard=globalThis.navigator?.clipboard;
  if(clipboard?.writeText&&globalThis.isSecureContext!==false){try{return await clipboard.writeText(text);}catch{}}
  const area=documentObject.createElement('textarea');area.value=text;area.setAttribute('readonly','');
  area.style.cssText='position:fixed;top:0;left:0;opacity:0;pointer-events:none';
  documentObject.body.append(area);area.select();let ok=false;
  try{ok=documentObject.execCommand('copy');}finally{area.remove();}
  if(!ok)throw Error('Copy failed');
}
// Render inert text and caller-owned detail content; callbacks retain their original authority checks.
export function createChatAction({title,message,details,body,actions=[],documentObject=globalThis.document,guard=fn=>fn}) {
  const node=documentObject.createElement('section');node.className='chat-action';
  node.setAttribute('role','group');node.setAttribute('aria-label',title);
  const heading=documentObject.createElement('div');heading.className='chat-action-heading';
  const label=documentObject.createElement('strong');label.textContent=title;heading.append(label);
  if(details)heading.append(details.toggle);node.append(heading);
  if(message!=null){const text=documentObject.createElement('p');text.className='chat-action-message';text.textContent=message;node.append(text);}
  if(details)node.append(details.body);if(body)node.append(body);
  const buttons=documentObject.createElement('div');buttons.className='chat-action-actions';
  for(const item of actions)buttons.append(createChatActionButton(item,{documentObject,guard}));
  node.append(buttons);return node;
}
// Shared notices for any page: shown above the chat composer while it is on screen,
// otherwise in a floating stack. A key replaces its earlier notice; actions close
// the notice when they succeed and show their error otherwise.
export function showNotification({key,title,message='',actions=[],timeout=10000,documentObject=globalThis.document}) {
  const panel=documentObject.getElementById('chat-action-panel');
  let host=panel?.closest('#chat-input-area')?.getClientRects().length?panel:documentObject.getElementById('app-notifications');
  if(!host){host=documentObject.createElement('div');host.id='app-notifications';host.setAttribute('aria-live','polite');documentObject.body.append(host);}
  if(key)for(const old of documentObject.querySelectorAll('[data-notification-key]'))if(old.dataset.notificationKey===key)old.remove();
  let timer=null,node=null;
  const close=()=>{clearTimeout(timer);node?.remove();};
  const wrap=item=>({...item,run:async()=>{
    try{await item.run();close();}catch(error){node.querySelector('.chat-action-message').textContent=error?.message||'Action failed';}
  }});
  node=createChatAction({title,message,documentObject,actions:[...actions.map(wrap),{label:'Dismiss',kind:'neutral',run:close}]});
  node.setAttribute('role','status');if(key)node.dataset.notificationKey=key;
  host.append(node);
  if(timeout)timer=setTimeout(close,timeout);
  return {close};
}
export function mountChatActionPanel({root,guard,request,note}) {
  const host=root.querySelector('#chat-action-panel');
  const modeSlot=document.createElement('div');
  const toolSlot=document.createElement('div');
  host.append(modeSlot,toolSlot);
  let finishMode=null;const routeChoices=new Map();
  // Keyed run/approval cards. A refresh with unchanged display data keeps the same nodes, so text
  // selection, scroll position, focus and typed recovery input survive the frequent run polls.
  const runCards=new Map(),toolCards=new Map();let cycle=null;
  function carryInput(from,to){
    const pairs=(selector)=>[from.querySelectorAll?.(selector)||[],to.querySelectorAll?.(selector)||[]];
    const [oldText,newText]=pairs('textarea');for(let i=0;i<Math.min(oldText.length,newText.length);i++)if(oldText[i].value)newText[i].value=oldText[i].value;
    const [oldPre,newPre]=pairs('pre');for(let i=0;i<Math.min(oldPre.length,newPre.length);i++)newPre[i].scrollTop=oldPre[i].scrollTop;
  }
  function holdsSelection(node){
    try{
      const selection=globalThis.getSelection?.();
      if(!selection||selection.isCollapsed||!selection.rangeCount)return false;
      return !!node.contains?.(selection.anchorNode)||!!node.contains?.(selection.focusNode);
    }catch{return false;}
  }
  function upsert(slot,cards,kind,key,sig,build){
    if(!cycle){const node=build();slot.append(node);return node;}
    cycle[kind].add(key);const prior=cards.get(key);let node=prior?.node;
    // While the user is highlighting text inside a card, defer its rebuild; the stored signature stays old,
    // so the next refresh after the selection ends applies the change.
    const holding=prior&&node.isConnected&&prior.sig!==sig&&holdsSelection(node);
    if(!holding&&(!prior||prior.sig!==sig||!node.isConnected)){
      const next=build();if(prior&&node.isConnected){carryInput(node,next);node.replaceWith(next);}
      node=next;cards.set(key,{sig,node});
    }
    const last=cycle.last[kind],before=last?last.nextSibling:slot.firstChild;
    if(before!==node)slot.insertBefore(node,before||null);
    cycle.last[kind]=node;return node;
  }
  // A refresh brackets its showRun/showTool calls; cards not shown again are removed at the end.
  function beginRuns(){cycle={runs:new Set(),tools:new Set(),last:{runs:null,tools:null}};}
  function endRuns(){
    const done=cycle;cycle=null;if(!done)return;
    for(const [key,entry] of runCards)if(!done.runs.has(key)){entry.node.remove();runCards.delete(key);}
    for(const [key,entry] of toolCards)if(!done.tools.has(key)){entry.node.remove();toolCards.delete(key);}
    pendingApprovals=toolCards.size>0;updateRoutesVisibility();fitRoom();
  }
  // Cap each action card to the room left between the chat header and the pill after the band's other rows
  // (progress line, notices). CSS takes the tighter of this and 35dvh; the card scrolls inside its own border.
  // The value does not depend on the card's own height, so re-measuring on band resize converges.
  const band=host.closest?.('#chat-above-pill');
  function fitRoom(){
    try{
      const pill=root.querySelector('#chat-input-row'),header=root.querySelector('#chat-header');
      if(!band?.style?.setProperty||!pill?.getBoundingClientRect)return;
      const top=header?.getBoundingClientRect?Math.max(0,header.getBoundingClientRect().bottom):0;
      const other=Math.max(0,(band.getBoundingClientRect?.().height||0)-(host.getBoundingClientRect?.().height||0));
      const room=Math.max(120,Math.floor(pill.getBoundingClientRect().top-top-other-24))+'px';
      if(band.style.getPropertyValue?.('--chat-action-room')!==room)band.style.setProperty('--chat-action-room',room);
    }catch{}
  }
  const roomObserver=typeof ResizeObserver==='function'&&band?new ResizeObserver(()=>fitRoom()):null;
  try{
    const pill=root.querySelector('#chat-input-row'),panel=band?.closest?.('#chat-panel');
    if(roomObserver&&pill?.nodeType===1)roomObserver.observe(pill);
    if(roomObserver&&panel?.nodeType===1)roomObserver.observe(panel);
    if(roomObserver&&band?.nodeType===1)roomObserver.observe(band);
  }catch{}
  globalThis.visualViewport?.addEventListener?.('resize',fitRoom);fitRoom();

  // Bind the common renderers to this panel's interaction guard.
  const action=(label,kind,run)=>createChatActionButton({label,kind,run},{guard});
  const card=options=>createChatAction({...options,guard});
  // Remembered routes use the same card and action renderer as pending approvals.
  const routesSlot=document.createElement('div');host.append(routesSlot);
  let routeSession='',routeGeneration=0,routeRead=0,routeExpanded=false,routeCount=0,pendingApprovals=false;const revokeKeys=new Map();
  // Saved permissions accompany pending approvals; idle chats have no notification card.
  function updateRoutesVisibility(){
    const hidden=!pendingApprovals||!routeCount;routesSlot.hidden=routeCard.hidden=hidden;
    for(const row of routeList.children)row.hidden=hidden;
  }
  async function loadRoutes(){
    const sid=routeSession,epoch=routeGeneration,read=++routeRead;if(!sid)return;
    const data=await request('/sessions/'+encodeURIComponent(sid)+'/messaging-routes');
    if(sid!==routeSession||epoch!==routeGeneration||read!==routeRead)return;
    routeList.replaceChildren();
    const routes=(data.routes||[]).filter(route=>!route.revoked);
    routeCount=routes.length;
    if(!routes.length){const empty=document.createElement('p');empty.textContent='No remembered messaging routes.';routeList.append(empty);}
    for(const route of routes){
      const row=card({title:'To '+(route.target_title||route.scope.target_session_id),message:'queue or steer',actions:[{label:'Revoke',kind:'deny',run:async()=>{
        if(sid!==routeSession||epoch!==routeGeneration)return;
        const key=route.key+':'+route.revision;if(!revokeKeys.has(key))revokeKeys.set(key,crypto.randomUUID());
        const button=row.querySelector('button');button.disabled=true;
        try{
          await request('/sessions/'+encodeURIComponent(sid)+'/messaging-routes/'+encodeURIComponent(route.scope.target_session_id),'POST',{expected_revision:route.revision,client_request_id:revokeKeys.get(key)});
          revokeKeys.delete(key);if(sid===routeSession&&epoch===routeGeneration)await loadRoutes();
        }finally{if(button.isConnected)button.disabled=false;}
      }}]});
      routeList.append(row);
    }
    updateRoutesVisibility();
  }
  const routeList=document.createElement('div');routeList.id='messaging-permissions-details';routeList.hidden=true;
  const routeToggle=document.createElement('button');routeToggle.type='button';routeToggle.className='chat-action-detail-toggle';routeToggle.textContent='Show permissions';routeToggle.setAttribute('aria-controls',routeList.id);routeToggle.setAttribute('aria-expanded','false');
  routeToggle.onclick=guard(async()=>{
    routeExpanded=!routeExpanded;routeList.hidden=!routeExpanded;routeToggle.setAttribute('aria-expanded',String(routeExpanded));routeToggle.textContent=routeExpanded?'Hide permissions':'Show permissions';
    if(routeExpanded)await loadRoutes();
  });
  const routeCard=card({title:'Messaging permissions',details:{toggle:routeToggle,body:routeList},actions:[]});routesSlot.append(routeCard);routesSlot.hidden=routeCard.hidden=true;
  const messagingRoutes={
    open(id,owner){routeGeneration++;routeSession=owner?id:'';routeCount=0;pendingApprovals=false;routesSlot.hidden=routeCard.hidden=true;routeExpanded=false;routeList.hidden=true;routeList.replaceChildren();routeToggle.setAttribute('aria-expanded','false');routeToggle.textContent='Show permissions';},
    refresh:loadRoutes,
    disconnect(){this.open('',false);},
    destroy(){this.disconnect();revokeKeys.clear();routeToggle.onclick=null;routesSlot.remove();}
  };
  const runSlot=document.createElement('div');host.append(runSlot);
  // A send that exhausted its automatic retries; kept apart from run cards, which refresh constantly.
  const sendSlot=document.createElement('div');host.append(sendSlot);
  function showSendError({message,retry}){
    const node=card({title:'Message not sent',message,actions:[{label:'Retry',kind:'approve',run:async()=>{clearSendError();await retry();}},{label:'Dismiss',kind:'neutral',run:clearSendError}]});
    node.setAttribute('role','alert');sendSlot.replaceChildren(node);
  }
  function clearSendError(){sendSlot.replaceChildren();}
  // A private credential form owned by this conversation. It is rebuilt from session state, so it follows
  // the session across devices and session switches; the value goes only to the nonce-bound vault endpoint.
  const credentialSlot=document.createElement('div');host.append(credentialSlot);let credentialKey='',credentialSaved=false;
  function clearCredential(){credentialSlot.replaceChildren();credentialKey='';credentialSaved=false;}
  function showCredential(detail,session){
    if(!detail){if(!credentialSaved)clearCredential();return;}
    const key=session+':'+detail.requestId;if(key===credentialKey)return; // Keep a value being typed across polls.
    credentialKey=key;credentialSaved=false;
    const endpoint='/core/vault/'+encodeURIComponent(session)+'/'+encodeURIComponent(detail.name);
    const title=detail.label||'Private credential';
    const body=document.createElement('form');body.className='chat-action-credential';
    const where=document.createElement('p');where.className='chat-action-message';
    if(detail.createUrl){
      const link=document.createElement('a');link.href=detail.createUrl;link.target='_blank';link.rel='noopener noreferrer';
      const url=new URL(detail.createUrl);link.textContent=url.host+(url.pathname==='/'?'':url.pathname);
      where.append(document.createTextNode('Create it at '),link,document.createTextNode(', then paste it here.'));
    }else where.textContent='Paste the value for '+new URL(detail.origin).host+'.';
    // The privacy note expands inside the card behind an info toggle so the card stays compact.
    const note=document.createElement('p');note.id='chat-action-note-'+detail.requestId.slice(0,12);note.className='chat-action-note';note.hidden=true;
    // The same shield, larger, marks the note as security information.
    const noteShield=document.createElement('span');noteShield.className='chat-action-secure';noteShield.setAttribute('aria-hidden','true');
    const noteText=document.createElement('span');noteText.textContent='Only you see this value. It is stored privately in your vault, and the agent receives status only, never the value itself.';
    note.append(noteShield,noteText);
    const info=document.createElement('button');info.type='button';info.className='chat-action-info';info.textContent='i';info.setAttribute('aria-label','Who can see this value');
    info.setAttribute('aria-controls',note.id);info.setAttribute('aria-expanded','false');
    info.onclick=()=>{note.hidden=!note.hidden;info.setAttribute('aria-expanded',String(!note.hidden));};
    where.append(document.createTextNode(' '),info);
    const input=document.createElement('input');input.type='password';input.autocomplete='off';input.required=true;input.maxLength=8192;input.dataset.secret='true';input.setAttribute('aria-label',title);
    const status=document.createElement('p');status.className='chat-action-message';status.setAttribute('role','status');status.setAttribute('aria-live','polite');
    body.append(where,note,input,status);
    const save=async()=>{
      if(!input.value)return input.focus();
      status.textContent='Saving…';
      try{await request(endpoint,'POST',{request_id:detail.requestId,expected_revision:detail.revision,value:input.value});}
      catch(error){input.value='';status.textContent=error.message;return;}
      input.value='';credentialSaved=true;
      credentialSlot.replaceChildren(card({title:title+' saved',message:'Saved privately. Tell the agent to continue.',actions:[{label:'Dismiss',kind:'neutral',run:clearCredential}]}));
    };
    body.onsubmit=guard(async event=>{event.preventDefault();await save();});
    const node=card({title,body,actions:[
      {label:'Save',kind:'approve',run:save},
      // Dismiss clears the request server-side, so the card disappears on every device.
      {label:'Dismiss',kind:'neutral',run:async()=>{await request(endpoint,'DELETE',{request_id:detail.requestId});clearCredential();}}
    ]});
    // A shield marks the private vault form apart from ordinary approval cards.
    const shield=document.createElement('span');shield.className='chat-action-secure';shield.setAttribute('role','img');shield.setAttribute('aria-label','Private vault input');
    const heading=node.querySelector('.chat-action-heading');heading.insertBefore(shield,heading.children[0]);
    credentialSlot.replaceChildren(node);
  }
  // Run status, observer approval and uncertain-outcome controls share the approval card UI.
  function showRun({run,active,canControl=false,expanded,renderRecovery,cancel,recover}) {
    if(run.status==='completed'||(!run.uncertain&&['queued','running','waiting_gate','waiting_external','failed','paused','stopped','cancelled'].includes(run.status)))return;
    // Only displayed fields form the signature; callbacks are keyed by the same run ID and remain equivalent.
    const sig=JSON.stringify([run.status,run.error||'',!!run.uncertain,run.pending||null,run.tool_failure||null,!!canControl,active?.id===run.id]);
    return upsert(runSlot,runCards,'runs',run.id,sig,()=>{
      let title=run.status+(run.error?' — '+run.error:''),body;const actions=[];
      if(run.pending&&run.status==='waiting_approval')title=run.pending.waiting_message||'Waiting for the authorized user';
      else {
        if((run.uncertain||run.status==='recovery')&&canControl){
          title='Run recovery';
          body=renderRecovery(run,{expanded,button:(label,fn)=>action(label,label==='Run again'||label==='Resume run'?'approve':label==='End run'?'deny':'neutral',fn),decide:recover,code:text=>createCodeBlock(text).wrap});
        }
        if(canControl&&active?.id===run.id)actions.push({label:'Cancel run',kind:'neutral',run:cancel});
      }
      return card({title,body,actions});
    });
  }
  function clearRuns(){runSlot.replaceChildren();runCards.clear();clearTools();}
  // Resolve an outstanding auto-mode prompt as denied/cancelled.
  function cancelMode() {
    finishMode?.(false);
  }
  // Replace the prior prompt and resolve from approve, deny, or Escape.
  function confirmMode(label) {
    cancelMode();
    return new Promise(resolve=>{
      const previous=document.activeElement;
      // Settle only the currently mounted prompt, restore focus if possible, and clear its DOM.
      const settle=accepted=>{
        if(finishMode!==settle)return;
        finishMode=null;
        modeSlot.replaceChildren();
        if(previous?.isConnected)previous.focus({preventScroll:true});
        resolve(accepted);
      };
      finishMode=settle;
      const node=card({
        title:'This will allow all write permitted tools',
        actions:[
          // These action callbacks resolve the confirmation promise; they do not issue a separate API request.
      {label:'Approve Auto mode',kind:'approve',run:()=>settle(true)},
          {label:'Deny',kind:'deny',run:()=>settle(false)}
        ]
      });
      // Escape denies the confirmation without changing session execution state.
      node.addEventListener('keydown',event=>{
        if(event.key==='Escape'){event.preventDefault();settle(false);}
      });
      modeSlot.replaceChildren(node);
      node.querySelector('.chat-action-deny').focus({preventScroll:true});
    });
  }
  // Show a pending approval, persist expansion in the caller's Set, and delegate decisions.
  // Approval cards are keyed by approval ID; an unchanged approval keeps its node across refreshes.
  function showTool(options) {
    const {approval,run,active}=options;
    const sig=JSON.stringify([approval,run?.id,active?.id===run?.id]);
    const node=upsert(toolSlot,toolCards,'tools',approval.id,sig,()=>buildTool(options));
    pendingApprovals=true;updateRoutesVisibility();return node;
  }
  function buildTool({approval,run,active,expanded,decide,cancel}) {
    // The raw arguments use the shared capped code surface with its top-right Copy control.
    const details=createCodeBlock(JSON.stringify(approval.messaging_binding||approval.arguments,null,2)).wrap;
    details.id='approval-details-'+approval.id;
    details.hidden=!expanded.has(approval.id);
    const remembered=document.createElement('input');remembered.type='checkbox';remembered.checked=routeChoices.get(approval.id)===true;remembered.onchange=()=>{routeChoices.set(approval.id,remembered.checked);if(routeChoices.size>100)routeChoices.delete(routeChoices.keys().next().value);};
    const rememberLabel=document.createElement('label');rememberLabel.append(remembered,document.createTextNode(' Remember this route for queue and steer with these recipient permissions'));
    const toggle=document.createElement('button');
    toggle.type='button';
    toggle.className='chat-action-detail-toggle';
    toggle.textContent=approval.repeat_of ? 'Repeat: '+approval.name : approval.new_intent_confirmation ? 'Additional action: '+approval.name : approval.name;
    toggle.title='Show tool call details';
    toggle.setAttribute('aria-controls',details.id);
    toggle.setAttribute('aria-expanded',String(!details.hidden));
    // Keep the caller-owned expansion Set in sync with the disclosure control.
    toggle.onclick=()=>{
      details.hidden=!details.hidden;
      if(details.hidden)expanded.delete(approval.id);
      else expanded.add(approval.id);
      toggle.setAttribute('aria-expanded',String(!details.hidden));
    };
    const actions=[
      // Decision callbacks are supplied by the panel and perform the authenticated API action.
      {label:approval.repeat_of ? 'Approve repeat' : approval.new_intent_confirmation ? 'Approve additional action' : 'Approve',kind:'approve',run:()=>decide('approve',approval.messaging_can_remember?{remember_messaging_route:remembered.checked}:{})},
      {label:'Deny',kind:'deny',run:()=>decide('deny')}
    ];
    if(active?.id===run.id)actions.push({label:'Cancel run',kind:'neutral',run:cancel});
    const node=card({title:approval.messaging_binding?'Approve agent message':approval.repeat_of ? 'Repeat an action the agent already ran?' : approval.new_intent_confirmation ? 'Confirm a separate action after '+approval.separate_from+':' : 'Need approval:',details:{toggle,body:details},actions});
    // The tool's repeat warning tells the user what approving may duplicate.
    if(approval.repeat_of&&typeof approval.repeat_warning==='string'){const text=document.createElement('p');text.textContent=approval.repeat_warning;node.insertBefore(text,node.lastChild);}
    if(approval.messaging_binding){const text=document.createElement('p');const b=approval.messaging_binding;text.textContent='From '+(b.source_title||b.source_session_id)+' to '+(b.target_title||b.target_session_id)+' · '+b.target_agent_id+' · '+b.delivery+' · recipient '+(b.target_mode?.mode==='write'?'write-capable':'read-only');node.insertBefore(text,node.lastChild);}
    if(approval.messaging_can_remember)node.insertBefore(rememberLabel,node.lastChild);
    return node;
  }
  // Remove tool-approval cards without affecting a mode confirmation.
  function clearTools(){toolSlot.replaceChildren();toolCards.clear();pendingApprovals=false;updateRoutesVisibility();}
  // Cancel pending confirmation and remove both slots from the supplied composer host.
  function destroy(){cycle=null;roomObserver?.disconnect();globalThis.visualViewport?.removeEventListener?.('resize',fitRoom);messagingRoutes.destroy();routeChoices.clear();cancelMode();clearRuns();clearSendError();clearCredential();host.replaceChildren();}
  return {confirmMode,cancelMode,showTool,clearTools,showRun,clearRuns,beginRuns,endRuns,showSendError,clearSendError,showCredential,clearCredential,messagingRoutes,destroy};
}
