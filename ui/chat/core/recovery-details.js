// Presentation only; authenticated recovery remains the decision authority.
export function createRecoveryDetails(run,{expanded,button,decide}) {
  const details=document.createElement('details');details.className='recovery-details';details.open=expanded.has(run.id);
  details.addEventListener('toggle',()=>{if(details.open)expanded.add(run.id);else expanded.delete(run.id);});
  const p=run.pending||{},labels={timeout:'Timed out',cancelled:'Cancelled',transport_error:'Connection interrupted',uncertain:'Outcome unknown'};
  const label=labels[p.interruption?.reason];
  const summary=document.createElement('summary');summary.textContent=(p.name||'Tool')+' — '+(label||'Outcome unknown')+' · Details';details.append(summary);
  const warning=document.createElement('p');warning.textContent='The action may have completed. Verify its outcome before authorizing a retry.';details.append(warning);
  const info=document.createElement('p');info.textContent='Reason: '+(label||'Not recorded for this interruption')+(p.arguments?.timeout_seconds!=null?' · Timeout limit: '+p.arguments.timeout_seconds+' seconds':'');details.append(info);
  if(p.arguments){const args=document.createElement('pre');args.textContent=JSON.stringify(p.arguments,null,2);details.append(args);}
  const reference=document.createElement('p');reference.className='recovery-reference';reference.textContent='Run: '+run.id+(p.call_id?' · Call: '+p.call_id:'');details.append(reference);
  const output=document.createElement('textarea');output.placeholder='Known result to accept';output.setAttribute('aria-label','Known action result');details.append(output);
  const actions=document.createElement('div');actions.className='recovery-actions';
  for(const decision of ['accept','retry','abandon'])actions.append(button({accept:'Accept known result',retry:'Authorize retry',abandon:'Abandon run'}[decision],()=>decide(decision,output.value)));
  details.append(actions);return details;
}
