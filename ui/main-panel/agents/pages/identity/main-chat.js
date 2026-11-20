import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigList,createLongTextRow,createSelectRow,createTextRow} from '../../../../shared/config-rows/config-rows.js';

/** Agent-specific coordination settings and text overrides. Config: config/configuration/prompts/main_chat.json supplies the defaults shown in grey; the host resolves effective texts. */
export async function renderMainChat(host, request, {agentId,canManage=false}={}) {
  if(!agentId)throw Error('Agent identity is required');
  const path='/models/agents/'+encodeURIComponent(agentId)+'/main-chat';
  const {section,body,toggle:enabled}=createSubpage(host,{id:'main-chat',title:'Main Chat',toggleLabel:'Enable Main Chat'});
  section.classList.add('main-chat-settings');section.dataset.agentId=agentId;enabled.disabled=true;
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Applies to all sessions for this agent. Changes apply after Save. Main Chat uses the same context settings as this agent’s other chats.';body.append(hint);
  const size=document.createElement('p');size.className='ac-hint main-chat-notes-size';body.append(size);
  const form=document.createElement('form');body.append(form);
  // Rows only collect edits here: nothing saves until the explicit Save below, which also guards deactivation.
  const list=createConfigList(form,{label:'Main Chat settings'});
  const fields=new Map(),editors=new Map(),fallbacks={};
  const text=(key,title)=>{const row=createTextRow(list,{title,maxLength:200});row.control.required=true;row.control.dataset.mainChatField=key;fields.set(key,row);};
  const choice=(key,title,options)=>{const row=createSelectRow(list,{title,options});row.control.dataset.mainChatField=key;fields.set(key,row);};
  text('chat_name','Main Chat name');text('notes_name','Main Notes name');
  choice('write_mode','Notes write approval',[['automatic','Automatic writes and edits (default)'],['new_topics','Approve new topics and scope changes'],['every_write','Approve every write or edit']]);
  choice('schedule_approval','Scheduling approval',[['tool','Follow the tool permission and chat mode (default)'],['each','Approve each schedule'],['recurring','Approve recurring schedules only'],['free','Schedule freely']]);
  // Texts from prompts/main_chat.json; {chat} and {notes} become the two names above.
  for(const [key,title,description]of [['contribute','Notes guidance','Sent ahead of the notes in this agent’s working sessions while enabled.'],['review','Main Chat prompt','Sent only in this agent’s Main Chat while enabled.']]){
    const row=createLongTextRow(list,{title,hint:description+' Leave empty and Save to use the default from main_chat.json, shown in grey.',minHeight:160});
    row.row.dataset.mainChatSkill=key;row.editor.area.dataset.mainChatSkillInput=key;
    const warning=document.createElement('p');warning.className='main-chat-skill-warning';warning.style.color='var(--danger, #dc2626)';warning.setAttribute('role','alert');warning.textContent='Fallback skill is not available. Enter an agent-specific override or ask the app administrator to update main_chat.json.';warning.hidden=true;
    row.row.append(warning);editors.set(key,{row,warning});fallbacks[key]='';
  }
  const policy=document.createElement('p');policy.className='ac-hint';policy.textContent='Notes read/write tools remain independently available through Base and their tool permissions. The toggle controls coordination guidance and follow-ups. Saving notes or editing skills never authorizes execution. Disabling retains this agent’s chat and notes.';form.append(policy);
  const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn ac-btn-ghost';save.textContent='Save Main Chat';save.disabled=true;
  const reload=document.createElement('button');reload.type='button';reload.className='ac-btn ac-btn-ghost';reload.textContent='Reload settings';
  const open=document.createElement('a');open.className='ac-btn ac-btn-ghost';open.textContent='Open Main Chat';open.hidden=true;
  const confirmation=document.createElement('div');confirmation.className='main-chat-deactivation';confirmation.hidden=true;confirmation.setAttribute('role','alertdialog');confirmation.setAttribute('aria-labelledby','main-chat-deactivate-title-'+agentId);confirmation.style.cssText='border:var(--border-width,1px) solid var(--warning);border-radius:8px;padding:12px;margin:10px 0';
  const warningTitle=document.createElement('h4');warningTitle.id='main-chat-deactivate-title-'+agentId;warningTitle.textContent='Deactivate Main Chat?';
  const warningText=document.createElement('p');warningText.textContent='This Main Chat session will be archived and never reused. This agent’s Main Notes are kept, and coordinated follow-ups pause while disabled. Enabling Main Chat again creates a new Main Chat session that starts from the current notes; to carry anything else over, ask for a handoff in this chat before disabling.';
  const keep=document.createElement('button');keep.type='button';keep.className='ac-btn ac-btn-ghost';keep.textContent='Keep Main Chat';
  const confirm=document.createElement('button');confirm.type='button';confirm.className='ac-btn ac-btn-ghost';confirm.textContent='Confirm deactivation';confirmation.append(warningTitle,warningText,keep,confirm);form.append(confirmation,save,reload,open,status);
  let state=null,pending=null,busy=false,generation=0;
  const alive=()=>section.isConnected;
  const editable=()=>canManage&&state?.can_manage===true;
  /** Freeze uncertain requests, and retain server authority independently of client rendering. */
  function lock(value){enabled.disabled=value||!editable();for(const row of fields.values())row.setDisabled(value||!editable());for(const {row}of editors.values())row.setDisabled(value||!editable());save.disabled=value||!editable();save.hidden=!!state&&!editable();reload.disabled=value;}
  /** Missing guidance is a visible warning only; never disable otherwise available functions. */
  function warnings(){for(const [key,{row,warning}]of editors)warning.hidden=!!row.getValue().trim()||!!fallbacks[key].trim();}
  let repair=false;
  function dirty(){if(!busy&&editable()){status.textContent='Unsaved changes';warnings();}}
  list.addEventListener('input',dirty);list.addEventListener('change',dirty);enabled.addEventListener('change',dirty);
  /** Adopt only the requested agent’s revisioned settings and overrides; defaults never become saved overrides. */
  function paint(data){
    if(data.agent_id!==agentId||!data.settings||!data.skills)throw Error('Agent settings scope mismatch');
    state=data;pending=null;
    enabled.checked=data.settings.enabled===true;
    for(const [key,row]of fields)row.setValue(data.settings[key]||(key==='schedule_approval'?'tool':''));
    for(const [key,{row}]of editors){
      row.setValue(typeof data.skills[key]?.override==='string'?data.skills[key].override:'');
      fallbacks[key]=typeof data.skills[key]?.fallback==='string'?data.skills[key].fallback:'';
      // The editor greys its default text in while the field is empty; it arrives with the settings.
      row.editor.setDefaultText(fallbacks[key]);
    }
    const topics=Object.keys(data.topics||{}).length;
    size.textContent=topics?'Main Notes in every session’s context: '+topics+(topics===1?' topic':' topics')+', about '+Number(data.notes_context_tokens||0).toLocaleString()+' tokens.':'Main Notes are empty. Once agents add notes, every session carries them in its context.';
    open.hidden=!data.session_id;if(data.session_id)open.href='/app?session_id='+encodeURIComponent(data.session_id);warnings();lock(false);save.textContent='Save Main Chat';
  }
  /** Explicit reload reconciles conflicts; detached navigation cannot repaint another agent. */
  async function load(){const epoch=++generation;busy=true;lock(true);status.textContent='Loading Main Chat…';try{const data=await request(path);if(!alive()||epoch!==generation)return;paint(data);status.textContent=data.can_manage===true?'':'Only this agent’s Operators can edit these settings.';repair=data.session_missing===true&&data.settings.enabled===true&&data.can_manage===true;}catch(error){if(alive()&&epoch===generation)status.textContent='Main Chat unavailable: '+error.message;}finally{if(epoch===generation){busy=false;reload.disabled=false;
    // The linked Main Chat session record is gone: save the stored settings once so the server recreates it.
    if(repair&&alive()){status.textContent='Main Chat session was missing. Recreating it…';form.requestSubmit();}}}}
  reload.addEventListener('click',()=>{if(!busy)void load();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(busy||!state||!editable())return;
    if(!pending){
      const settings={enabled:enabled.checked,chat_name:fields.get('chat_name').getValue().trim(),notes_name:fields.get('notes_name').getValue().trim(),write_mode:fields.get('write_mode').getValue(),schedule_approval:fields.get('schedule_approval').getValue()};
      if(!settings.chat_name||!settings.notes_name){status.textContent='Both names are required.';return;}
      if([settings.chat_name,settings.notes_name].some(v=>new TextEncoder().encode(v).length>200)){status.textContent='Each name must fit within 200 UTF-8 bytes.';return;}
      const skill_overrides={};for(const [key,{row}]of editors){const value=row.getValue();if(new TextEncoder().encode(value).length>65536){status.textContent='Each skill must fit within 65,536 UTF-8 bytes.';return;}skill_overrides[key]=value.trim()?value:null;}
      pending={expected_revision:state.revision,expected_session_revision:state.session_metadata_revision,request_id:crypto.randomUUID(),settings,skill_overrides};
    }
    // A second explicit activation approves the frozen deactivation payload; the server must enforce the same transition guard.
    if(state.settings.enabled===true&&pending.settings.enabled===false&&pending.confirm_deactivation!==true){confirmation.hidden=false;lock(true);status.textContent='Confirm deactivation before this change is saved.';keep.focus();return;}
    busy=true;lock(true);status.textContent='Saving Main Chat…';
    try{await request(path,'PUT',pending);if(!alive())return;const latest=await request(path);if(!alive())return;paint(latest);status.textContent=repair?'Main Chat session was missing and has been recreated.':'Main Chat saved. Updated skills apply on the next model call.';repair=false;document.dispatchEvent(new CustomEvent('v3:main-chat-changed',{detail:{agentId}}));window.dispatchEvent(new Event('v3:sessions-changed'));}
    catch(error){if(!alive())return;repair=false;status.textContent='Save not confirmed: '+error.message+'. Reload to reconcile, or retry the same change.';save.textContent='Retry save';save.disabled=false;reload.disabled=false;}
    finally{busy=false;}
  });
  /** Cancel only the deactivation intent; other unsaved name and skill edits stay in the form. */
  function cancelDeactivation(){if(confirmation.hidden||busy)return;pending=null;confirmation.hidden=true;enabled.checked=state.settings.enabled===true;lock(false);status.textContent='Deactivation cancelled. Other changes have not been saved.';enabled.focus();}
  keep.addEventListener('click',cancelDeactivation);
  confirm.addEventListener('click',()=>{if(!pending||busy||confirmation.hidden)return;pending.confirm_deactivation=true;confirmation.hidden=true;form.requestSubmit();});
  confirmation.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();cancelDeactivation();}});
  await load();
}
