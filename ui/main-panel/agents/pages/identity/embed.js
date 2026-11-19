import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigForm,createConfigList,createToggleRow} from '../../../../shared/config-rows/config-rows.js';
/** Loads and saves per-agent embed enablement, guest access, and exact allowed origins; appearance stays in the chat UI profile. */
export async function renderEmbedSettings(host,request,{agentId}){
 const row=createSubpage(host,{id:'website-chat-widget',title:'Website chat widget',toggleLabel:'Enable website embedding'});
 const section=row.body;row.toggle.disabled=true;
 const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');section.append(status);
 let current;try{current=await request('/agents/'+encodeURIComponent(agentId)+'/embed')}catch(error){status.textContent='Widget settings unavailable: '+error.message;return}
 const enabled=row.toggle;enabled.checked=!!current.config.enabled;enabled.disabled=false;
 // The header switch, guest switch and origins save together with one explicit Save.
 const form=createConfigForm(section,{saveLabel:'Save widget settings',onSave:async values=>{
  current=await request('/agents/'+encodeURIComponent(agentId)+'/embed','PUT',{expected_revision:current.revision,config:{enabled:values.enabled,guest_access:values.guest_access,allowed_origins:values.allowed_origins.split(/\s+/).filter(Boolean)}});
  status.textContent='Widget settings saved';
 }});
 form.register('enabled');form.set('enabled',enabled.checked,true);enabled.addEventListener('change',()=>form.set('enabled',enabled.checked));
 const guests=createToggleRow(createConfigList(section,{label:'Widget access'}),{title:'Allow guest chat when the app admits guests',value:!!current.config.guest_access,key:'guest_access',form});
 // Origins are a list, one per line, so they keep a plain list box rather than the prose editor.
 const origins=document.createElement('textarea');origins.className='ac-input';origins.rows=3;origins.value=current.config.allowed_origins.join('\n');origins.placeholder='https://example.com';origins.setAttribute('aria-label','Allowed website origins, one per line');
 form.register('allowed_origins');form.set('allowed_origins',origins.value,true);origins.addEventListener('input',()=>form.set('allowed_origins',origins.value));
 const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Allow exact website origins. Agent, account and tool policies govern chat actions. Appearance and launcher choices use chat.json → chat_widget.';
 section.append(origins,hint);form.attach();
 const snippet=document.createElement('pre');snippet.style.whiteSpace='pre-wrap';snippet.textContent='<script src="'+location.origin+'/ui/embed/loader.js" data-agent="'+agentId+'" async></script>';
 section.append(snippet);
}
