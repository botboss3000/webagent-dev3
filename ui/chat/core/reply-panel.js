// Builds an agent's reply panel (present_chat_component choice or form) as a card form: round
// single-choice rows, checkbox rows, an optional "Something else…" row, text and free-text fields
// whose + stages files on the answer, and an optional one-question-per-page flow with a step
// counter, Back, Skip and Continue. Every open panel ends with Submit and Dismiss. The answer only
// ever goes out as the user's own message; a panel never authorizes anything.

const OTHER='\u0000other';

/** Makes an element with a class name and optional text. */
function el(tag,className,text){const node=document.createElement(tag);if(className)node.className=className;if(text!=null)node.textContent=text;return node;}

/** Formats one answer for the user's message: lists join with commas and an empty answer reads "skipped". */
export function shownAnswer(value){if(Array.isArray(value))return value.length?value.join(', '):'skipped';return value===''||value==null?'skipped':String(value);}

/** Builds the reply text: "Title: value" for one field, else the title then one "Label: value" line per field. */
export function answerText(title,answers){return answers.length===1?title+': '+shownAnswer(answers[0].value):[title,...answers.map(item=>item.label+': '+shownAnswer(item.value))].join('\n');}

/** Builds one question: its label, hint, control, inline error, and how to read, check and clear it. */
function question(field,saved,locked,addFiles,key){
  const label=typeof field.label==='string'?field.label:field.name;
  const page=el('div','v3-reply-field');
  if(label)page.append(el('div','v3-reply-q',label));
  if(field.hint)page.append(el('p','v3-reply-hint',field.hint));
  const error=el('p','v3-reply-error');error.hidden=true;error.setAttribute('role','alert');
  let read,check,clear;
  if(field.type==='radio'||field.type==='multi'){
    const single=field.type==='radio',options=(Array.isArray(field.options)?field.options:[]).slice(0,8);
    const picked=Array.isArray(saved)?saved:typeof saved==='string'&&saved?[saved]:[];
    const list=el('div','v3-reply-options');list.setAttribute('role',single?'radiogroup':'group');list.setAttribute('aria-label',label||'Answer');
    const boxes=[];
    const row=(text,value)=>{const option=el('label','v3-reply-option');const box=document.createElement('input');box.type=single?'radio':'checkbox';box.name=key;box.value=value;box.disabled=locked;const mark=el('span','v3-reply-mark');mark.setAttribute('aria-hidden','true');option.append(box,mark,el('span','v3-reply-option-text',text));list.append(option);boxes.push(box);return box;};
    for(const value of options)row(value,value).checked=picked.includes(value);
    let other=null,otherBox=null;
    if(field.other){
      // "Something else…" reveals a one-line text box; its text is the answer for that row.
      const extra=picked.filter(value=>!options.includes(value));
      otherBox=row('Something else…',OTHER);
      other=document.createElement('input');other.type='text';other.className='v3-reply-other';other.maxLength=200;other.placeholder='Type your answer';other.setAttribute('aria-label',(label||'Answer')+': something else');other.disabled=locked;
      if(extra.length){otherBox.checked=true;other.value=extra.join(', ');}
      other.hidden=!otherBox.checked;list.append(other);
      for(const box of boxes)box.addEventListener('change',()=>{other.hidden=!otherBox.checked;if(box===otherBox&&otherBox.checked)other.focus();});
    }
    page.append(list);
    read=()=>{const values=boxes.filter(box=>box.checked).map(box=>box===otherBox?other.value.trim():box.value).filter(Boolean);return single?values[0]||'':values;};
    check=()=>{if(otherBox?.checked&&!other.value.trim())return 'Type your answer or pick another option.';if(field.required===true&&!boxes.some(box=>box.checked))return single?'Choose one option.':'Choose at least one option.';return '';};
    clear=()=>{for(const box of boxes)box.checked=false;if(other){other.value='';other.hidden=true;}};
  }else{
    const wrap=el('div','v3-reply-input');
    let input;
    if(field.type==='select'){input=document.createElement('select');for(const value of Array.isArray(field.options)?field.options:[])input.append(new Option(value,value));}
    else if(field.type==='textarea'){input=document.createElement('textarea');input.rows=3;input.maxLength=2000;}
    else{input=document.createElement('input');input.type=['email','number'].includes(field.type)?field.type:'text';}
    input.setAttribute('aria-label',label||'Answer');input.disabled=locked;if(typeof saved==='string'||typeof saved==='number')input.value=String(saved);
    wrap.append(input);page.append(wrap);
    if(field.attach===true&&!locked&&addFiles){
      // + stages files exactly like the composer's picker; they go out with the answer message.
      const plus=el('button','v3-reply-attach','+');plus.type='button';plus.setAttribute('aria-label','Attach files');
      const picker=document.createElement('input');picker.type='file';picker.multiple=true;picker.hidden=true;
      const files=el('small','v3-reply-files');files.hidden=true;files.setAttribute('aria-live','polite');
      plus.onclick=()=>picker.click();
      picker.onchange=async()=>{const chosen=[...picker.files];picker.value='';if(!chosen.length)return;plus.disabled=true;try{const count=await addFiles(chosen);files.textContent=count===1?'1 file will be sent with your answer':count+' files will be sent with your answer';files.hidden=!count;}finally{plus.disabled=false;}};
      wrap.append(plus,picker);page.append(files);
    }
    read=()=>String(input.value).trim();
    check=()=>{if(field.required===true&&!read())return (label||'This answer')+' is required.';if(!input.checkValidity())return input.validationMessage||'Check this answer.';return '';};
    clear=()=>{input.value='';};
  }
  page.append(error);
  return {field,label:label||field.name,page,read,clear,valid(){const message=check();error.textContent=message;error.hidden=!message;return !message;}};
}

/**
 * Builds the panel's form. fields are server-validated field specs; state is 'answered', 'dismissed'
 * or empty; values holds a recorded answer by field name; dismiss is the panel's Dismiss button;
 * addFiles(files) stages files and resolves with the staged count; send(answers) sends the answer
 * and rejects to keep the panel open. A paged form shows one question per page.
 */
export function buildReplyForm({fields,paged=false,state='',values={},dismiss,addFiles,send}){
  const form=el('form','v3-reply-form');form.noValidate=true;
  const locked=Boolean(state),key='v3r-'+Math.random().toString(36).slice(2);
  const items=fields.map((field,index)=>question(field,values[field.name],locked,addFiles,key+'-'+index));
  for(const item of items)form.append(item.page);
  if(locked)return form;
  const button=(text,kind)=>{const node=el('button','v3-reply-btn '+kind,text);node.type='button';return node;};
  const foot=el('div','v3-reply-foot'),step=el('span','v3-reply-step');
  const back=button('Back','is-quiet'),skip=button('Skip','is-quiet'),next=button('Continue','is-primary'),done=button('Submit','is-primary');done.type='submit';
  dismiss.className='v3-reply-btn is-quiet v3-reply-dismiss';
  foot.append(step,back,dismiss,skip,next,done);form.append(foot);
  const pages=paged&&items.length>1;let index=0;
  const show=()=>{const last=!pages||index===items.length-1;items.forEach((item,i)=>{item.page.hidden=pages&&i!==index;});step.hidden=!pages;step.textContent='Step '+(index+1)+' of '+items.length;back.hidden=!pages||index===0;skip.hidden=!pages||last||items[index].field.required===true;next.hidden=last;done.hidden=!last;};
  next.onclick=()=>{if(!items[index].valid())return;index++;show();};
  back.onclick=()=>{index--;show();};
  // Skip leaves the question unanswered and moves on.
  skip.onclick=()=>{items[index].clear();items[index].valid();index++;show();};
  form.onsubmit=async event=>{event.preventDefault();
    // Enter in a one-line field on an earlier page continues instead of sending.
    if(pages&&index<items.length-1){next.click();return;}
    const bad=items.findIndex(item=>!item.valid());if(bad>=0){if(pages){index=bad;show();}return;}
    const controls=[back,skip,next,done,dismiss];for(const node of controls)node.disabled=true;
    try{await send(items.map(item=>({name:item.field.name,label:item.label,value:item.read()})));}
    catch{for(const node of controls)node.disabled=false;}
  };
  show();return form;
}
