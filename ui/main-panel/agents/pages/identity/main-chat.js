// Personal coordination settings are independent of the selected agent identity.
export async function renderMainChat(host, request) {
  const section=document.createElement('section');section.className='main-chat-settings';section.style.webkitTapHighlightColor='transparent';host.append(section);
  const title=document.createElement('h3');title.textContent='Main Chat';section.append(title);
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Your private coordination chat and cross-session notes. These settings apply to your account across agents.';section.append(hint);
  const form=document.createElement('form');section.append(form);
  const fields=new Map();
  function field(key,label,type='text',choices=null){
    const wrap=document.createElement('div');wrap.className='ac-cfg-field';const text=document.createElement('label');text.className='ac-label';text.textContent=label;
    const input=document.createElement(choices?'select':'input');input.id='main-chat-'+key;input.dataset.mainChatField=key;text.htmlFor=input.id;
    if(choices){for(const [value,label] of choices){const option=document.createElement('option');option.value=value;option.textContent=label;input.append(option);}}else input.type=type;
    if(type!=='checkbox')input.className='ac-input';wrap.append(text,input);fields.set(key,input);return wrap;
  }
  form.append(field('enabled','Enable Main Chat','checkbox'));
  const nested=document.createElement('fieldset');nested.style.cssText='border:0;padding:0;margin:0;min-width:0';const legend=document.createElement('legend');legend.textContent='Names and coordination';nested.append(legend);form.append(nested);
  nested.append(field('chat_name','Main Chat name'),field('notes_name','Main Notes name'),field('context_kib','Context limit (KiB)','number'),
    field('write_mode','Notes write approval','text',[['every_write','Approve every write or edit'],['new_topics','Approve new topics and scope changes'],['automatic','Automatic writes and edits']]),
    field('reporting','Updates in Main Chat','text',[['meaningful','Meaningful results and requests'],['progress','Include progress updates']]));
  for(const key of ['chat_name','notes_name']){fields.get(key).required=true;fields.get(key).maxLength=200;}
  Object.assign(fields.get('context_kib'),{min:'1',max:'4096',step:'1',required:true});
  const policy=document.createElement('p');policy.className='ac-hint';policy.textContent='Approved topics have a defined scope. Saving or approving a note does not authorize its actions. Configure individual tool and Automation permissions on Abilities. Tool permissions can still require approval or deny a note write. Disabling pauses coordination and retains your chat and notes. Compact remains available; notes persist separately.';nested.append(policy);
  const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn ac-btn-ghost';save.textContent='Save Main Chat';save.disabled=true;
  const reload=document.createElement('button');reload.type='button';reload.className='ac-btn ac-btn-ghost';reload.textContent='Reload settings';
  const open=document.createElement('a');open.className='ac-btn ac-btn-ghost';open.textContent='Open Main Chat';open.hidden=true;
  form.append(save,reload,open,status);
  let state=null,pending=null,busy=false,generation=0;
  const alive=()=>section.isConnected;
  function lock(value){for(const input of fields.values())input.disabled=value;save.disabled=value;}
  function paint(data){state=data;pending=null;const settings=data.settings;for(const [key,input]of fields){if(key==='enabled')input.checked=settings.enabled;else input.value=key==='context_kib'?settings.context_limit_bytes/1024:settings[key];}open.hidden=!data.session_id;if(data.session_id)open.href='/app?session_id='+encodeURIComponent(data.session_id);lock(false);save.textContent='Save Main Chat';}
  async function load(){const epoch=++generation;busy=true;lock(true);status.textContent='Loading Main Chat…';try{const data=await request('/main-chat');if(!alive()||epoch!==generation)return;paint(data);status.textContent=data.review_status==='automation_unavailable'?'Automation is unavailable. Enable the Automations ability to coordinate reviews.':data.review_status==='paused'?'Review subscription is paused. Review its Automation approval and controls.':'';}catch(error){if(alive()&&epoch===generation)status.textContent='Main Chat unavailable: '+error.message;}finally{if(epoch===generation)busy=false;}}
  reload.addEventListener('click',()=>{if(!busy)void load();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(busy||!state)return;
    if(!pending){const context=Number(fields.get('context_kib').value);if(!Number.isSafeInteger(context)||context<1||context>4096){status.textContent='Enter a whole context limit from 1 to 4,096 KiB.';return;}
      const settings={...state.settings,enabled:fields.get('enabled').checked,chat_name:fields.get('chat_name').value.trim(),notes_name:fields.get('notes_name').value.trim(),context_limit_bytes:context*1024,write_mode:fields.get('write_mode').value,reporting:fields.get('reporting').value};
      if(!settings.chat_name||!settings.notes_name){status.textContent='Both names are required.';return;}
      pending={expected_revision:state.revision,expected_session_revision:state.session_metadata_revision,request_id:crypto.randomUUID(),settings};
    }
    busy=true;lock(true);status.textContent='Saving Main Chat…';
    try{await request('/main-chat','PUT',pending);if(!alive())return;const latest=await request('/main-chat');if(!alive())return;paint(latest);status.textContent='Main Chat saved';document.dispatchEvent(new Event('v3:main-chat-changed'));window.dispatchEvent(new Event('v3:sessions-changed'));}
    catch(error){if(!alive())return;status.textContent='Save not confirmed: '+error.message+'. Reload to reconcile, or retry the same change.';save.textContent='Retry save';save.disabled=false;}
    finally{busy=false;}
  });
  await load();
}
