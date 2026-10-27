import {requestTouchVibration} from '../../shared/vibrate.js';
import {advanceDeleteBtn,resetDeleteBtn} from './pages/sessions/js/delete-control.js';
import {icon} from './pages/sessions/js/adapter.js';

// Like V2: managers tick agent squares, move them to the recycling bin and restore them from it.
// The default agent and agents the caller cannot manage have no checkbox.
const deletable=agent=>agent.can_manage===true&&agent.id!=='shared_default';
const press=button=>{button.type='button';button.style.webkitTapHighlightColor='transparent';button.addEventListener('click',requestTouchVibration);return button;};
const paint=()=>window.lucide?.createIcons();

export function createAgentBin({grid,request,roster,rerender,notify,onChanged,canUseBin}) {
  const selected=new Set();
  let open=false,binned=[];
  const actions=grid.querySelector('#agents-toolbar-actions'),back=grid.querySelector('#agents-back-btn'),restore=grid.querySelector('#agents-restore-btn');
  const remove=press(document.createElement('button'));remove.id='agents-delete-btn';remove.className='agents-tb-btn agents-tb-delete';
  const bin=press(document.createElement('button'));bin.id='agents-bin-btn';bin.className='agents-tb-btn';bin.title='Open the recycling bin';
  bin.innerHTML=icon('archive',{size:'15px'})+'<span>Bin</span>';
  actions.append(remove,bin);press(back);press(restore);
  const resetRemove=()=>resetDeleteBtn(remove,{size:'15px',title:'Move selected agents to the recycling bin',label:'Delete selected agents'});
  resetRemove();
  const visible=()=>open?binned:roster();
  function sync(){
    for(const id of [...selected])if(!visible().some(agent=>agent.id===id))selected.delete(id);
    remove.hidden=open||!selected.size;bin.hidden=open||!canUseBin();
    back.style.display=open?'inline-flex':'none';restore.style.display=open?'inline-flex':'none';restore.disabled=!selected.size;
    paint();
  }
  function toggle(id,row){
    if(selected.has(id))selected.delete(id);else selected.add(id);
    row.classList.toggle('selected',selected.has(id));
    row.querySelector('.agent-select-box')?.setAttribute('aria-checked',String(selected.has(id)));
    if(!selected.size)resetRemove();
    sync();
  }
  function decorate(row,agent){
    if(!deletable(agent))return;
    const box=press(document.createElement('button'));box.className='agent-select-box';
    box.setAttribute('role','checkbox');box.setAttribute('aria-checked',String(selected.has(agent.id)));box.setAttribute('aria-label','Select '+(agent.name||agent.id));
    box.innerHTML=icon('check',{size:'13px'});
    box.addEventListener('click',event=>{event.stopPropagation();toggle(agent.id,row);});
    box.addEventListener('keydown',event=>event.stopPropagation());
    row.classList.toggle('selected',selected.has(agent.id));
    row.firstElementChild.style.position='relative';row.firstElementChild.append(box);
  }
  function render(squares){
    for(const agent of binned){
      const row=document.createElement('div');row.className='agent-row';row.dataset.agentId=agent.id;
      const card=document.createElement('div');card.className='agent-card agent-square';
      card.innerHTML='<div class="agent-card-top"><div class="agent-card-icon-wrap color-purple">'+icon('archive',{size:'24px'})+'</div><div class="agent-card-meta"><div class="agent-card-name-row"><span class="agent-card-name"></span></div></div></div>';
      card.querySelector('.agent-card-name').textContent=agent.name||agent.id;
      if(agent.description)card.title=agent.description;
      row.append(card);decorate(row,agent);card.onclick=()=>toggle(agent.id,row);squares.append(row);
    }
    if(!binned.length){const empty=document.createElement('p');empty.className='ac-hint';empty.textContent='The recycling bin is empty.';squares.append(empty);}
    sync();
  }
  async function loadBin(){binned=(await request('/models/agents?deleted=true')).agents||[];}
  async function setOpen(value){
    if(value)await loadBin();
    open=value;selected.clear();resetRemove();grid.classList.toggle('agents-bin-view',open);rerender();
  }
  // One request per agent, fenced by the revision the square was rendered with; failures stay selected.
  async function apply(path,verb){
    const agents=visible().filter(agent=>selected.has(agent.id));
    const results=await Promise.allSettled(agents.map(agent=>request('/models/agents/'+encodeURIComponent(agent.id)+path,path?'POST':'DELETE',{expected_revision:agent.revision})));
    const done=agents.filter((_,index)=>results[index].status==='fulfilled').map(agent=>agent.id);
    const failed=results.map((result,index)=>result.status==='rejected'?(agents[index].name||agents[index].id)+': '+result.reason.message:'').filter(Boolean);
    selected.clear();agents.filter(agent=>!done.includes(agent.id)).forEach(agent=>selected.add(agent.id));
    notify(failed.length?'Not '+verb+' — '+failed.join('; '):'');
    if(open)await loadBin();
    await onChanged(done,verb);
    rerender();
    return done;
  }
  remove.addEventListener('click',()=>advanceDeleteBtn(remove,{size:'15px',armTitle:'Click again to move the selected agents to the recycling bin',armLabel:'Confirm deleting selected agents',busyLabel:'Deleting selected agents',onConfirm:async()=>{
    try{await apply('','deleted');}catch(error){notify('Not deleted — '+error.message);}finally{resetRemove();sync();}
  }}));
  restore.addEventListener('click',async()=>{
    if(!selected.size)return;restore.disabled=true;
    try{await apply('/restore','restored');}catch(error){notify('Not restored — '+error.message);}finally{sync();}
  });
  bin.addEventListener('click',()=>setOpen(true).catch(error=>notify('Recycling bin unavailable: '+error.message)));
  back.addEventListener('click',()=>setOpen(false));
  return {get open(){return open;},decorate,render,sync};
}
