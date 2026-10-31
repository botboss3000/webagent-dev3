import {requestTouchVibration} from '../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};

/** Signed-in accounts opening an agent link they cannot use may ask its Operators
 *  for access (V2 "Authorized" mode). The agent's name stays private until approved. */
export async function offerAccessRequest(request,agentId){
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
  actions.append(ask,close);dialog.append(node('h3','Request access'),text,actions);document.body.append(dialog);dialog.showModal();
}
