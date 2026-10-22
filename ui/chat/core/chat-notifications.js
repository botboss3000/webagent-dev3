// @code-index {"version":1,"feature":"chat-run-notifications","aliases":["run approval","mode confirmation"],"purpose":"Locates guarded mode confirmations and pending run-approval cards in the composer","span_lines":80,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider notice labels and presentation settings; approval and cancellation authority remain enforced."]}}
// One composer surface for confirmations issued by chat controls and the runtime.
// Mount composer confirmation slots; decisions remain caller-owned guarded callbacks.
// All chat notices use these card and action primitives, including notices owned by other controllers.
export function createChatNotificationAction({label,kind='neutral',run,title,disabled=false},{documentObject=globalThis.document,guard=fn=>fn}={}) {
  const button=documentObject.createElement('button');button.type='button';
  button.className='chat-notification-action chat-notification-'+kind;button.textContent=label;
  button.disabled=disabled;if(title)button.title=title;button.onclick=guard(run);return button;
}
// Render inert text and caller-owned detail content; callbacks retain their original authority checks.
export function createChatNotification({title,message,details,body,actions=[],documentObject=globalThis.document,guard=fn=>fn}) {
  const node=documentObject.createElement('section');node.className='chat-notification';
  node.setAttribute('role','group');node.setAttribute('aria-label',title);
  const heading=documentObject.createElement('div');heading.className='chat-notification-heading';
  const label=documentObject.createElement('strong');label.textContent=title;heading.append(label);
  if(details)heading.append(details.toggle);node.append(heading);
  if(message!=null){const text=documentObject.createElement('p');text.className='chat-notification-message';text.textContent=message;node.append(text);}
  if(details)node.append(details.body);if(body)node.append(body);
  const buttons=documentObject.createElement('div');buttons.className='chat-notification-actions';
  for(const item of actions)buttons.append(createChatNotificationAction(item,{documentObject,guard}));
  node.append(buttons);return node;
}
export function mountChatNotifications({root,guard,request,note}) {
  const host=root.querySelector('#chat-notifications');
  const modeSlot=document.createElement('div');
  const toolSlot=document.createElement('div');
  host.append(modeSlot,toolSlot);
  let finishMode=null;const routeChoices=new Map();

  // Bind the common renderers to this panel's interaction guard.
  const action=(label,kind,run)=>createChatNotificationAction({label,kind,run},{guard});
  const card=options=>createChatNotification({...options,guard});
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
  const routeToggle=document.createElement('button');routeToggle.type='button';routeToggle.className='chat-notification-detail-toggle';routeToggle.textContent='Show permissions';routeToggle.setAttribute('aria-controls',routeList.id);routeToggle.setAttribute('aria-expanded','false');
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
  // Run status, observer approval and uncertain-outcome controls share the approval card UI.
  function showRun({run,active,canControl=false,expanded,renderRecovery,resume,cancel,recover}) {
    if(run.status==='completed'||(!run.uncertain&&['queued','running','waiting_gate','waiting_external','failed'].includes(run.status)))return;
    let title=run.status+(run.error?' — '+run.error:''),body;const actions=[];
    if(run.pending&&run.status==='waiting_approval')title=run.pending.waiting_message||'Waiting for the authorized user';
    else {
      if((run.uncertain||run.status==='recovery')&&canControl){
        title='Run recovery';
        body=renderRecovery(run,{expanded,button:(label,fn)=>action(label,label==='Authorize retry'?'approve':label==='Abandon run'?'deny':'neutral',fn),decide:recover});
      }
      if(canControl&&!run.uncertain&&['paused','stopped'].includes(run.status))actions.push({label:'Resume run',kind:'approve',run:resume});
      if(canControl&&active?.id===run.id)actions.push({label:'Cancel run',kind:'neutral',run:cancel});
    }
    const node=card({title,body,actions});runSlot.append(node);return node;
  }
  function clearRuns(){runSlot.replaceChildren();clearTools();}
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
      node.querySelector('.chat-notification-deny').focus({preventScroll:true});
    });
  }
  // Show a pending approval, persist expansion in the caller's Set, and delegate decisions.
  function showTool({approval,run,active,expanded,decide,cancel}) {
    const details=document.createElement('pre');
    details.id='approval-details-'+approval.id;
    details.hidden=!expanded.has(approval.id);
    details.textContent=JSON.stringify(approval.messaging_binding||approval.arguments,null,2);
    const remembered=document.createElement('input');remembered.type='checkbox';remembered.checked=routeChoices.get(approval.id)===true;remembered.onchange=()=>{routeChoices.set(approval.id,remembered.checked);if(routeChoices.size>100)routeChoices.delete(routeChoices.keys().next().value);};
    const rememberLabel=document.createElement('label');rememberLabel.append(remembered,document.createTextNode(' Remember this route for queue and steer with these recipient permissions'));
    const toggle=document.createElement('button');
    toggle.type='button';
    toggle.className='chat-notification-detail-toggle';
    toggle.textContent=approval.new_intent_confirmation ? 'Additional action: '+approval.name : approval.name;
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
      {label:approval.new_intent_confirmation ? 'Approve additional action' : 'Approve',kind:'approve',run:()=>decide('approve',approval.messaging_can_remember?{remember_messaging_route:remembered.checked}:{})},
      {label:'Deny',kind:'deny',run:()=>decide('deny')}
    ];
    if(active?.id===run.id)actions.push({label:'Cancel run',kind:'neutral',run:cancel});
    const node=card({title:approval.messaging_binding?'Approve agent message':approval.new_intent_confirmation ? 'Confirm a separate action after '+approval.separate_from+':' : 'Need approval:',details:{toggle,body:details},actions});
    if(approval.messaging_binding){const text=document.createElement('p');const b=approval.messaging_binding;text.textContent='From '+(b.source_title||b.source_session_id)+' to '+(b.target_title||b.target_session_id)+' · '+b.target_agent_id+' · '+b.delivery+' · recipient '+(b.target_mode?.mode==='write'?'write-capable':'read-only');node.insertBefore(text,node.lastChild);}
    if(approval.messaging_can_remember)node.insertBefore(rememberLabel,node.lastChild);
    toolSlot.append(node);pendingApprovals=true;updateRoutesVisibility();
  }
  // Remove tool-approval cards without affecting a mode confirmation.
  function clearTools(){toolSlot.replaceChildren();pendingApprovals=false;updateRoutesVisibility();}
  // Cancel pending confirmation and remove both slots from the supplied composer host.
  function destroy(){messagingRoutes.destroy();routeChoices.clear();cancelMode();clearRuns();host.replaceChildren();}
  return {confirmMode,cancelMode,showTool,clearTools,showRun,clearRuns,messagingRoutes,destroy};
}
