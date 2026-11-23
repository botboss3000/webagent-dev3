// The system module owns history and measurements; this renderer never fabricates missing samples.
export const advancedIds=['metric_chart','devices','db_mode','db_latency','db_latency_chart','loop_split','token_rate','git_changes'];
export async function readDashboard(signal,window){
 try{
  const response=await fetch('/api/v3/admin/system/dashboard?window='+encodeURIComponent(window),{credentials:'same-origin',cache:'no-store',signal});
  if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===403?'Administrator access required':'Dashboard measurements unavailable ('+response.status+')');
  const value=await response.json();
  if(value.schema_version!==1||value.window!==window||!Array.isArray(value.history?.points)||value.history.points.length>601||!value.history.points.every(point=>point&&numeric(point.at))||!Array.isArray(value.database?.probes)||!value.database.probes.every(probe=>probe&&typeof probe==='object')||!['scope','devices','git','loop','throughput','usage','runtime'].every(key=>value[key]&&typeof value[key]==='object'))throw Error('Invalid dashboard measurements');
  return value;
 }catch(error){return {error:error.name==='AbortError'?'Dashboard request timed out':error.message};}
}
const numeric=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
const fixed=(value,suffix='')=>numeric(value)?value.toFixed(2)+suffix:'Unavailable';
const chartMetrics={cpu:['CPU','%'],memory:['RAM','%'],input_tokens:['Reported input tokens','tokens'],output_tokens:['Reported output tokens','tokens'],reported_calls:['Reported calls','calls'],known_cost_usd:['Known cost subtotal','USD'],active_runs:['Active runs','runs'],db_ms:['Database probe','ms']};
export const metricOptions=Object.entries(chartMetrics).map(([id,[label]])=>[id,label]);
/** Render at most 600 real samples and break lines across collection gaps. */
function chart(host,points,key,node,interval=30){
 const [label,unit]=chartMetrics[key]||chartMetrics.cpu;
 const valid=points.filter(p=>numeric(p[key])&&numeric(p.at));
 if(!valid.length){host.append(node('p','Collecting '+label.toLowerCase()+' samples…'));return;}
 const stride=Math.max(1,Math.ceil(valid.length/600));
 const sampled=valid.filter((_,index)=>index%stride===0||index===valid.length-1);
 const ns='http://www.w3.org/2000/svg',svg=document.createElementNS(ns,'svg');
 svg.setAttribute('viewBox','0 0 640 170');svg.setAttribute('role','img');svg.setAttribute('aria-label',label+' sampled history in '+unit);svg.classList.add('dashboard-history');
 const first=sampled[0].at,last=sampled.at(-1).at,max=['cpu','memory'].includes(key)?100:Math.max(1,...sampled.map(p=>p[key]));
 let segment=[],previous;
 const flush=()=>{if(segment.length>1){const line=document.createElementNS(ns,'polyline');line.setAttribute('points',segment.join(' '));line.setAttribute('fill','none');line.setAttribute('stroke','currentColor');line.setAttribute('stroke-width','2');svg.append(line);}segment=[];};
 for(const point of sampled){
  if(previous!=null&&point.at-previous>3*interval*stride)flush();
  const x=last===first?320:12+616*(point.at-first)/(last-first),y=155-140*point[key]/max;
  segment.push(x+','+y);previous=point.at;
  if(point===sampled.at(-1)){const dot=document.createElementNS(ns,'circle');dot.setAttribute('cx',x);dot.setAttribute('cy',y);dot.setAttribute('r','3');dot.setAttribute('fill','currentColor');svg.append(dot);}
 }
 flush();host.append(svg,node('p',label+': '+fixed(valid.at(-1)[key],' '+unit)));
 host.append(node('p',valid.length+' real samples · '+new Date(first*1000).toLocaleTimeString()+' – '+new Date(last*1000).toLocaleTimeString(),'dashboard-scope'));
}
/** Paint the remaining tile contracts and complete usage projections from the same scoped backend snapshot. */
export function paintDashboard(views,data,metric,node){
 if(!data)return;
 const set=(id,rows,note,state='live',draw=null)=>{
  const view=views.get(id);if(!view)return;
  view.tile.dataset.state=state;view.badge.textContent=state==='live'?'Live':state==='warming'?'Collecting samples':state==='loading'?'Loading…':state==='partial'?'Live · partial measurement':'Unavailable';
  view.body.replaceChildren();
  for(const [label,value] of rows){const row=node('div',undefined,'dashboard-field');row.append(node('span',String(label)),node('span',String(value)));view.body.append(row);}
  if(draw)draw(view.body);
  view.needs.textContent=note;
 };
 if(data.loading){for(const id of advancedIds)set(id,[],'Loading selected window…','loading');return;}
 if(data.error){for(const id of advancedIds)set(id,[],data.error,'unavailable');return;}
 const scope='History is collected on dashboard requests; gaps are not filled. Retained for 7 days at 30-second resolution.';
 set('metric_chart',[],scope,'live',body=>chart(body,data.history.points,metric,node,data.history.plot_interval_seconds||30));
 const db=data.database;
 set('db_mode',db.probes.map(p=>[p.name,p.available?p.journal_mode:'Unavailable']),'Journal modes from the application, current-user and current-agent SQLite pools.',db.available?'live':'partial');
 set('db_latency',[...db.probes.map(p=>[p.name,fixed(p.latency_ms,' ms')]),['Window p95',fixed(db.window_p95_ms,' ms')]],db.measurement,db.available?'live':'partial');
 set('db_latency_chart',[],scope+' '+db.measurement,'live',body=>chart(body,data.history.points,'db_ms',node,data.history.plot_interval_seconds||30));
 const devices=data.devices;
 set('devices',[['Visible devices',devices.devices?.length??'Unavailable'],...(devices.devices||[]).slice(0,16).map(d=>[d.name,d.kind+(d.state?' · '+d.state:'')])],devices.scope+' · showing up to 16; inventory is bounded per device kind.',devices.available?'live':'unavailable');
 const git=data.git;
 set('git_changes',[['Source changes',git.available?git.changes.length+(git.truncated?'+':''):'Unavailable'],...(git.changes||[]).map(c=>[c.path,c.state])],
  git.available?git.scope+'. Read-only status; use Controlled Builds for source changes.':git.reason,git.available?'live':'unavailable',body=>{const link=node('a','Open Controlled Builds');link.href='/app?tab=builds';body.append(link);});
 const loop=data.loop,total=loop.total_samples;
 set('loop_split',[['Model phase',total?fixed(100*loop.model_samples/total,'%'):'No running samples'],['Tool phase',total?fixed(100*loop.tool_samples/total,'%'):'No running samples'],['Other running phase',total?fixed(100*loop.other_samples/total,'%'):'No running samples'],['Running phase observations',total]],loop.measurement+'. Current-agent runs only.');
 const throughput=data.throughput;
 set('token_rate',[['Reported output',fixed(throughput.output_tokens_per_second,' tokens/s')],['Reported calls',fixed(throughput.calls_per_second,' calls/s')],['Sample interval',fixed(throughput.interval_seconds,' s')]],throughput.measurement,throughput.available?'live':'warming');
 const usage=data.usage,limited=data.scope.sessions_truncated||usage.sessions_without_usage>0||usage.unreported_calls>0;
 const note='Caller-owned active sessions with current access · '+usage.session_count+' sessions'+(data.scope.sessions_truncated?' · bounded at 2,000 sessions':'')+'. Only provider-reported usage is included.';
 set('tokens',[['Reported input tokens',usage.input_tokens],['Reported output tokens',usage.output_tokens],['Unreported calls',usage.unreported_calls],['Sessions without usage',usage.sessions_without_usage]],note,limited?'partial':'live');
 const knownCalls=usage.reported_calls+usage.unreported_calls-usage.unknown_cost_calls;
 set('cost',[['Known cost subtotal (USD)',knownCalls>0?fixed(usage.known_cost_usd,' USD'):'Unavailable'],['Calls with unknown cost',usage.unknown_cost_calls]],note+' Missing pricing is not treated as free.',usage.unknown_cost_calls||limited?'partial':'live');
 if(Array.isArray(usage.tools))set('tool_usage',[['Reported model calls',usage.reported_calls],...usage.tools.map(t=>[t.name,t.calls])],note+' Tool counts cover up to 500 admitted model interaction records.');
 if(Array.isArray(usage.agents))set('tokens_by_agent',usage.agents.map(a=>[a.agent,a.tokens]),note,limited?'partial':'live');
}
