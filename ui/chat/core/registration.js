// Credentials stay in the human form and the existing authentication API, never tool results.
export function openRegistrationDialog({request}) {
  if(document.querySelector('dialog[data-core-registration]'))return;
  const dialog=document.createElement('dialog');dialog.className='wa-confirm-panel';dialog.dataset.coreRegistration='true';
  const title=document.createElement('h2');title.textContent='Create account';
  const hint=document.createElement('p');hint.textContent='Create a separate account. Your current conversation stays with this account. Registration may require administrator approval.';
  const form=document.createElement('form'),fields={};
  for(const [key,label,type,autocomplete] of [['display_name','Display name','text','name'],['username','Username','text','username'],['password','Password','password','new-password']]){
    const wrapper=document.createElement('label');wrapper.textContent=label;
    const input=document.createElement('input');input.name=key;input.type=type;input.autocomplete=autocomplete;input.required=true;input.maxLength=key==='password'?1024:128;
    if(key==='password'){input.minLength=8;input.dataset.secret='true';}
    wrapper.append(input);form.append(wrapper);fields[key]=input;
  }
  const status=document.createElement('p');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  const submit=document.createElement('button');submit.type='submit';submit.className='ac-btn';submit.textContent='Create account';
  const cancel=document.createElement('button');cancel.type='button';cancel.className='ac-btn';cancel.textContent='Cancel';cancel.onclick=()=>dialog.close();
  form.append(status,submit,cancel);dialog.append(title,hint,form);
  dialog.addEventListener('close',()=>{form.reset();dialog.remove();},{once:true});
  form.onsubmit=async event=>{
    event.preventDefault();submit.disabled=true;status.textContent='Creating account…';
    try{
      const result=await request('/auth/register','POST',{username:fields.username.value.trim(),display_name:fields.display_name.value.trim(),password:fields.password.value});
      fields.password.value='';
      if(result.pending_approval){status.textContent='Account created and waiting for administrator approval.';submit.hidden=true;cancel.textContent='Close';}
      else{form.reset();location.reload();}
    }catch(error){fields.password.value='';status.textContent=error.message;submit.disabled=false;}
  };
  document.body.append(dialog);dialog.showModal();fields.display_name.focus();
}
