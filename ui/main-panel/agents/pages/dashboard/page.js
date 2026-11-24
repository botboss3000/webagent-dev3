import {requestTouchVibration} from '../../../../shared/vibrate/vibrate.js';
import {renderPlanPanel} from '../../../../shared/plan-panel/plan-panel.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);if(text)el.textContent=text;if(cls)el.className=cls;return el;};
const number=value=>Number(value||0).toLocaleString();
const time=seconds=>new Date(seconds*1000).toLocaleTimeString(undefined,{hour:'numeric',minute:'2-digit'});
const day=seconds=>new Date(seconds*1000).toLocaleDateString(undefined,{month:'short',day:'numeric'});
const minutes=seconds=>seconds>=7200?(seconds/3600).toFixed(1).replace(/\.0$/,'')+' h':Math.round(seconds/60)+' min';
const megabytes=bytes=>number(Math.ceil(Number(bytes||0)/1048576))+' MB';
const dollars=micros=>'$'+(Number(micros)/1e6).toFixed(Number(micros)<1e5?4:2);

/** A section: heading, then one shared option-list of rows. */
function section(host,title){host.append(node('h3',title,'dash-heading'));const list=node('div','','ac-list dash-list');list.setAttribute('aria-label',title);host.append(list);return list;}

/** One read-only row: title with its hint below, the value on the right and, with a limit, a meter. */
function row(list,title,hint,value,share=null){
  const item=node('div','','ac-ability-row ac-row-static dash-row'),label=node('span','','ac-ability-label');
  label.append(node('span',title,'ac-ability-name'));if(hint)label.append(node('span',hint,'ac-ability-desc'));
  item.append(label,node('span',value,'dash-value'));
  if(share!=null){const bar=node('span','','dash-meter'),fill=node('span','','dash-meter-fill');
    fill.style.width=Math.min(100,Math.round(share*100))+'%';bar.dataset.level=share>=1?'full':share>=0.8?'high':'ok';
    bar.setAttribute('role','meter');bar.setAttribute('aria-valuenow',String(Math.round(share*100)));bar.setAttribute('aria-valuemin','0');bar.setAttribute('aria-valuemax','100');bar.setAttribute('aria-label',title);
    bar.append(fill);item.append(bar);}
  list.append(item);return item;
}

/** "3 of 10" with its share, or "3 · no limit". */
function against(meter,format=number){
  const limit=meter?.limit;
  if(limit==null)return [format(meter?.used)+' · no limit',null];
  return [format(meter.used)+' of '+format(limit),limit>0?Number(meter.used||0)/limit:1];
}

/** One period's usage per model, with its total first. */
function usage(list,rows,empty){
  if(!rows.length){row(list,empty,'','');return;}
  const sum=key=>rows.reduce((total,item)=>total+Number(item[key]||0),0);
  const costs=rows.filter(item=>item.cost_micros!=null);
  const hint=item=>number(item.input_tokens)+' tokens in · '+number(item.output_tokens)+' out'+(item.compute_ms?' · '+minutes(item.compute_ms/1000)+' of commands':'');
  if(rows.length>1)row(list,'All models',hint({input_tokens:sum('input_tokens'),output_tokens:sum('output_tokens'),compute_ms:sum('compute_ms')}),number(sum('calls'))+' turns'+(costs.length?' · '+dollars(costs.reduce((total,item)=>total+item.cost_micros,0)):''));
  for(const item of rows)row(list,item.model||'Unknown model',hint(item),number(item.calls)+' turns'+(item.cost_micros!=null?' · '+dollars(item.cost_micros):''));
}

/** An agent's Dashboard for its owner (and administrators): shared plan limits across all the
 * owner's agents, this agent's usage per model, its people against its limits, today's limit
 * notices and the plan. Fetched on open and on Refresh; no timer. */
export async function renderDashboard(host,request,{agentId}){
  const generation=(host._dashboardGeneration||0)+1;host._dashboardGeneration=generation;
  const current=()=>host._dashboardGeneration===generation;
  const head=node('div','','dash-head'),refresh=node('button','Refresh','ac-btn ac-btn-sm'),stamp=node('span','','ac-hint dash-stamp');
  refresh.type='button';refresh.style.webkitTapHighlightColor='transparent';
  refresh.addEventListener('click',event=>{requestTouchVibration(event);void renderDashboard(host,request,{agentId});});
  head.append(node('h2','Dashboard'),stamp,refresh);
  const status=node('p','Loading…','ac-hint dash-status');status.setAttribute('role','status');
  const body=node('div','','dash-body');
  // A refresh keeps the figures on screen until the new ones arrive.
  const shown=host.querySelector('.dash-status');
  if(shown&&host.querySelector('.dash-body'))shown.textContent='Refreshing…';
  else host.replaceChildren(head,status,body);
  let data,billing=null;
  try{
    data=await request('/agents/'+encodeURIComponent(agentId)+'/dashboard');
    // Money figures belong to the viewer's own plan; another viewer's billing is never read.
    if(data.viewer_is_owner)billing=await request('/billing').catch(()=>null);
  }catch(error){if(current()){host.replaceChildren(head,status);status.textContent='Dashboard unavailable: '+error.message;}return;}
  if(!current())return;
  status.textContent='';stamp.textContent='Updated '+time(Date.now()/1000);
  host.replaceChildren(head,status,body);

  if(data.notices?.length){
    const list=section(body,'Limit notices today');
    for(const notice of data.notices){const item=row(list,notice.message,'',notice.severity==='warning'?'Limit reached':'Nearly reached');item.dataset.severity=notice.severity;}
  }

  const shared=section(body,data.viewer_is_owner?'Your plan, across all your agents':'The owner\'s plan, across all their agents');
  const [runs,runShare]=against(data.shared.runs_at_once);
  row(shared,'Runs going at once','Runs on all the agents, now.',runs,runShare);
  const [compute,computeShare]=against(data.shared.compute_seconds_today,minutes);
  row(shared,'Command time today','Commands, sandbox and browser. Resets '+day(data.resets.day)+' at '+time(data.resets.day)+'.',compute,computeShare);
  const storage=data.shared.storage_bytes;
  if(storage){
    const [stored,storedShare]=storage.used==null?['Not measured yet',null]:against({used:storage.used,limit:storage.limit},megabytes);
    row(shared,'Storage',storage.checked_at?'Measured '+day(storage.checked_at)+' '+time(storage.checked_at)+'.':'Your chats, files and folders.',stored,storedShare);
  }else row(shared,'Storage','Only the owner sees their storage.','—');
  const [owned,ownedShare]=against(data.shared.agents_owned);
  row(shared,'Agents owned','',owned,ownedShare);
  const automations=data.shared.automations_per_person?.limit;
  row(shared,'Automations',automations==null?'No limit on scheduled tasks and event triggers.':'Each person may keep this many scheduled tasks and event triggers on these agents.',automations==null?'No limit':'Up to '+number(automations));

  usage(section(body,'This agent today'),data.usage.today||[],'No model use yet today.');
  const month=section(body,'This agent this month');
  usage(month,data.usage.month||[],'No model use yet this month.');
  row(month,'Resets','',day(data.resets.month));

  const people=section(body,'People using this agent');
  const [recent,recentShare]=against(data.users.last_30_days);
  row(people,'People in the last 30 days','Members and visitors with a message here. New people are refused at the limit; those counted keep working.',recent,recentShare);
  const [now,nowShare]=against(data.users.at_once);
  row(people,'People at once','People with a run going now.',now,nowShare);
  for(const level of data.users.levels||[]){
    if(level.operator)continue;
    const limits=[level.runs_at_once!=null?level.runs_at_once+' runs at once':'',level.daily_turns!=null?level.daily_turns+' messages a day':'',level.monthly_turns!=null?level.monthly_turns+' in 30 days':''].filter(Boolean);
    row(people,level.name,limits.length?limits.join(' · '):'No message limits.',number(level.users)+(level.users===1?' person':' people'));
  }
  body.append(node('p','Levels and their limits are edited on the Identity page.','ac-hint'));

  body.append(node('h3','Plan','dash-heading'));
  const plan=node('div');body.append(plan);
  renderPlanPanel(plan,{plan:data.plan,billing});
  if(data.viewer_is_owner)body.append(node('p','Upgrade, credits and billing details are on your Profile page.','ac-hint'));
}
