// Builds an agent's reply panel (present_chat_component choice or form) as a card form: round
// single-choice rows (tap a chosen row again to clear it), checkbox rows, an optional "Something
// else…" row, an optional comment box under each pick list (on unless the field sets comment:false),
// text and free-text fields with + inside the box to stage files on the answer, and an
// optional one-question-per-page flow with "N of M" between back/forward chevrons. Dismiss sits at
// the card's top right; the main button reads Continue, Skip (optional page left empty) or Submit.
// The answer only ever goes out as the user's own message, which the transcript leaves out; a panel
// never authorizes anything. An answered panel keeps its answers visible but greyed out and locked,
// with Complete where Submit was.

const OTHER='\u0000other';

/** Makes an element with a class name and optional text. */
function el(tag,className,text){const node=document.createElement(tag);if(className)node.className=className;if(text!=null)node.textContent=text;return node;}

/** Draws a chevron icon pointing left or right. */
function chevron(left){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');const path=document.createElementNS('http://www.w3.org/2000/svg','path');path.setAttribute('d',left?'M15 18l-6-6 6-6':'M9 18l6-6-6-6');path.setAttribute('fill','none');path.setAttribute('stroke','currentColor');path.setAttribute('stroke-width','2');path.setAttribute('stroke-linecap','round');path.setAttribute('stroke-linejoin','round');svg.append(path);return svg;}

/** Formats one answer for the user's message: lists join with commas and an empty answer reads "skipped". */
export function shownAnswer(value){if(Array.isArray(value))return value.length?value.join(', '):'skipped';return value===''||value==null?'skipped':String(value);}

/** Builds the reply text: "Title: value" for one uncommented field, else the title then one "Label: value" line per field, each comment on its own "Label — comment: text" line. */
export function answerText(title,answers){if(answers.length===1&&!answers[0].comment)return title+': '+shownAnswer(answers[0].value);return [title,...answers.flatMap(item=>[item.label+': '+shownAnswer(item.value),...(item.comment?[item.label+' — comment: '+item.comment]:[])])].join('\n');}

/** Collects the run that carried each panel's answer message, which the transcript leaves out. */
export function answeredPanelRuns(components){const runs=new Set();for(const row of Array.isArray(components)?components:[]){const run=row?.component?.lifecycle?.message_run_id;if(typeof run==='string'&&run)runs.add(run);}return runs;}

/** Builds one question: its label, hint, control, inline error, and how to read, check and clear it. */
function question(field,saved,locked,addFiles,key,savedComment){
  const label=typeof field.label==='string'?field.label:field.name;
  const page=el('div','v3-reply-field');
  if(label)page.append(el('div','v3-reply-q',label));
  if(field.hint)page.append(el('p','v3-reply-hint',field.hint));
  const error=el('p','v3-reply-error');error.hidden=true;error.setAttribute('role','alert');
  let read,check,clear,comment=()=>'',clearComment=()=>{};
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
    if(single){
      // Tapping the chosen round option again clears it, so a question can be left undecided.
      let chosen=boxes.find(box=>box.checked)||null;
      for(const box of boxes)box.addEventListener('click',()=>{if(box===chosen){box.checked=false;chosen=null;box.dispatchEvent(new Event('change',{bubbles:true}));}else chosen=box;});
    }
    page.append(list);
    if(field.comment!==false&&(!locked||savedComment)){
      // The comment box sits on the same step as its decision, so a paged form keeps one page per question.
      const box=document.createElement('textarea');box.className='v3-reply-comment';box.rows=2;box.maxLength=1000;box.placeholder='Comments (optional)';box.setAttribute('aria-label',(label||'Answer')+': comments');box.disabled=locked;if(typeof savedComment==='string')box.value=savedComment;
      page.append(box);comment=()=>box.value.trim();clearComment=()=>{box.value='';};
    }
    read=()=>{const values=boxes.filter(box=>box.checked).map(box=>box===otherBox?other.value.trim():box.value).filter(Boolean);return single?values[0]||'':values;};
    check=()=>{if(otherBox?.checked&&!other.value.trim())return 'Type your answer or pick another option.';if(field.required===true&&!boxes.some(box=>box.checked))return single?'Choose one option.':'Choose at least one option.';return '';};
    clear=()=>{for(const box of boxes)box.checked=false;if(other){other.value='';other.hidden=true;}clearComment();};
  }else{
    const wrap=el('div','v3-reply-input');
    let input;
    if(field.type==='select'){input=document.createElement('select');input.append(new Option('Choose…',''));for(const value of Array.isArray(field.options)?field.options:[])input.append(new Option(value,value));}
    else if(field.type==='textarea'){input=document.createElement('textarea');input.rows=3;input.maxLength=2000;}
    else{input=document.createElement('input');input.type=['email','number'].includes(field.type)?field.type:'text';}
    input.setAttribute('aria-label',label||'Answer');input.disabled=locked;if(typeof saved==='string'||typeof saved==='number')input.value=String(saved);
    wrap.append(input);page.append(wrap);
    if(field.attach===true&&!locked&&addFiles){
      // + sits inside the box and stages files exactly like the composer's picker; they go out with the answer message.
      const plus=el('button','v3-reply-attach','+');plus.type='button';plus.setAttribute('aria-label','Attach files');
      const picker=document.createElement('input');picker.type='file';picker.multiple=true;picker.hidden=true;
      const files=el('small','v3-reply-files');files.hidden=true;files.setAttribute('aria-live','polite');
      plus.onclick=()=>picker.click();
      picker.onchange=async()=>{const chosen=[...picker.files];picker.value='';if(!chosen.length)return;plus.disabled=true;try{const count=await addFiles(chosen);files.textContent=count===1?'1 file will be sent with your answer':count+' files will be sent with your answer';files.hidden=!count;page.dispatchEvent(new Event('input',{bubbles:true}));}finally{plus.disabled=false;}};
      wrap.append(plus,picker);page.append(files);
    }
    read=()=>String(input.value).trim();
    check=()=>{if(field.required===true&&!read())return (label||'This answer')+' is required.';if(!input.checkValidity())return input.validationMessage||'Check this answer.';return '';};
    clear=()=>{input.value='';};
  }
  page.append(error);
  const filled=()=>{const value=read();return Array.isArray(value)?value.length>0||comment()!=='':value!==''||comment()!=='';};
  return {field,label:label||field.name,page,read,comment,clear,filled,valid(){const message=check();error.textContent=message;error.hidden=!message;return !message;}};
}

/**
 * Builds the panel's form. fields are server-validated field specs; state is 'answered', 'dismissed'
 * or empty; values holds a recorded answer by field name and comments a recorded comment by field name; dismiss is the panel's Dismiss button;
 * addFiles(files) stages files and resolves with the staged count; send(answers) sends the answer
 * and rejects to keep the panel open. A paged form shows one question per page.
 */
export function buildReplyForm({fields,paged=false,state='',values={},comments={},dismiss,addFiles,send}){
  const form=el('form','v3-reply-form');form.noValidate=true;
  const locked=Boolean(state),key='v3r-'+Math.random().toString(36).slice(2);
  const items=fields.map((field,index)=>question(field,values[field.name],locked,addFiles,key+'-'+index,comments[field.name]));
  for(const item of items)form.append(item.page);
  // An answered panel shows its recorded answer read-only, with Complete where Submit was.
  if(locked){const foot=el('div','v3-reply-foot');if(state==='answered')foot.append(el('span','v3-reply-complete','Complete'));if(foot.childElementCount)form.append(foot);return form;}
  // Dismiss sits at the card's top right, away from the answer buttons.
  dismiss.className='v3-reply-btn is-quiet v3-reply-dismiss';form.prepend(dismiss);
  const foot=el('div','v3-reply-foot');
  const nav=el('div','v3-reply-nav'),step=el('span','v3-reply-step');
  const prev=el('button','v3-reply-chev');prev.type='button';prev.setAttribute('aria-label','Previous question');prev.append(chevron(true));
  const fwd=el('button','v3-reply-chev');fwd.type='button';fwd.setAttribute('aria-label','Next question');fwd.append(chevron(false));
  nav.append(prev,step,fwd);
  const main=el('button','v3-reply-btn');main.type='button';
  const done=el('button','v3-reply-btn is-primary','Submit');done.type='submit';
  foot.append(nav,main,done);form.append(foot);
  const pages=paged&&items.length>1;let index=0;
  // The main button reads Skip while an optional page is empty, otherwise Continue.
  const label=()=>{const item=items[index],skip=!item.filled()&&item.field.required!==true;main.textContent=skip?'Skip':'Continue';main.className='v3-reply-btn'+(skip?'':' is-primary');};
  const show=()=>{const last=!pages||index===items.length-1;items.forEach((item,i)=>{item.page.hidden=pages&&i!==index;});nav.hidden=!pages;step.textContent=(index+1)+' of '+items.length;prev.classList.toggle('is-off',index===0);prev.disabled=index===0;fwd.classList.toggle('is-off',last);fwd.disabled=last;main.hidden=last;done.hidden=!last;if(!last)label();};
  // Moving forward checks the page; an empty optional page counts as skipped.
  const advance=()=>{const item=items[index];if(!item.filled()&&item.field.required!==true){item.clear();item.valid();}else if(!item.valid())return;index++;show();};
  main.onclick=advance;fwd.onclick=advance;
  prev.onclick=()=>{if(index>0){index--;show();}};
  form.addEventListener('change',()=>{if(pages&&index<items.length-1)label();});
  form.addEventListener('input',()=>{if(pages&&index<items.length-1)label();});
  form.onsubmit=async event=>{event.preventDefault();
    // Enter in a one-line field on an earlier page continues instead of sending.
    if(pages&&index<items.length-1){advance();return;}
    const bad=items.findIndex(item=>!item.valid());if(bad>=0){if(pages){index=bad;show();}return;}
    const controls=[prev,fwd,main,done,dismiss];for(const node of controls)node.disabled=true;
    try{await send(items.map(item=>({name:item.field.name,label:item.label,value:item.read(),comment:item.comment()})));}
    catch{for(const node of controls)node.disabled=false;show();}
  };
  show();return form;
}
