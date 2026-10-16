// @code-index {"version":1,"feature":"chat-elements","aliases":["chat elements","activity group","typing ellipsis","mini transcript"],"purpose":"Owns modular chat elements. Config: data/config/chat.json -> chat_common.chat_elements and show_activity_groups, merged with desktop/mobile/widget profiles.","span_lines":16,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/chat_elements","/chat_common/show_activity_groups"],"profile_overrides":true}],"opportunities":[]}}
// Typing ellipsis uses public run state and never requests private activity.
const ACTIVE=new Set(['queued','running','waiting_approval','waiting_gate','waiting_external','finalizing','stopping']);
/** Own a simple activity indicator; an optional action can open a transcript later. */
export function createTypingEllipsis({onActivate}={}) {
  const element=document.createElement(onActivate?'button':'div');
  element.className='chat-typing-ellipsis';element.hidden=true;
  element.setAttribute('aria-label','Agent is working');
  if(onActivate){element.type='button';element.addEventListener('click',onActivate);}
  else element.setAttribute('role','status');
  for(let i=0;i<3;i++){const dot=document.createElement('span');dot.setAttribute('aria-hidden','true');element.append(dot);}
  return {element,
    // Terminal or absent runs hide the indicator and stop its animation through display:none.
    update(runs,{enabled=true}={}){element.hidden=!enabled||!runs.some(run=>ACTIVE.has(run.state||run.status));},
    // Remove any optional activation handler and the mount-owned DOM.
    dispose(){if(onActivate)element.removeEventListener('click',onActivate);element.remove();}};
}
