'use strict';
import {requestTouchVibration} from '../vibrate/vibrate.js';

// The Plan panel on the Profile page and on an agent's Dashboard: the owner's tier and its end
// date, and the money figures only when the billing service has sent a plan summary. The open
// app holds no prices: amounts and purchase labels come from the billing service.
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);if(text)el.textContent=text;if(cls)el.className=cls;return el;};
const dollars=cents=>'$'+(Number(cents)/100).toFixed(2);
const date=seconds=>new Date(seconds*1000).toLocaleDateString(undefined,{year:'numeric',month:'short',day:'numeric'});
const UNAVAILABLE='Billing is unavailable right now';

/** What each tier means for the server's allowance, in owner-facing words. */
function tierText(tier){
  if(tier==='administrator')return 'No plan limits.';
  if(tier==='free')return 'A smaller server allowance that may be slowed when the server is busy.';
  return 'A larger server allowance; your agents\' runs go first when the server is busy.';
}

/** One row of the shared option-list look: title, hint below it, value on the right. */
function row(list,title,hint,value){
  const item=node('div','','ac-ability-row ac-row-static plan-row'),label=node('span','','ac-ability-label');
  label.append(node('span',title,'ac-ability-name'));if(hint)label.append(node('span',hint,'ac-ability-desc'));
  item.append(label);if(value!=null)item.append(node('span',value,'plan-value'));list.append(item);return item;
}

/**
 * Draw the panel into `host`. `plan` is {tier,name,until,source} (from `/plan` or the
 * Dashboard); `billing` is the `/billing` answer or null (no billing connection, or not the
 * viewer's own plan). With `actions`, purchase and billing-details buttons are shown when a
 * billing connection is configured; each asks the app for a one-time link and opens it.
 */
export function renderPlanPanel(host,{plan,billing=null,request=null,actions=false}){
  const list=node('div','','ac-list plan-panel');list.setAttribute('aria-label','Plan');
  const until=plan.until?'Until '+date(plan.until)+(plan.source==='paid'||plan.source==='both'?' (paid)':''):'No end date';
  const timed=plan.tier!=='free'&&plan.tier!=='administrator';
  row(list,plan.name||plan.tier,tierText(plan.tier),timed?until:null);
  const summary=billing?.configured?billing.summary?.summary:null;
  if(summary){
    if(summary.allowance_cents!=null)row(list,'Monthly allowance',summary.next_reset?'Resets '+date(summary.next_reset):'',
      dollars(summary.allowance_used_cents||0)+' of '+dollars(summary.allowance_cents)+' used');
    if(summary.credit_cents!=null)row(list,'Credit','Used after the allowance.',dollars(summary.credit_cents));
    if(summary.daily_cap_cents!=null)row(list,'Spending today','Resets at 00:00 UTC.',dollars(summary.daily_used_cents||0)+' of '+dollars(summary.daily_cap_cents));
    if(summary.plan_ends)row(list,summary.cancel_at_period_end?'Plan ends':'Renews',summary.cancel_at_period_end?'Cancelled; it stays until then.':'',date(summary.plan_ends));
  }
  const status=node('p','','ac-hint plan-status');status.setAttribute('role','status');
  host.replaceChildren(list);
  if(!actions||!billing?.configured||!request)return;
  const offers=Array.isArray(billing.offers)?billing.offers:[];
  const open=async(what,button)=>{
    if(!what){status.textContent=UNAVAILABLE;return;}
    button.disabled=true;status.textContent='Opening…';
    try{const {url}=await request('/billing/link','POST',{what});status.textContent='';location.assign(url);}
    catch(error){status.textContent=error.status===404?error.message:UNAVAILABLE;}
    finally{button.disabled=false;}
  };
  const bar=node('div','','plan-actions');
  const button=(label,kind,onClick)=>{const el=node('button',label,'ac-btn'+(kind?' ac-btn-'+kind:''));el.type='button';el.style.webkitTapHighlightColor='transparent';
    el.addEventListener('click',event=>{requestTouchVibration(event);onClick(el);});bar.append(el);return el;};
  if(plan.tier==='free')button('Upgrade to Plus','primary',el=>open(offers.find(offer=>offer.what.startsWith('plus'))?.what,el));
  const credits=offers.filter(offer=>offer.what.startsWith('credit'));
  const choices=node('div','','plan-actions plan-credits');choices.hidden=true;
  for(const offer of credits){const el=node('button',offer.label,'ac-btn');el.type='button';el.style.webkitTapHighlightColor='transparent';
    el.addEventListener('click',event=>{requestTouchVibration(event);open(offer.what,el);});choices.append(el);}
  button('Buy credits','',()=>{if(!credits.length){status.textContent=UNAVAILABLE;return;}choices.hidden=!choices.hidden;});
  button('All billing details','',el=>open('billing_details',el));
  host.append(bar,choices,status);
}

/** The signed-in person's own plan and billing, as the Profile page reads them on open. */
export async function loadOwnPlan(request){
  const [plan,billing]=await Promise.all([request('/plan'),request('/billing').catch(error=>{
    // No billing routes in this build, or billing refused for this sign-in: no connection.
    if(error.status===404||error.status===403)return null;throw error;})]);
  return {plan,billing};
}
