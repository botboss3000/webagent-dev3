// Administrator popup, top left on every page: ability changes in the live checkout that
// need a rebuild. The server serves it to administrators only and remembers dismissals per
// change set; "Remind me at next login" hides it for this login only.
const LABELS={added_ability:'New ability',removed_ability:'Removed ability',added_tool:'New tool',removed_tool:'Removed tool',invalid_tool:'Invalid tool',changed_schema:'Changed tool',changed_manifest:'Changed manifest',changed_guidance_files:'Guidance files',changed_code:'Changed code'};
const SHOWN=20;

/** Poll the change set and keep `render` showing it until it is dismissed or gone. */
export function createAbilityDrift({request,render,setIntervalFn=setInterval,clearIntervalFn=clearInterval,intervalMs=30000,uuid=()=>crypto.randomUUID()}){
  let timer=null,current=null,turn=0;
  async function poll(){
    const mine=++turn;
    try{const value=await request('/admin/ability-drift');if(mine!==turn)return;current=value?.show&&value.digest?value:null;render(current);}catch{}
  }
  // Hide at once; a refused or failed dismissal shows the current change set again.
  async function dismiss(remind){
    if(!current)return;
    const digest=current.digest;current=null;turn++;render(null);
    try{await request('/admin/ability-drift/dismiss','POST',{digest,remind_next_login:!!remind});}catch{await poll();}
  }
  // One request id per press; the broker returns the same job when a press is retried.
  async function rebuild(mode){
    return request('/admin/ability-drift/rebuild','POST',{mode,request_id:uuid()});
  }
  return {
    start(){if(timer!==null)return;void poll();timer=setIntervalFn(()=>void poll(),intervalMs);},
    stop(){if(timer!==null){clearIntervalFn(timer);timer=null;}turn++;current=null;render(null);},
    dismiss,poll,rebuild,
  };
}

const REBUILDS=[
  ['dev','Rebuild packages','Dev release: rebuilds the changed abilities; the app keeps running.'],
  ['stable','Stable release','Full rebuild linking every ability; the app restarts briefly.'],
];

/** The popup element and its renderer; `onDismiss(remind)` receives the checkbox state and
 * `onRebuild(mode)` resolves to the broker's reply. */
export function abilityDriftView(doc,onDismiss,vibrate=()=>{},onRebuild=null){
  const box=doc.createElement('section');
  box.id='ability-drift';box.hidden=true;box.setAttribute('role','alertdialog');box.setAttribute('aria-labelledby','ability-drift-title');
  function render(value){
    box.replaceChildren();box.hidden=!value;
    if(!value)return;
    const title=doc.createElement('h2');title.id='ability-drift-title';title.textContent='Rebuild needed';
    const intro=doc.createElement('p');intro.textContent='These ability changes in the live checkout take effect after a rebuild.';
    const list=doc.createElement('ul');
    for(const notice of value.notices.slice(0,SHOWN)){
      const item=doc.createElement('li'),label=doc.createElement('strong'),detail=doc.createElement('div');
      label.textContent=(LABELS[notice.change]||'Changed')+': ';
      detail.textContent=notice.detail;
      item.append(label,notice.ability+(notice.tool?' · '+notice.tool:''),detail);list.append(item);
    }
    if(value.notices.length>SHOWN){const more=doc.createElement('li');more.textContent='and '+(value.notices.length-SHOWN)+' more';list.append(more);}
    const remind=doc.createElement('label'),box_=doc.createElement('input');
    box_.type='checkbox';box_.checked=true;remind.append(box_,' Remind me at next login');
    const button=doc.createElement('button');button.type='button';button.textContent='Dismiss';
    button.addEventListener('click',()=>{vibrate();void onDismiss(box_.checked);});
    const actions=doc.createElement('div');actions.className='ability-drift-actions';actions.append(remind);
    if(onRebuild){
      const rebuild=doc.createElement('button');rebuild.type='button';rebuild.textContent='Rebuild…';
      rebuild.addEventListener('click',()=>{vibrate();confirmRebuild();});
      actions.append(rebuild);
    }
    actions.append(button);
    box.append(title,intro,list,actions);
  }
  // Confirm first: the broker releases the whole live checkout as it is.
  function confirmRebuild(){
    const panel=doc.createElement('div');panel.className='ability-drift-confirm';
    const warning=doc.createElement('p');
    warning.textContent='This releases the live checkout as it is, including any other uncommitted edits in it.';
    panel.append(warning);
    for(const [mode,label,detail] of REBUILDS){
      const choice=doc.createElement('button');choice.type='button';choice.textContent=label;choice.title=detail;
      const note=doc.createElement('small');note.textContent=detail;
      choice.addEventListener('click',()=>{vibrate();void start(mode,panel);});
      panel.append(choice,note);
    }
    const cancel=doc.createElement('button');cancel.type='button';cancel.textContent='Cancel';
    cancel.addEventListener('click',()=>{vibrate();panel.remove();});
    panel.append(cancel);
    box.querySelector('.ability-drift-confirm')?.remove();box.append(panel);
  }
  async function start(mode,panel){
    const status=doc.createElement('p');status.className='ability-drift-status';status.textContent='Starting the '+mode+' rebuild…';
    panel.replaceWith(status);
    try{
      const reply=await onRebuild(mode);
      status.textContent=reply?.status==='no_changes'?'Nothing to rebuild: the checkout matches the running release.'
        :'Rebuild '+(reply?.job_id?'job '+reply.job_id+' ':'')+'queued. Follow it on the Builds page.';
    }catch(error){status.textContent='Rebuild refused: '+(error?.message||'unknown error');}
  }
  return {element:box,render};
}
