// V3 transport and projection boundary for the reference Sessions renderer.
export const _esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
export const _escAttr = _esc;
export const icon = (name,{size='14px'}={}) => `<i data-lucide="${_esc(name)}" style="width:${_esc(size)};height:${_esc(size)}" aria-hidden="true"></i>`;
export const claudeMark = options => icon('bot',options);
export const codexMark = claudeMark;
export const ICON_PICKER_ICONS = ['bot','message-square','cpu','sparkles','code-2'];
export const _formatRelativeTime = value => {
  const timestamp = typeof value==='number' ? value*1000 : Date.parse(value);
  if (!Number.isFinite(timestamp)||timestamp<=0) return 'Unknown';
  const seconds=Math.max(0,Math.floor((Date.now()-timestamp)/1000));
  for(const [unit,scale] of [['w',604800],['d',86400],['h',3600],['m',60]]) if(seconds>=scale)return Math.floor(seconds/scale)+unit+' ago';
  return seconds+'s ago';
};
export const _fmtTime = value => value ? _formatRelativeTime(value) : '—';
export const _isSessionRead = row => !row.is_unread;
export function positionKebabPopup(popup,button){const r=button.getBoundingClientRect();popup.style.position='fixed';popup.style.zIndex='10000';popup.style.top=Math.max(4,Math.min(r.bottom,innerHeight-popup.offsetHeight-4))+'px';popup.style.left=Math.max(4,Math.min(r.right-popup.offsetWidth,innerWidth-popup.offsetWidth-4))+'px';}
export const compareSessionsByPinnedOrder = (a,b) => Number(!!b.pinned&&b._origin!=='bin')-Number(!!a.pinned&&a._origin!=='bin')||(b.last_active_at||0)-(a.last_active_at||0)||String(a.id).localeCompare(String(b.id));
export function createSessionAdapter({request, roster, userId, changed, isAlive=()=>true}) {
  const receipts=new Map(), rows=new Map();
  const iso=seconds=>seconds>0?new Date(seconds*1000).toISOString():null;
  function project(item){
    const agent=roster.find(a=>a.id===item.agent_id),usage=item.usage;
    const recorded=(usage?.reported_calls||0)>0;
    const partial=(usage?.unreported_calls||0)>0;
    const row={...item,session_id:item.id,_rowType:'session',agent_name:agent?.name||item.agent_id,agent_icon:agent?.icon||'bot',last_active:iso(item.last_active_at),created_at:iso(item.created_at),_origin:item.status==='recycled'?'bin':'active',total_input_tokens:recorded?usage.input_tokens:null,total_output_tokens:recorded?usage.output_tokens:null,total_cost:recorded&&usage.unknown_cost_calls===0&&!partial?usage.known_cost_usd:null,total_duration_ms:null,usage_note:!usage?'Usage unavailable; historical calls may predate recording':partial?'Partial provider-reported token totals; some calls have no usage':'Provider-reported token totals',run_status:({completed:'complete',failed:'error'})[item.run_status]||item.run_status};
    rows.set(item.id,row);return row;
  }
  async function catalog(endpoint){let offset=0,result=[];do{const data=await request(endpoint+'&limit=100&offset='+offset);if(!isAlive())return [];result.push(...data.sessions.map(project));if(data.next_offset==null)break;if(data.next_offset<=offset)throw Error('Invalid statistics continuation');offset=data.next_offset;}while(isAlive());return result;}
  async function mutate(id,action,fields={}){
    if(!isAlive())throw Error('Sessions page closed');
    const row=rows.get(id);if(!row)throw Error('Refresh before changing this session');
    if(['recycle','purge'].includes(action)&&(row.main_chat===true||row.can_delete===false))throw Error('Main Chat cannot be deleted');
    if(action==='recycle'&&row._origin==='bin')throw Error('Permanent deletion is pending in V3');
    const signature=JSON.stringify({id,action,fields});
    let body=receipts.get(signature);
    if(!body){body={client_request_id:crypto.randomUUID(),expected_revision:row.metadata_revision,...fields};receipts.set(signature,body);}
    try{
      const method=action==='restore'?'POST':action==='recycle'?'DELETE':'PATCH';
      const data=await request('/sessions/'+encodeURIComponent(id)+(action==='restore'?'/restore':''),method,body);
      if(isAlive()){rows.set(id,{...row,...data.session,_origin:data.session.status==='recycled'?'bin':'active'});receipts.delete(signature);changed();}
      return data;
    }catch(error){if(error.status===409||error.status===400)receipts.delete(signature);throw error;}
  }
  async function markRead(id){if(!isAlive())throw Error('Sessions page closed');const row=rows.get(id);if(!row||row._origin==='bin')throw Error('Restore this session before marking it read');await request('/sessions/'+encodeURIComponent(id)+'/read','PUT',{through_seq:row.last_public_seq||0});if(isAlive())changed();}
  const stateKey='v3:sessions-view:'+userId;
  return {project,catalog,mutate,markRead,rows,
    loadState(key){try{return JSON.parse(localStorage.getItem(stateKey+":"+key)||'{}');}catch{return {}; }},
    saveState(key,value){try{localStorage.setItem(stateKey+":"+key,JSON.stringify(value));}catch{}},
    async search(q,status,continuation){return request('/sessions/search?'+new URLSearchParams({q,status,limit:'50',...(continuation?{continuation}:{})}));}
  };
}
