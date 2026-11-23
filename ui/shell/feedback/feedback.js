// Send feedback: the account menu's form, which the server forwards to the feedback relay as an issue.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';

// Wire the form inside the account menu; returns the switch the UI config drives and a reset for menu close.
export function mountFeedback(menu,api){
  const $=id=>document.getElementById(id);
  const open=$('send-feedback'),form=$('feedback-view'),body=$('feedback-body'),email=$('feedback-email'),status=$('feedback-status'),send=$('feedback-send'),count=$('feedback-count');
  let closeTimer=0;
  const say=(text,kind='')=>{status.textContent=text;status.dataset.kind=kind;};
  function reset(){
    clearTimeout(closeTimer);
    menu.classList.remove('feedback-open');form.hidden=true;form.reset();count.textContent='0';say('');
  }
  open.onclick=event=>{
    requestTouchVibration(event);event.stopPropagation();
    menu.classList.add('feedback-open');form.hidden=false;say('');
    setTimeout(()=>body.focus(),50);
  };
  $('feedback-cancel').onclick=event=>{requestTouchVibration(event);reset();};
  body.addEventListener('input',()=>{count.textContent=String(body.value.length);});
  form.onsubmit=async event=>{
    event.preventDefault();
    if(!body.value.trim()){say('Describe your feedback before sending.','error');return;}
    send.disabled=true;say('Sending…');
    try{
      await api('/feedback','POST',{type:form.elements['feedback-type'].value,body:body.value,email:email.value.trim()||null});
      say('Thanks — your feedback was received.','success');
      closeTimer=setTimeout(reset,1400);
    }catch(error){say(error.message||'Could not send feedback. Please try again.','error');}
    finally{send.disabled=false;}
  };
  send.addEventListener('click',requestTouchVibration);
  return {setEnabled(enabled){open.hidden=!enabled;if(!enabled&&!form.hidden)reset();},reset};
}
