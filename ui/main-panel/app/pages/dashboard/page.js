import { readDashboard, paintDashboard, metricOptions } from './advanced.js';
import { readActivity } from './activity.js';
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
  const notice=node('p','System resources refresh every 5 seconds; other connected tiles every 30 seconds. Red marks unavailable measurements. History is sampled while this dashboard is visited and retains actual samples for seven days. Metrics reflect the server process namespace, not container quotas; storage covers /.');
  const toolbar=node('div',undefined,'dashboard-toolbar');
  const count=node('span');count.setAttribute('role','status');
  const add=node('button','Add card');add.type='button';add.setAttribute('aria-expanded','false');
  const reset=node('button','Restore all tiles');reset.type='button';
  const edit=node('button','Edit layout');edit.type='button';edit.setAttribute('aria-pressed','false');
  const windowSelect=node('select');windowSelect.setAttribute('aria-label','Dashboard time window');
  for(const [index,label] of ['Last 5 min','Last hour','Last 24 hours','Last 7 days'].entries()){const option=node('option',label);option.value=['5m','1h','24h','7d'][index];windowSelect.append(option);}
  windowSelect.selectedIndex=1;
  const metricSelect=node('select');metricSelect.setAttribute('aria-label','History chart metric');
  for(const [value,label] of metricOptions){const option=node('option',label);option.value=value;metricSelect.append(option);}
  const refreshNow=node('button','Refresh now');refreshNow.type='button';
  toolbar.append(count,windowSelect,metricSelect,refreshNow,edit,add,reset);
  const picker=node('div',undefined,'dashboard-picker');picker.hidden=true;picker.setAttribute('aria-label','Add dashboard card');
  const grid=node('div',undefined,'dashboard-grid');
  host.append(heading,notice,toolbar,picker,grid);
  let selected=catalog.cards.map(card=>card.id),editing=false;
  let activity=new Map(),nextActivityAt=0,dashboardData=null;
  const updateDashboard=()=>paintDashboard(activityViews,dashboardData,metricSelect.value,node);
  const activityViews=new Map();
  let sample=null,failure='Loading system resources…',inFlight=false;
  const resources=new Set(['cpu','memory','storage','uptime','network','system_resources']);
  const views=new Map();
  const number=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
  const bytes=value=>number(value)?(value/1073741824).toFixed(2)+' GiB':'Unavailable';
  const percent=value=>number(value)?value.toFixed(1)+'%':'Unavailable';
  const duration=value=>number(value)?Math.floor(value/86400)+'d '+Math.floor(value/3600)%24+'h '+Math.floor(value/60)%60+'m':'Unavailable';
  const speed=value=>number(value)?(value>=1048576?(value/1048576).toFixed(2)+' MiB/s':(value/1024).toFixed(1)+' KiB/s'):'Unavailable';
  const sum=(items,key)=>items.reduce((total,item)=>total+(number(item[key])?item[key]:0),0);
  /** Update values in place so automatic refresh preserves keyboard focus and layout. */
  function updateMetrics(){
    const m=sample?.metrics||{},cpu=m.cpu,memory=m.memory,uptime=m.uptime,swap=memory?.swap;
    const disk=m.disks?.available?m.disks.filesystems?.find(item=>item.mount==='/')?.usage:null;
    const nics=m.network?.available&&Array.isArray(m.network.interfaces)?m.network.interfaces.filter(item=>item&&!item.loopback):null;
    const rated=Boolean(nics?.length)&&nics.every(item=>item.rates_available&&number(item.rx_bytes_per_second)&&number(item.tx_bytes_per_second));
    const cpuValue=cpu?.available&&cpu.utilization_available?percent(cpu.utilization_percent):cpu?.available?'Warming up':'Unavailable';
    const swapText=swap?.available?(number(swap.total_bytes)&&swap.total_bytes>0?' · swap '+bytes(swap.used_bytes)+' / '+bytes(swap.total_bytes):' · no swap'):' · swap unavailable';
    const values={
      cpu:[cpuValue,cpu?.available&&number(cpu.logical_processors)?cpu.logical_processors+' logical processors':'Unavailable'],
      memory:[memory?.available?percent(memory.utilization_percent):'Unavailable',memory?.available?bytes(memory.used_bytes)+' used / '+bytes(memory.total_bytes)+swapText:'Unavailable'],
      uptime:[uptime?.available?duration(uptime.seconds):'Unavailable','System uptime'],
      storage:[disk?.available?bytes(disk.available_bytes):'Unavailable',disk?.available?'Available on / · '+bytes(disk.used_bytes)+' used of '+bytes(disk.total_bytes)+' ('+percent(disk.utilization_percent)+')':'Available on /'],
      network:[rated?'In '+speed(sum(nics,'rx_bytes_per_second'))+' · Out '+speed(sum(nics,'tx_bytes_per_second')):nics?.length?'Warming up':'Unavailable',
        nics?.length?nics.length+' non-loopback interface'+(nics.length===1?'':'s')+' · received '+bytes(sum(nics,'rx_bytes'))+' · sent '+bytes(sum(nics,'tx_bytes'))+' · errors '+(sum(nics,'rx_errors')+sum(nics,'tx_errors'))+' · dropped '+(sum(nics,'rx_dropped')+sum(nics,'tx_dropped')):'No non-loopback interface counters']
    };
    const fields={
      'CPU cores':cpu?.available&&number(cpu.logical_processors)?String(cpu.logical_processors):null,
      'RAM':memory?.available?bytes(memory.used_bytes)+' / '+bytes(memory.total_bytes):null,
      'Disk free':disk?.available?bytes(disk.available_bytes):null,
      'Uptime':uptime?.available?duration(uptime.seconds):null,
      'Load average':dashboardData?.runtime?.load_average??null,
      'Active chat runs':number(dashboardData?.runtime?.active_runs)?String(dashboardData.runtime.active_runs):null,
      'Chat concurrency':number(dashboardData?.runtime?.concurrency)?(dashboardData.runtime.concurrency===0?'Unlimited':String(dashboardData.runtime.concurrency)):null,
      'Open descriptors':number(dashboardData?.runtime?.open_descriptors)?String(dashboardData.runtime.open_descriptors):null
    };
    for(const [id,view] of views){
      const combined=id==='system_resources';
      const available=combined?Object.values(fields).some(Boolean):id==='cpu'?cpu?.available&&cpu.utilization_available&&number(cpu.utilization_percent):id==='memory'?memory?.available&&number(memory.utilization_percent):id==='storage'?disk?.available&&number(disk.available_bytes):id==='network'?rated:uptime?.available&&number(uptime.seconds);
      view.tile.dataset.state=failure?'error':available?(combined?'partial':'live'):'unavailable';
      view.badge.textContent=failure||(!available?((id==='cpu'&&cpu?.available)||(id==='network'&&nics?.length)?'Warming up':'Unavailable'):combined?'Live · server resources and scoped runs':'Live');
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
  /** Combine readiness with resource and database readings already polled; no alert thresholds are inferred. */
  function health(data){
    const m=failure?{}:sample?.metrics||{},disk=m.disks?.available?m.disks.filesystems?.find(item=>item.mount==='/')?.usage:null;
    const probes=Array.isArray(dashboardData?.database?.probes)?dashboardData.database.probes:null;
    const rows=[...data.rows,
      ['CPU',m.cpu?.available&&m.cpu.utilization_available?percent(m.cpu.utilization_percent):'Unavailable'],
      ['RAM',m.memory?.available?percent(m.memory.utilization_percent):'Unavailable'],
      ['Disk used on /',disk?.available?percent(disk.utilization_percent):'Unavailable'],
      ['Database pools',probes?.length?probes.filter(probe=>probe?.available===true).length+' / '+probes.length+' responding':'Unavailable']];
    if(data.state==='unavailable')return {...data,rows};
    const complete=rows.every(([,value])=>value!=='Unavailable')&&probes.every(probe=>probe?.available===true);
    return {...data,rows,state:complete?'live':'partial',note:'API readiness, current server resources and read-only database probes. No alert thresholds are applied.'};
  }
  /** Paint scoped API projections without touching the layout controls. */
  function updateActivity(){
    for(const [id,view] of activityViews){
      let data=activity.get(id);if(!data)continue;
      if(id==='health_board')data=health(data);
      view.tile.dataset.state=data.state;
      view.badge.textContent=data.state==='live'?'Live':data.state==='partial'?'Live · limited scope':'Unavailable';
      view.body.replaceChildren();
      for(const [label,value] of data.rows){
        const row=node('div',undefined,'dashboard-field');row.append(node('span',String(label)),node('span',String(value)));view.body.append(row);
      }
      if(!data.rows.length)view.body.append(node('p',data.state==='unavailable'?'—':'No reported data'));
      view.needs.textContent=data.note;
      if(data.link){const link=node('a',data.link[0]);link.href=data.link[1];view.body.append(link);}
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
    const pendingActivity=Date.now()>=nextActivityAt?readActivity(request.signal):null;
    const requestedWindow=windowSelect.value,pendingDashboard=readDashboard(request.signal,requestedWindow);
    try{
      const response=await fetch('/api/v3/admin/system/summary',{credentials:'same-origin',cache:'no-store',signal:request.signal});
      if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===403?'Administrator access required':'System metrics unavailable ('+response.status+')');
      const data=await response.json();
      if(data.schema_version!==1||!data.metrics||!number(data.sampled_at_unix_ms))throw Error('Invalid system metrics response');
      if(!stopped){sample=data;failure='';}
    }catch(error){
      if(!stopped){sample=null;failure=error.name==='AbortError'?'System metrics request timed out':error.message;}
    }finally{
      if(pendingActivity){const result=await pendingActivity;if(!stopped){activity=result;nextActivityAt=Date.now()+30000;}}
      const dashboardResult=await pendingDashboard;if(!stopped&&requestedWindow===windowSelect.value)dashboardData=dashboardResult;
      clearTimeout(timeout);controller.signal.removeEventListener('abort',cancel);inFlight=false;
      if(!stopped){updateMetrics();updateActivity();updateDashboard();timer=setTimeout(refresh,requestedWindow===windowSelect.value?5000:0);}
    }
  }
  /** Repaint local layout only; persistence remains explicitly pending. */
  function paint(){
    views.clear();activityViews.clear();grid.replaceChildren();picker.replaceChildren();count.textContent=selected.length+' tiles · layout changes last until reload';
    for(const [index,id] of selected.entries()){
      const card=catalog.cards.find(item=>item.id===id);
      const tile=node('article',undefined,'dashboard-tile');tile.dataset.card=id;tile.style.setProperty('--tile-span',card.w);
      const title=node('h3',card.label),badge=node('p','Loading…','dashboard-pending');
      const body=node('div',undefined,'dashboard-preview');
      if(card.id==='metric_chart'||card.id==='db_latency_chart'){const plot=node('div','Loading sampled history…','dashboard-chart-empty');body.append(plot);}
      for(const label of card.fields){const row=node('div',undefined,'dashboard-field');row.append(node('span',label),node('span','—'));body.append(row);}
      if(!card.fields.length)body.append(node('div','—','dashboard-value'));
      const needs=node('p','Loading dashboard data…','dashboard-needs');
      tile.append(title,badge,body,needs);
      if(resources.has(id)){
        if(id!=='system_resources')body.replaceChildren(node('div','—','dashboard-value'));
        const fields=new Map([...body.querySelectorAll('.dashboard-field')].map(row=>[row.firstElementChild.textContent,row.lastElementChild]));
        views.set(id,{tile,badge,needs,fields,value:body.querySelector('.dashboard-value')});
      }
      if(!resources.has(id))activityViews.set(id,{tile,badge,body,needs});
      if(editing){
        const actions=node('div',undefined,'dashboard-tile-actions');
        for(const [label,delta] of [['Move earlier',-1],['Move later',1],['Remove tile',0]]){
          const button=node('button',label);button.type='button';button.setAttribute('aria-label',label+': '+card.label);
          button.disabled=delta<0?index===0:delta>0?index===selected.length-1:false;
          button.onclick=()=>{if(delta){[selected[index],selected[index+delta]]=[selected[index+delta],selected[index]];}else selected.splice(index,1);paint();};actions.append(button);
        }tile.append(actions);
      }grid.append(tile);
    }
    updateMetrics();updateActivity();updateDashboard();
    for(const card of catalog.cards){const button=node('button',card.label);button.type='button';button.disabled=selected.includes(card.id);button.onclick=()=>{selected.push(card.id);picker.hidden=true;add.setAttribute('aria-expanded','false');paint();};picker.append(button);}
  }
  add.onclick=()=>{picker.hidden=!picker.hidden;add.setAttribute('aria-expanded',String(!picker.hidden));};
  reset.onclick=()=>{selected=catalog.cards.map(card=>card.id);paint();};
  edit.onclick=()=>{editing=!editing;edit.setAttribute('aria-pressed',String(editing));edit.textContent=editing?'Done editing':'Edit layout';paint();};
  host.onkeydown=event=>{if(event.key==='Escape'){picker.hidden=true;add.setAttribute('aria-expanded','false');add.focus();}};
  windowSelect.onchange=()=>{dashboardData={loading:true};updateDashboard();void refresh();};
  metricSelect.onchange=updateDashboard;
  refreshNow.onclick=()=>{nextActivityAt=0;void refresh();};
  paint();
  void refresh();
  return dispose;
}
