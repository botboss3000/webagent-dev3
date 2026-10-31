import {requestTouchVibration} from '../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};

/** Accounts opening an agent link they cannot use may ask its Operators
 *  for access (V2 "Authorized" mode). The agent's name stays private until approved. */
export async function offerAccessRequest(request,agentId,{guest=false}={}){
  const path='/agents/'+encodeURIComponent(agentId)+'/access-request';
  let requested;try{({requested}=await request(path));}catch{return;}// Unknown agents get no prompt.
  const dialog=node('dialog','','request-access-dialog'),text=node('p','','ac-hint'),actions=node('div'),close=node('button','Close','ac-btn');
  dialog.setAttribute('aria-label','Request access');text.setAttribute('role','status');close.type='button';
  const pending=()=>{text.textContent='Your access request is pending. You can use this agent once an Operator approves it.';ask.hidden=true;};
  const ask=node('button','Request access','ac-btn');ask.type='button';
  for(const button of [ask,close])button.style.webkitTapHighlightColor='transparent';
  text.textContent='You don\'t have access to this agent yet. Its Operators can approve you.';
  ask.onclick=async event=>{requestTouchVibration(event);ask.disabled=true;try{await request(path,'POST',{});pending();}catch(error){text.textContent=error.message;ask.disabled=false;}};
  close.onclick=event=>{requestTouchVibration(event);dialog.close();};
  dialog.addEventListener('close',()=>dialog.remove());
  if(requested)pending();
  // Guests cannot hold memberships; they are told how to become eligible instead.
  if(guest){text.textContent='Sign in with an app account to request access to this agent.';ask.hidden=true;}
  actions.append(ask,close);dialog.append(node('h3','Request access'),text,actions);document.body.append(dialog);dialog.showModal();
}

/** Shows (once) any Operator decisions on this account's access requests. */
export async function showAccessNotices(request,roster){
  let notices;try{({notices}=await request('/access-notices','POST',{}));}catch{return;}
  if(!notices?.length)return;
  const dialog=node('dialog','','request-access-dialog'),actions=node('div'),close=node('button','Close','ac-btn');
  dialog.setAttribute('aria-label','Access requests');close.type='button';close.style.webkitTapHighlightColor='transparent';dialog.append(node('h3','Access requests'));
  for(const notice of notices){
    const name=roster.find(agent=>agent.id===notice.agent_id)?.name;
    if(notice.decision==='approved'){
      const line=node('p','You now have access to '+(name||'an agent you asked for')+'.','ac-hint'),open=node('button','Open','ac-btn');open.type='button';open.style.webkitTapHighlightColor='transparent';
      open.onclick=event=>{requestTouchVibration(event);location.href='/app?agent='+encodeURIComponent(notice.agent_id);};line.append(' ',open);dialog.append(line);
    }else dialog.append(node('p','An Operator declined one of your access requests.','ac-hint'));
  }
  close.onclick=event=>{requestTouchVibration(event);dialog.close();};dialog.addEventListener('close',()=>dialog.remove());
  actions.append(close);dialog.append(actions);document.body.append(dialog);dialog.showModal();
}
