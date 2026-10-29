// Administrator popup, top left on every page: ability changes in the live checkout that
// need a rebuild. The server serves it to administrators only and remembers dismissals per
// change set; "Remind me at next login" hides it for this login only.
const LABELS={added_ability:'New ability',removed_ability:'Removed ability',added_tool:'New tool',removed_tool:'Removed tool',invalid_tool:'Invalid tool',changed_schema:'Changed tool',changed_manifest:'Changed manifest',changed_guidance_files:'Guidance files',changed_code:'Changed code'};
const SHOWN=20;

/** Poll the change set and keep `render` showing it until it is dismissed or gone. */
export function createAbilityDrift({request,render,setIntervalFn=setInterval,clearIntervalFn=clearInterval,intervalMs=30000}){
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
  return {
    start(){if(timer!==null)return;void poll();timer=setIntervalFn(()=>void poll(),intervalMs);},
    stop(){if(timer!==null){clearIntervalFn(timer);timer=null;}turn++;current=null;render(null);},
    dismiss,poll,
  };
}

/** The popup element and its renderer; `onDismiss(remind)` receives the checkbox state. */
export function abilityDriftView(doc,onDismiss,vibrate=()=>{}){
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
    const actions=doc.createElement('div');actions.className='ability-drift-actions';actions.append(remind,button);
    box.append(title,intro,list,actions);
  }
  return {element:box,render};
}
