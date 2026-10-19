// Owner-only remembered routes. The server is the grant and revocation authority.
export function mountMessagingRoutes({root,request,guard,note}) {
 const host=document.createElement('details');host.className='chat-notification chat-messaging-routes';host.hidden=true;
 const summary=document.createElement('summary');summary.textContent='Messaging permissions';host.append(summary);
 const list=document.createElement('div');host.append(list);root.querySelector('#chat-notifications').append(host);
 let session='',generation=0;const keys=new Map();
 async function load(){
  const sid=session,epoch=generation;if(!sid||!host.open)return;
  const data=await request('/sessions/'+encodeURIComponent(sid)+'/messaging-routes');
  if(sid!==session||epoch!==generation)return;
  list.replaceChildren();
  const routes=(data.routes||[]).filter(route=>!route.revoked);
  if(!routes.length){const empty=document.createElement('p');empty.textContent='No remembered messaging routes.';list.append(empty);}
  for(const route of routes){
   const row=document.createElement('p'),label=document.createElement('span'),button=document.createElement('button');
   label.textContent='To '+(route.target_title||route.scope.target_session_id)+' · queue or steer ';
   button.type='button';button.className='chat-notification-action chat-notification-deny';button.textContent='Revoke';
   button.onclick=guard(async()=>{
    if(sid!==session||epoch!==generation)return;
    const key=route.key+':'+route.revision;if(!keys.has(key))keys.set(key,crypto.randomUUID());
    button.disabled=true;
    try{
     await request('/sessions/'+encodeURIComponent(sid)+'/messaging-routes/'+encodeURIComponent(route.scope.target_session_id),'POST',{expected_revision:route.revision,client_request_id:keys.get(key)});
     keys.delete(key);if(sid===session&&epoch===generation)await load();
    }finally{if(button.isConnected)button.disabled=false;}
   });
   row.append(label,button);list.append(row);
  }
 }
 host.ontoggle=()=>{if(host.open)void load().catch(error=>note(error.message));};
 return {
  open(id,owner){generation++;session=owner?id:'';host.hidden=!session;host.open=false;list.replaceChildren();},
  disconnect(){generation++;session='';host.hidden=true;host.open=false;list.replaceChildren();},
  destroy(){this.disconnect();host.ontoggle=null;host.remove();}
 };
}
