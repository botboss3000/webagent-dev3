// @code-index {"version":1,"feature":"chat-embed-auth","aliases":["embedded chat authorization","PKCE popup consent","embed sign in"],"purpose":"Renders the account-side consent page for a one-use embed challenge and reports authorization to the opener only after server acceptance.","span_lines":40,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Challenge consent and opener-origin checks are authorization boundaries, not user-configurable UI settings."}}
/** Resolves the requested embed challenge, explains the agent and delegated chat actions, and posts a one-use authorization result to the same-origin opener after explicit consent. Returns false when no embed_auth challenge is present and true when the popup route is handled. */
export async function authorizeEmbedPopup({request,me}){
 const challenge=new URL(location.href).searchParams.get('embed_auth');if(!challenge)return false;
 const style=document.createElement('link');style.rel='stylesheet';style.href='/v3-ui/embed/embed.css';document.head.append(style);
 document.getElementById('app').hidden=true;document.body.classList.remove('is-booting');window.__boot?.reveal();
 const card=document.createElement('section');card.className='wa-embed-connect';const title=document.createElement('h1');title.textContent='Connect website chat';
 const detail=document.createElement('p'),status=document.createElement('p');status.setAttribute('role','status');
 const connect=document.createElement('button');connect.type='button';connect.textContent='Connect';connect.disabled=true;
 const cancel=document.createElement('button');cancel.type='button';cancel.textContent='Cancel';cancel.onclick=()=>window.close();card.append(title,detail,connect,cancel,status);document.body.append(card);
 try{const info=await request('/embed/challenges/'+encodeURIComponent(challenge));detail.textContent=info.origin+' wants to use '+info.agent.name+' as '+(me.display_name||me.user||'your account')+'. The website can control this agent’s chat, including approvals and recovery. Existing permissions still apply.';connect.disabled=false}catch(error){status.textContent=error.message;return true}
 connect.onclick=async()=>{connect.disabled=true;try{await request('/embed/authorize','POST',{challenge_id:challenge});window.opener?.postMessage({namespace:'webagent.widget.auth',challenge_id:challenge,authorized:true},location.origin);status.textContent='Connected. You can close this window.';window.close()}catch(error){status.textContent=error.message;connect.disabled=false}};return true;
}
