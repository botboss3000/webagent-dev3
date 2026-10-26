// Human values travel only to the nonce-bound authenticated credential endpoint.
export function closeCredentialPanel(){document.querySelector('dialog[data-core-credential]')?.close();}
export function openCredentialPanel({request,session,name,origin,requestId,revision}){
  closeCredentialPanel();
  const dialog=document.createElement('dialog');dialog.dataset.coreCredential='true';dialog.className='wa-confirm-panel';
  const title=document.createElement('h2');title.textContent='Private credential: '+name;
  const purpose=document.createElement('p');purpose.textContent='Service: '+origin+'. The agent and mini apps receive status only. This form does not enable network access.';
  const form=document.createElement('form'),label=document.createElement('label');label.textContent='Credential value';
  const input=document.createElement('input');input.type='password';input.autocomplete='off';input.required=true;input.maxLength=8192;input.dataset.secret='true';label.append(input);
  const status=document.createElement('p');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn';save.textContent='Save credential';
  const cancel=document.createElement('button');cancel.type='button';cancel.className='ac-btn';cancel.textContent='Cancel';cancel.onclick=()=>dialog.close();
  form.append(label,status,save,cancel);dialog.append(title,purpose,form);dialog.addEventListener('close',()=>{form.reset();dialog.remove();},{once:true});
  form.onsubmit=async event=>{event.preventDefault();save.disabled=true;status.textContent='Saving…';try{
    await request('/core/vault/'+encodeURIComponent(session)+'/'+encodeURIComponent(name),'POST',{request_id:requestId,expected_revision:revision,value:input.value});
    input.value='';save.hidden=true;cancel.textContent='Close';status.textContent='Saved privately. Tell the agent when you are ready to continue.';
  }catch(error){input.value='';status.textContent=error.message;save.disabled=false;}};
  document.body.append(dialog);dialog.showModal();input.focus();
}
// Sign-in runs only in the first-party popup started by signIn; this panel never collects a password.
export function openSignInPanel({agent,message,identity,signIn,guest}){
  closeCredentialPanel();
  const dialog=document.createElement('dialog');dialog.dataset.coreCredential='true';dialog.className='wa-confirm-panel';
  const title=document.createElement('h2');title.textContent='Sign in to '+agent;
  const purpose=document.createElement('p');purpose.textContent=message;
  const status=document.createElement('p');status.setAttribute('role','status');status.textContent=identity?'Signed in as '+identity:'';
  const actions=document.createElement('div');
  // Run after close so a popup opened by signIn keeps this click's user activation.
  const action=(label,run)=>{const button=document.createElement('button');button.type='button';button.className='ac-btn';button.textContent=label;button.onclick=()=>{dialog.close();run?.();};actions.append(button);return button;};
  const primary=action('Sign in',signIn);if(guest)action('Continue as guest',guest);action('Cancel');
  dialog.append(title,purpose,status,actions);dialog.addEventListener('close',()=>dialog.remove(),{once:true});
  document.body.append(dialog);dialog.showModal();primary.focus();
}
