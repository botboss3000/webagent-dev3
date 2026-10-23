// The catalog owns tile identity and size; system summary supplies current resource values.
 // @code-index {"version":1,"feature":"app-dashboard","purpose":"Config: ui/main-panel/app/pages/dashboard/cards.json -> cards; frontend tile inventory and pending backend state","span_lines":64,"configuration":{"status":"configured","references":[{"path":"ui/main-panel/app/pages/dashboard/cards.json","pointers":["/cards"]}],"opportunities":[]},"aliases":["dashboard tiles"]}
/** Mount resource tiles from the administrator summary; return a polling disposer. */
export async function mountDashboard(host) {
  host._dashboardDispose?.();
  const controller=new AbortController();
  let stopped=false,timer;
  const visible=()=>host.isConnected&&!host.closest('[hidden]')&&!document.hidden;
  const observer=new MutationObserver(()=>{if(!host.isConnected)dispose();});
  function dispose(){
    stopped=true;clearTimeout(timer);controller.abort();observer.disconnect();
    document.removeEventListener('visibilitychange',onVisibility);
    if(host._dashboardDispose===dispose)delete host._dashboardDispose;
  }
  function onVisibility(){if(!document.hidden&&!timer&&!stopped)void refresh();}
  host._dashboardDispose=dispose;
  observer.observe(document.body,{childList:true,subtree:true});
  document.addEventListener('visibilitychange',onVisibility);
  // Page descriptors accept sibling filenames only; this module owns its nested stylesheet.
  const stylesheet=new URL('./page.css',import.meta.url).href;
  if(!document.querySelector('link[data-dashboard-style]')){
    const link=document.createElement('link');link.rel='stylesheet';link.href=stylesheet;
    link.dataset.dashboardStyle='true';document.head.append(link);
  }
  const response=await fetch(new URL('./cards.json',import.meta.url),{cache:'no-store',signal:controller.signal});
  if(!response.ok)throw Error('Dashboard tile catalog unavailable');
  const catalog=await response.json();
  if(stopped||!host.isConnected){dispose();return dispose;}
  if(catalog.version!==1||!Array.isArray(catalog.cards))throw Error('Invalid dashboard tile catalog');
  const ids=new Set();
  for(const card of catalog.cards){
    if(!/^[a-z][a-z0-9_]*$/.test(card.id)||ids.has(card.id)||!card.label||!Number.isInteger(card.w)||card.w<1||card.w>12||!Array.isArray(card.fields)||!Array.isArray(card.sections))throw Error('Invalid dashboard tile');
    ids.add(card.id);
  }
  const node=(tag,text,className)=>{const el=document.createElement(tag);if(text!==undefined)el.textContent=text;if(className)el.className=className;return el;};
  host.replaceChildren();host.classList.add('v3-dashboard');
  const heading=node('h2','Dashboard');
  const notice=node('p','Live system resources refresh every 5 seconds. Red marks unavailable data or backend work still needed. Metrics reflect the server process namespace, not container quotas; storage covers /.');
  const toolbar=node('div',undefined,'dashboard-toolbar');
  const count=node('span');count.setAttribute('role','status');
  const add=node('button','Add card');add.type='button';add.setAttribute('aria-expanded','false');
  const reset=node('button','Restore all tiles');reset.type='button';
  const edit=node('button','Edit layout');edit.type='button';edit.setAttribute('aria-pressed','false');
  const windowSelect=node('select');windowSelect.disabled=true;windowSelect.setAttribute('aria-label','Time window — backend work needed');
  for(const label of ['Last 5 min','Last hour','Last 24 hours','Last 7 days'])windowSelect.append(node('option',label));
  windowSelect.selectedIndex=1;
  toolbar.append(count,windowSelect,edit,add,reset);
  const picker=node('div',undefined,'dashboard-picker');picker.hidden=true;picker.setAttribute('aria-label','Add dashboard card');
  const grid=node('div',undefined,'dashboard-grid');
  host.append(heading,notice,toolbar,picker,grid);
  let selected=catalog.cards.map(card=>card.id),editing=false;
  let sample=null,failure='Loading system resources…',inFlight=false;
  const resources=new Set(['cpu','memory','storage','uptime','system_resources']);
  const views=new Map();
  const number=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
  const bytes=value=>number(value)?(value/1073741824).toFixed(2)+' GiB':'Unavailable';
  const percent=value=>number(value)?value.toFixed(1)+'%':'Unavailable';
  const duration=value=>number(value)?Math.floor(value/86400)+'d '+Math.floor(value/3600)%24+'h '+Math.floor(value/60)%60+'m':'Unavailable';
  /** Update values in place so automatic refresh preserves keyboard focus and layout. */
  function updateMetrics(){
    const m=sample?.metrics||{},cpu=m.cpu,memory=m.memory,uptime=m.uptime;
    const disk=m.disks?.available?m.disks.filesystems?.find(item=>item.mount==='/')?.usage:null;
    const cpuValue=cpu?.available&&cpu.utilization_available?percent(cpu.utilization_percent):cpu?.available?'Warming up':'Unavailable';
    const values={
      cpu:[cpuValue,cpu?.available&&number(cpu.logical_processors)?cpu.logical_processors+' logical processors':'Unavailable'],
      memory:[memory?.available?percent(memory.utilization_percent):'Unavailable',memory?.available?bytes(memory.used_bytes)+' used / '+bytes(memory.total_bytes):'Unavailable'],
      uptime:[uptime?.available?duration(uptime.seconds):'Unavailable','System uptime'],
      storage:[disk?.available?bytes(disk.available_bytes):'Unavailable','Available on /']
    };
    const fields={
      'CPU cores':cpu?.available&&number(cpu.logical_processors)?String(cpu.logical_processors):null,
      'RAM':memory?.available?bytes(memory.used_bytes)+' / '+bytes(memory.total_bytes):null,
      'Disk free':disk?.available?bytes(disk.available_bytes):null,
      'Uptime':uptime?.available?duration(uptime.seconds):null
    };
    for(const [id,view] of views){
      const combined=id==='system_resources';
      const available=combined?Object.values(fields).some(Boolean):id==='cpu'?cpu?.available&&cpu.utilization_available&&number(cpu.utilization_percent):id==='memory'?memory?.available&&number(memory.utilization_percent):id==='storage'?disk?.available&&number(disk.available_bytes):uptime?.available&&number(uptime.seconds);
      view.tile.dataset.state=failure?'error':available?(combined?'partial':'live'):'unavailable';
      view.badge.textContent=failure||(!available?(id==='cpu'&&cpu?.available?'Warming up':'Unavailable'):combined?'Live · some fields need backend work':'Live');
      if(combined){
        for(const [label,valueNode] of view.fields){
          const value=failure?null:fields[label];
          valueNode.textContent=value||'—';
          valueNode.parentElement.dataset.available=String(Boolean(value));
        }
      }else{
        view.value.textContent=failure?'—':values[id][0];
        view.needs.textContent=failure?'System metrics unavailable.':values[id][1];
      }
    }
  }
  /** Poll one cached summary; errors clear old readings and never become fake zeroes. */
  async function refresh(){
    clearTimeout(timer);timer=null;
    if(stopped)return;
    if(!host.isConnected){dispose();return;}
    if(!visible()){timer=setTimeout(refresh,5000);return;}
    if(inFlight)return;
    inFlight=true;
    const request=new AbortController();
    const cancel=()=>request.abort();controller.signal.addEventListener('abort',cancel,{once:true});
    const timeout=setTimeout(()=>request.abort(),15000);
    try{
      const response=await fetch('/api/v3/admin/system/summary',{credentials:'same-origin',cache:'no-store',signal:request.signal});
      if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===403?'Administrator access required':'System metrics unavailable ('+response.status+')');
      const data=await response.json();
      if(data.schema_version!==1||!data.metrics||!number(data.sampled_at_unix_ms))throw Error('Invalid system metrics response');
      if(!stopped){sample=data;failure='';}
    }catch(error){
      if(!stopped){sample=null;failure=error.name==='AbortError'?'System metrics request timed out':error.message;}
    }finally{
      clearTimeout(timeout);controller.signal.removeEventListener('abort',cancel);inFlight=false;
      if(!stopped){updateMetrics();timer=setTimeout(refresh,5000);}
    }
  }
  /** Repaint local layout only; persistence remains explicitly pending. */
  function paint(){
    views.clear();grid.replaceChildren();picker.replaceChildren();count.textContent=selected.length+' tiles · layout changes last until reload';
    for(const [index,id] of selected.entries()){
      const card=catalog.cards.find(item=>item.id===id);
      const tile=node('article',undefined,'dashboard-tile');tile.dataset.card=id;tile.style.setProperty('--tile-span',card.w);
      const title=node('h3',card.label),badge=node('p','Backend work needed','dashboard-pending');
      const body=node('div',undefined,'dashboard-preview');
      if(card.id==='metric_chart'||card.id==='db_latency_chart'){const plot=node('div','No time-series data connected','dashboard-chart-empty');body.append(plot);}
      for(const label of card.fields){const row=node('div',undefined,'dashboard-field');row.append(node('span',label),node('span','—'));body.append(row);}
      if(!card.fields.length)body.append(node('div','—','dashboard-value'));
      const needs=node('p',card.id==='git_changes'?'Needs V3 Git status and action adapters.':'Needs V3 '+card.sections.join(', ')+' data.','dashboard-needs');
      tile.append(title,badge,body,needs);
      if(resources.has(id)){
        if(id!=='system_resources')body.replaceChildren(node('div','—','dashboard-value'));
        const fields=new Map([...body.querySelectorAll('.dashboard-field')].map(row=>[row.firstElementChild.textContent,row.lastElementChild]));
        views.set(id,{tile,badge,needs,fields,value:body.querySelector('.dashboard-value')});
      }
      if(editing){
        const actions=node('div',undefined,'dashboard-tile-actions');
        for(const [label,delta] of [['Move earlier',-1],['Move later',1],['Remove tile',0]]){
          const button=node('button',label);button.type='button';button.setAttribute('aria-label',label+': '+card.label);
          button.disabled=delta<0?index===0:delta>0?index===selected.length-1:false;
          button.onclick=()=>{if(delta){[selected[index],selected[index+delta]]=[selected[index+delta],selected[index]];}else selected.splice(index,1);paint();};actions.append(button);
        }tile.append(actions);
      }grid.append(tile);
    }
    updateMetrics();
    for(const card of catalog.cards){const button=node('button',card.label);button.type='button';button.disabled=selected.includes(card.id);button.onclick=()=>{selected.push(card.id);picker.hidden=true;add.setAttribute('aria-expanded','false');paint();};picker.append(button);}
  }
  add.onclick=()=>{picker.hidden=!picker.hidden;add.setAttribute('aria-expanded',String(!picker.hidden));};
  reset.onclick=()=>{selected=catalog.cards.map(card=>card.id);paint();};
  edit.onclick=()=>{editing=!editing;edit.setAttribute('aria-pressed',String(editing));edit.textContent=editing?'Done editing':'Edit layout';paint();};
  host.onkeydown=event=>{if(event.key==='Escape'){picker.hidden=true;add.setAttribute('aria-expanded','false');add.focus();}};
  paint();
  void refresh();
  return dispose;
}
