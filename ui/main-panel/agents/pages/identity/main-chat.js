import {requestTouchVibration} from '../../../../shared/vibrate.js';

/** Agent-specific coordination settings and prompt overrides. Config: config/prompts.json -> main_chat_skills.contribute/review supplies placeholders only; the host resolves effective instructions. */
export async function renderMainChat(host, request, {agentId,canManage=false}={}) {
  if(!agentId)throw Error('Agent identity is required');
  const path='/models/agents/'+encodeURIComponent(agentId)+'/main-chat';
  const section=document.createElement('section');section.className='main-chat-settings conn-card ac-row';section.dataset.agentId=agentId;section.style.webkitTapHighlightColor='transparent';host.append(section);
  const header=document.createElement('div');header.className='conn-card-header ac-ability-row';section.append(header);
  const expand=document.createElement('button');expand.type='button';expand.className='ac-ability-label';expand.setAttribute('aria-label','Expand Main Chat');expand.setAttribute('aria-expanded','false');expand.setAttribute('aria-controls','main-chat-body-'+agentId);
  expand.style.cssText='display:flex;align-items:center;gap:8px;flex:1;min-width:0;min-height:36px;text-align:left;border:0;background:transparent;color:inherit;cursor:pointer';
  const chevron=document.createElement('span');chevron.className='ac-row-chevron ac-row-chevron--left';chevron.textContent='›';chevron.setAttribute('aria-hidden','true');
  const title=document.createElement('span');title.className='ac-ability-name';title.textContent='Main Chat';expand.append(chevron,title);header.append(expand);
  const toggleWrap=document.createElement('label');toggleWrap.className='conn-toggle-wrap ac-ability-toggle-wrap';
  const enabled=document.createElement('input');enabled.type='checkbox';enabled.className='conn-toggle';enabled.setAttribute('aria-label','Enable Main Chat');enabled.disabled=true;
  const track=document.createElement('span');track.className='conn-toggle-track';track.setAttribute('aria-hidden','true');toggleWrap.append(enabled,track);header.append(toggleWrap);
  const body=document.createElement('div');body.className='conn-fields ac-ability-body';body.id='main-chat-body-'+agentId;body.hidden=true;body.style.cssText='display:none;padding:12px;min-width:0';section.append(body);
  /** Expand only presentation; toggling or expanding never silently saves configuration. */
  function expanded(open){body.hidden=!open;body.style.display=open?'block':'none';section.classList.toggle('expanded',open);expand.setAttribute('aria-expanded',String(open));expand.setAttribute('aria-label',(open?'Collapse':'Expand')+' Main Chat');chevron.style.transform=open?'rotate(90deg)':'';}
  expand.addEventListener('click',()=>expanded(body.hidden));
  section.addEventListener('click',event=>{if(event.target.closest('button:not(:disabled),input[type=checkbox]:not(:disabled)'))requestTouchVibration(event);},{capture:true});
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Applies to all sessions for this agent. Changes apply after Save. Main Chat uses the same context settings as this agent’s other chats.';body.append(hint);
  const form=document.createElement('form');body.append(form);
  const fields=new Map([['enabled',enabled]]),editors=new Map();
  /** Build labeled fields without treating editable names as canonical session identity. */
  function field(key,label,choices=null){
    const wrap=document.createElement('div');wrap.className='ac-cfg-field';const text=document.createElement('label');text.className='ac-label';text.textContent=label;
    const input=document.createElement(choices?'select':'input');input.id='main-chat-'+key+'-'+agentId;input.dataset.mainChatField=key;text.htmlFor=input.id;input.className='ac-input';
    if(choices)for(const [value,label]of choices){const option=document.createElement('option');option.value=value;option.textContent=label;input.append(option);}
    else{input.type='text';input.required=true;input.maxLength=200;}
    wrap.append(text,input);fields.set(key,input);form.append(wrap);
  }
  field('chat_name','Main Chat name');field('notes_name','Main Notes name');
  field('write_mode','Notes write approval',[['every_write','Approve every write or edit'],['new_topics','Approve new topics and scope changes'],['automatic','Automatic writes and edits']]);
  for(const [key,title,description]of [['contribute','Contribute to Main Notes','Included automatically in this agent’s working sessions while enabled.'],['review','Coordinate Main Notes','Included automatically in this agent’s Main Chat while enabled.']]){
    const card=document.createElement('details');card.className='ac-row main-chat-skill';card.dataset.mainChatSkill=key;
    const summary=document.createElement('summary');summary.className='ac-ability-row';summary.style.cssText='cursor:pointer;padding:8px 0;min-height:32px';summary.textContent=title;card.append(summary);
    const label=document.createElement('label');label.className='ac-label';label.textContent=title+' override';
    const input=document.createElement('textarea');input.className='ac-input';input.id='main-chat-skill-'+key+'-'+agentId;input.dataset.mainChatSkillInput=key;input.rows=9;input.style.cssText='box-sizing:border-box;width:100%;max-width:100%;resize:vertical;white-space:pre-wrap';label.htmlFor=input.id;
    const note=document.createElement('p');note.className='ac-hint';note.textContent=description+' Leave empty and Save to use the prompts.json fallback shown as placeholder text.';
    const warning=document.createElement('p');warning.className='main-chat-skill-warning';warning.style.color='var(--danger, #dc2626)';warning.setAttribute('role','alert');warning.textContent='Fallback skill is not available. Enter an agent-specific override or ask the app administrator to update prompts.json.';warning.hidden=true;
    card.append(note,label,input,warning);form.append(card);editors.set(key,{input,warning});
  }
  const policy=document.createElement('p');policy.className='ac-hint';policy.textContent='Notes read/write tools remain independently available through Base and their tool permissions. The toggle controls coordination guidance and follow-ups. Saving notes or editing skills never authorizes execution. Disabling retains this agent’s chat and notes.';form.append(policy);
  const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn ac-btn-ghost';save.textContent='Save Main Chat';save.disabled=true;
  const reload=document.createElement('button');reload.type='button';reload.className='ac-btn ac-btn-ghost';reload.textContent='Reload settings';
  const open=document.createElement('a');open.className='ac-btn ac-btn-ghost';open.textContent='Open Main Chat';open.hidden=true;
  const confirmation=document.createElement('div');confirmation.className='main-chat-deactivation';confirmation.hidden=true;confirmation.setAttribute('role','alertdialog');confirmation.setAttribute('aria-labelledby','main-chat-deactivate-title-'+agentId);confirmation.style.cssText='border:var(--border-width,1px) solid var(--warning);border-radius:8px;padding:12px;margin:10px 0';
  const warningTitle=document.createElement('h4');warningTitle.id='main-chat-deactivate-title-'+agentId;warningTitle.textContent='Deactivate Main Chat?';
  const warningText=document.createElement('p');warningText.textContent='This Main Chat will become a normal session. Its history and this agent’s Main Notes are retained. Coordinated follow-ups are suspended while disabled. Enabling Main Chat again will create a new Main Chat; this session will not be promoted again.';
  const keep=document.createElement('button');keep.type='button';keep.className='ac-btn ac-btn-ghost';keep.textContent='Keep Main Chat';
  const confirm=document.createElement('button');confirm.type='button';confirm.className='ac-btn ac-btn-ghost';confirm.textContent='Confirm deactivation';confirmation.append(warningTitle,warningText,keep,confirm);form.append(confirmation,save,reload,open,status);
  let state=null,pending=null,busy=false,generation=0;
  const alive=()=>section.isConnected;
  const editable=()=>canManage&&state?.can_manage===true;
  /** Freeze uncertain requests, and retain server authority independently of client rendering. */
  function lock(value){for(const input of fields.values())input.disabled=value||!editable();for(const {input}of editors.values()){input.disabled=value;input.readOnly=!editable();}save.disabled=value||!editable();save.hidden=!!state&&!editable();reload.disabled=value;}
  /** Missing guidance is a visible warning only; never disable otherwise available functions. */
  function warnings(){for(const {input,warning}of editors.values())warning.hidden=!!input.value.trim()||!!input.placeholder.trim();}
  function dirty(){if(!busy&&editable()){status.textContent='Unsaved changes';warnings();}}
  for(const input of fields.values())input.addEventListener('input',dirty);
  enabled.addEventListener('change',()=>{expanded(true);dirty();});
  for(const {input}of editors.values())input.addEventListener('input',dirty);
  /** Adopt only the requested agent’s revisioned settings and overrides; placeholders never become saved overrides. */
  function paint(data){
    if(data.agent_id!==agentId||!data.settings||!data.skills)throw Error('Agent settings scope mismatch');
    state=data;pending=null;
    for(const [key,input]of fields){if(key==='enabled')input.checked=data.settings.enabled===true;else input.value=data.settings[key]||'';}
    for(const [key,{input}]of editors){input.value=typeof data.skills[key]?.override==='string'?data.skills[key].override:'';input.placeholder=typeof data.skills[key]?.fallback==='string'?data.skills[key].fallback:'';}
    open.hidden=!data.session_id;if(data.session_id)open.href='/app?session_id='+encodeURIComponent(data.session_id);warnings();lock(false);save.textContent='Save Main Chat';
  }
  /** Explicit reload reconciles conflicts; detached navigation cannot repaint another agent. */
  async function load(){const epoch=++generation;busy=true;lock(true);status.textContent='Loading Main Chat…';try{const data=await request(path);if(!alive()||epoch!==generation)return;paint(data);status.textContent=data.can_manage===true?'':'Only this agent’s administrators can edit these settings.';}catch(error){if(alive()&&epoch===generation)status.textContent='Main Chat unavailable: '+error.message;}finally{if(epoch===generation){busy=false;reload.disabled=false;}}}
  reload.addEventListener('click',()=>{if(!busy)void load();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(busy||!state||!editable())return;
    if(!pending){
      const settings={enabled:enabled.checked,chat_name:fields.get('chat_name').value.trim(),notes_name:fields.get('notes_name').value.trim(),write_mode:fields.get('write_mode').value};
      if(!settings.chat_name||!settings.notes_name){status.textContent='Both names are required.';return;}
      if([settings.chat_name,settings.notes_name].some(v=>new TextEncoder().encode(v).length>200)){status.textContent='Each name must fit within 200 UTF-8 bytes.';return;}
      const skill_overrides={};for(const [key,{input}]of editors){if(new TextEncoder().encode(input.value).length>65536){status.textContent='Each skill must fit within 65,536 UTF-8 bytes.';return;}skill_overrides[key]=input.value.trim()?input.value:null;}
      pending={expected_revision:state.revision,expected_session_revision:state.session_metadata_revision,request_id:crypto.randomUUID(),settings,skill_overrides};
    }
    // A second explicit activation approves the frozen deactivation payload; the server must enforce the same transition guard.
    if(state.settings.enabled===true&&pending.settings.enabled===false&&pending.confirm_deactivation!==true){confirmation.hidden=false;lock(true);status.textContent='Confirm deactivation before this change is saved.';keep.focus();return;}
    busy=true;lock(true);status.textContent='Saving Main Chat…';
    try{await request(path,'PUT',pending);if(!alive())return;const latest=await request(path);if(!alive())return;paint(latest);status.textContent='Main Chat saved. Updated skills apply on the next model call.';document.dispatchEvent(new CustomEvent('v3:main-chat-changed',{detail:{agentId}}));window.dispatchEvent(new Event('v3:sessions-changed'));}
    catch(error){if(!alive())return;status.textContent='Save not confirmed: '+error.message+'. Reload to reconcile, or retry the same change.';save.textContent='Retry save';save.disabled=false;reload.disabled=false;}
    finally{busy=false;}
  });
  /** Cancel only the deactivation intent; other unsaved name and skill edits stay in the form. */
  function cancelDeactivation(){if(confirmation.hidden||busy)return;pending=null;confirmation.hidden=true;enabled.checked=state.settings.enabled===true;lock(false);status.textContent='Deactivation cancelled. Other changes have not been saved.';enabled.focus();}
  keep.addEventListener('click',cancelDeactivation);
  confirm.addEventListener('click',()=>{if(!pending||busy||confirmation.hidden)return;pending.confirm_deactivation=true;confirmation.hidden=true;form.requestSubmit();});
  confirmation.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();cancelDeactivation();}});
  await load();
}
