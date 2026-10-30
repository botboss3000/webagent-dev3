import {requestTouchVibration} from '../../shared/vibrate.js';
import {advanceDeleteBtn,resetDeleteBtn} from './pages/sessions/js/delete-control.js';
import {icon} from './pages/sessions/js/adapter.js';

// HARMONY-AGENTS-ARCHIVE — users tick agent squares and Remove them from their list: the owner's Remove
// archives the agent, anyone else's leaves it (and its member list). The owner's archived view unarchives
// or permanently deletes selected agents (agent, sessions and data). The default agent has no checkbox.
const removable=agent=>agent.id!=='shared_default';
const press=button=>{button.type='button';button.style.webkitTapHighlightColor='transparent';button.addEventListener('click',requestTouchVibration);return button;};
const paint=()=>window.lucide?.createIcons();

export function createAgentArchive({grid,request,roster,rerender,notify,onChanged,canUseArchive}) {
  const selected=new Set();
  let open=false,archived=[];
  const actions=grid.querySelector('#agents-toolbar-actions'),back=grid.querySelector('#agents-back-btn');
  // Built here, like its siblings, so the toolbar never depends on the shared agents.html revision.
  const unarchive=press(document.createElement('button'));unarchive.id='agents-unarchive-btn';unarchive.className='agents-tb-btn agents-tb-unarchive';unarchive.title='Unarchive selected agents';unarchive.style.display='none';
  unarchive.innerHTML=icon('archive-restore',{size:'15px'})+'<span>Unarchive</span>';
  const archive=press(document.createElement('button'));archive.id='agents-remove-btn';archive.className='agents-tb-btn agents-tb-remove';
  const remove=press(document.createElement('button'));remove.id='agents-delete-permanent-btn';remove.className='agents-tb-btn agents-tb-delete';
  const view=press(document.createElement('button'));view.id='agents-archive-view-btn';view.className='agents-tb-btn';view.title='Show archived agents';
  view.innerHTML=icon('archive',{size:'15px'})+'<span>Archived</span>';
  actions.append(unarchive,archive,remove,view);press(back);
  const resetRemoveFromList=()=>resetDeleteBtn(archive,{size:'15px',iconName:'circle-minus',title:'Remove selected agents from your list',label:'Remove selected agents'});
  const resetRemove=()=>resetDeleteBtn(remove,{size:'15px',iconName:'trash-2',title:'Delete selected agents permanently',label:'Delete selected agents permanently'});
  resetRemoveFromList();resetRemove();
  const visible=()=>open?archived:roster();
  function sync(){
    for(const id of [...selected])if(!visible().some(agent=>agent.id===id))selected.delete(id);
    archive.hidden=open||!selected.size;remove.hidden=!open||!selected.size;view.hidden=open||!canUseArchive();
    back.style.display=open?'inline-flex':'none';unarchive.style.display=open?'inline-flex':'none';unarchive.disabled=!selected.size;
    paint();
  }
  function toggle(id,row){
    if(selected.has(id))selected.delete(id);else selected.add(id);
    row.classList.toggle('selected',selected.has(id));
    row.querySelector('.agent-select-box')?.setAttribute('aria-checked',String(selected.has(id)));
    if(!selected.size){resetRemoveFromList();resetRemove();}
    sync();
  }
  function decorate(row,agent){
    if(!removable(agent))return;
    const box=press(document.createElement('button'));box.className='agent-select-box';
    box.setAttribute('role','checkbox');box.setAttribute('aria-checked',String(selected.has(agent.id)));box.setAttribute('aria-label','Select '+(agent.name||agent.id));
    box.innerHTML=icon('check',{size:'13px'});
    box.addEventListener('click',event=>{event.stopPropagation();toggle(agent.id,row);});
    box.addEventListener('keydown',event=>event.stopPropagation());
    row.classList.toggle('selected',selected.has(agent.id));
    row.firstElementChild.style.position='relative';row.firstElementChild.append(box);
  }
  function render(squares){
    for(const agent of archived){
      const row=document.createElement('div');row.className='agent-row';row.dataset.agentId=agent.id;
      const card=document.createElement('div');card.className='agent-card agent-square';
      card.innerHTML='<div class="agent-card-top"><div class="agent-card-icon-wrap color-purple">'+icon('archive',{size:'24px'})+'</div><div class="agent-card-meta"><div class="agent-card-name-row"><span class="agent-card-name"></span></div></div></div>';
      card.querySelector('.agent-card-name').textContent=agent.name||agent.id;
      if(agent.description)card.title=agent.description;
      row.append(card);decorate(row,agent);card.onclick=()=>toggle(agent.id,row);squares.append(row);
    }
    if(!archived.length){const empty=document.createElement('p');empty.className='ac-hint';empty.textContent='No archived agents.';squares.append(empty);}
    sync();
  }
  async function loadArchived(){archived=(await request('/models/agents?archived=true')).agents||[];}
  async function setOpen(value){
    if(value)await loadArchived();
    open=value;selected.clear();resetRemoveFromList();resetRemove();grid.classList.toggle('agents-archive-view',open);rerender();
  }
  // One request per agent, fenced by the revision the square was rendered with; failures stay selected.
  async function apply(method,path,verb){
    const agents=visible().filter(agent=>selected.has(agent.id));
    const results=await Promise.allSettled(agents.map(agent=>request('/models/agents/'+encodeURIComponent(agent.id)+path,method,{expected_revision:agent.revision})));
    const done=agents.filter((_,index)=>results[index].status==='fulfilled').map(agent=>agent.id);
    const failed=results.map((result,index)=>result.status==='rejected'?(agents[index].name||agents[index].id)+': '+result.reason.message:'').filter(Boolean);
    selected.clear();agents.filter(agent=>!done.includes(agent.id)).forEach(agent=>selected.add(agent.id));
    notify(failed.length?'Not '+verb+' — '+failed.join('; '):'');
    if(open)await loadArchived();
    await onChanged(done,verb);
    rerender();
    return done;
  }
  archive.addEventListener('click',()=>advanceDeleteBtn(archive,{size:'15px',iconName:'circle-minus',armTitle:'Click again to remove the selected agents from your list',armLabel:'Confirm removing selected agents',busyTitle:'Removing…',busyLabel:'Removing selected agents',onConfirm:async()=>{
    try{await apply('POST','/remove','removed');}catch(error){notify('Not removed — '+error.message);}finally{resetRemoveFromList();sync();}
  }}));
  remove.addEventListener('click',()=>advanceDeleteBtn(remove,{size:'15px',iconName:'trash-2',armTitle:'Click again to permanently delete the selected agents',armLabel:'Confirm permanently deleting selected agents, their sessions and data',busyTitle:'Deleting…',busyLabel:'Deleting selected agents permanently',onConfirm:async()=>{
    try{await apply('DELETE','','deleted');}catch(error){notify('Not deleted — '+error.message);}finally{resetRemove();sync();}
  }}));
  unarchive.addEventListener('click',async()=>{
    if(!selected.size)return;unarchive.disabled=true;
    try{await apply('POST','/unarchive','unarchived');}catch(error){notify('Not unarchived — '+error.message);}finally{sync();}
  });
  view.addEventListener('click',()=>setOpen(true).catch(error=>notify('Archived agents unavailable: '+error.message)));
  back.addEventListener('click',()=>setOpen(false));
  return {get open(){return open;},decorate,render,sync};
}
