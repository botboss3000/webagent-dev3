/** Loads and saves per-agent embed enablement, guest access, and exact allowed origins; appearance stays in the chat UI profile. */
export async function renderEmbedSettings(host,request,{agentId}){
 const section=document.createElement('details'),title=document.createElement('summary');title.textContent='Website chat widget';section.append(title);host.append(section);
 const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');section.append(status);
 let current;try{current=await request('/agents/'+encodeURIComponent(agentId)+'/embed')}catch(error){status.textContent='Widget settings unavailable: '+error.message;return}
 const form=document.createElement('form'),enabled=document.createElement('input');enabled.type='checkbox';enabled.checked=!!current.config.enabled;
 const label=document.createElement('label');label.textContent='Enable website embedding ';label.prepend(enabled);
 const guests=document.createElement('input');guests.type='checkbox';guests.checked=!!current.config.guest_access;
 const guestLabel=document.createElement('label');guestLabel.textContent='Allow guest chat when the app admits guests ';guestLabel.prepend(guests);
 const origins=document.createElement('textarea');origins.className='ac-input';origins.rows=3;origins.value=current.config.allowed_origins.join('\n');origins.placeholder='https://example.com';origins.setAttribute('aria-label','Allowed website origins, one per line');
 const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Allow exact website origins. Agent, account and tool policies govern chat actions. Appearance and launcher choices use chat.json → chat_widget.';
 const save=document.createElement('button');save.type='submit';save.className='ac-btn';save.textContent='Save widget settings';
 const snippet=document.createElement('pre');snippet.style.whiteSpace='pre-wrap';snippet.textContent='<script src="'+location.origin+'/v3-ui/embed/loader.js" data-agent="'+agentId+'" async></script>';
 form.append(label,guestLabel,origins,hint,save,snippet);section.append(form);
 form.onsubmit=async event=>{event.preventDefault();save.disabled=true;try{current=await request('/agents/'+encodeURIComponent(agentId)+'/embed','PUT',{expected_revision:current.revision,config:{enabled:enabled.checked,guest_access:guests.checked,allowed_origins:origins.value.split(/\s+/).filter(Boolean)}});status.textContent='Widget settings saved'}catch(error){status.textContent=error.message}finally{save.disabled=false}};
}
