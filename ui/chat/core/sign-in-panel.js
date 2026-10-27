// Sign-in runs only in the first-party popup started by signIn; this panel never collects a password.
export function openSignInPanel({agent,message,identity,signIn,guest}){
  document.querySelector('dialog[data-sign-in]')?.close();
  const dialog=document.createElement('dialog');dialog.dataset.signIn='true';dialog.className='wa-confirm-panel';
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
