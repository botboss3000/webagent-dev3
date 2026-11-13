// Renders inert detail text and explicit user choices for interrupted tool runs; the supplied callback and runtime retain recovery authority.
/**
 * Render the recovery choices for a run whose tool calls were interrupted.
 * The choices are always visible: each open call has "Run again" and "Use my result"
 * (enabled once a result is typed), the run has "End run", and a run with no open call
 * offers "Resume run". Diagnostics (reason, stage, IDs, arguments) sit in a collapsed
 * section whose open state the expanded Set keeps by run ID. Arguments and metadata are
 * inert text; the decision callback submits choices to authenticated runtime recovery.
 */
export function createRecoveryDetails(run,{expanded,button,decide,code}) {
  const root=document.createElement('div');root.className='recovery-details';
  const p=run.pending||{},labels={timeout:'Timed out',cancelled:'Cancelled',transport_error:'Connection interrupted',uncertain:'Outcome unknown'};
  const failure=p.failure?.version===1?p.failure:(!run.pending&&run.tool_failure?.version===1?run.tool_failure:null);
  const stages={preparation:'Preparing execution',authorization:'Checking authorization',connect:'Connecting to tool service',request_write:'Sending request',response_read:'Reading response',response_validation:'Validating response',execution:'Executing tool',execution_task:'Running execution task'};
  const reasons={internal_error:'Internal failure',invalid_request:'Invalid request',authorization_rejected:'Authorization rejected',scope_changed:'Authorization scope changed',authority_stopped:'Authority stopped',legacy_authorization_rejected:'Broker authorization rejected',deployment_authorization_rejected:'Deployment authorization rejected',socket_unavailable:'Tool service unavailable',connection_refused:'Tool service refused connection',io_failure:'Connection interrupted',disconnected_response:'Connection interrupted',malformed_response:'Invalid tool response',response_too_large:'Tool response exceeded limit',response_id_mismatch:'Response identity mismatch',cancelled:'Execution cancelled',task_panicked:'Execution task failed',task_terminated:'Execution task terminated'};
  const detail=failure&&typeof failure.stage==='string'&&typeof failure.code==='string'&&Object.hasOwn(stages,failure.stage)&&Object.hasOwn(reasons,failure.code)?failure:null;
  const identifier=value=>typeof value==='string'&&value.length<=128&&/^[A-Za-z0-9_:.-]+$/.test(value)?value:'';
  const label=detail?reasons[detail.code]:labels[p.interruption?.reason];
  const help=text=>{const node=document.createElement('p');node.className='recovery-help';node.textContent=text;return node;};
  const showCode=(host,value)=>{const text=JSON.stringify(value,null,2);if(typeof code==='function')host.append(code(text));else{const pre=document.createElement('pre');pre.textContent=text;host.append(pre);}};
  // One open call: the parallel calls under review, or the run's single pending call.
  const calls=Array.isArray(run.uncertain_calls)?run.uncertain_calls.filter(call=>identifier(call?.call_id)):[];
  if(!calls.length&&run.pending)calls.push({call_id:identifier(p.call_id),name:p.name,arguments:p.arguments,interaction_id:p.id,single:true});
  const intro=document.createElement('p');intro.className='recovery-intro';
  intro.textContent=calls.length
    ?(calls.length===1?'A tool call was interrupted and may or may not have finished.':calls.length+' tool calls were interrupted and may or may not have finished.')+' Check what happened, then choose for each call.'
    :'The run stopped while tools were running. No call needs a decision: resume the run or end it.';
  root.append(intro);
  for(const call of calls){
    const section=document.createElement('div');section.className='recovery-call';
    const head=document.createElement('p');head.className='recovery-call-tool';
    const action=typeof call.arguments?.action==='string'&&call.arguments.action.length<=40?' · '+call.arguments.action:'';
    head.textContent=(identifier(call.name)||'Tool')+action;section.append(head);
    const known=document.createElement('textarea');known.placeholder='If you know what happened, describe it here';
    known.setAttribute('aria-label',call.single?'Known action result':'Known result for '+call.call_id);section.append(known);
    const row=document.createElement('div');row.className='recovery-actions';
    const callId=call.single?undefined:call.call_id;
    const again=button('Run again',()=>decide('retry',known.value,callId));
    const mine=button('Use my result',()=>decide('accept',known.value,callId));
    // "Use my result" needs the result; it stays unavailable until one is typed.
    const sync=()=>{mine.disabled=!(known.value||'').trim();};sync();known.addEventListener('input',sync);
    row.append(again,mine);section.append(row);
    section.append(help('Run again sends this exact call again. Use my result does not re-run it; your text becomes its result.'));
    const info=document.createElement('details');info.className='recovery-call-details';
    const summary=document.createElement('summary');summary.textContent='Call details';info.append(summary);
    if(call.arguments!=null)showCode(info,call.arguments);
    const ids=document.createElement('p');ids.className='recovery-reference';
    ids.textContent=(call.call_id?'Call: '+call.call_id:'Call: not recorded')+(identifier(call.interaction_id)?' · Interaction: '+call.interaction_id:'')+(identifier(call.transport_request_id)?' · Request: '+call.transport_request_id:'');
    info.append(ids);section.append(info);root.append(section);
  }
  const actions=document.createElement('div');actions.className='recovery-actions';
  if(!calls.length)actions.append(button('Resume run',()=>decide('retry','')));
  actions.append(button('End run',()=>decide('abandon','')));
  root.append(actions);
  root.append(help((calls.length?'':'Resume run continues the agent with interrupted calls marked as outcome unknown. ')+'End run stops this run; the agent does not continue.'));
  // Diagnostics stay out of the way until asked for; the expanded Set keeps them open across refreshes.
  const details=document.createElement('details');details.className='recovery-diagnostics';details.open=expanded.has(run.id);
  details.addEventListener('toggle',()=>{if(details.open)expanded.add(run.id);else expanded.delete(run.id);});
  const summary=document.createElement('summary');summary.textContent='Diagnostics';details.append(summary);
  const info=document.createElement('p');info.textContent='Reason: '+(label||'Not recorded for this interruption')+(detail?' · Stage: '+stages[detail.stage]:' · Detailed failure evidence unavailable')+(p.arguments?.timeout_seconds!=null?' · Timeout limit: '+p.arguments.timeout_seconds+' seconds':'');details.append(info);
  if(detail){const timing=document.createElement('p');const delivery={not_sent:'Request not sent',possibly_sent:'Request may have been sent',response_received:'Response received'};timing.textContent=(delivery[detail.delivery]||'Delivery unknown')+(Number.isSafeInteger(detail.elapsed_ms)&&detail.elapsed_ms>=0?' · '+detail.elapsed_ms+' ms':'')+(identifier(detail.tool)?' · Tool: '+detail.tool:'');details.append(timing);}
  const reference=document.createElement('p');reference.className='recovery-reference';reference.textContent='Run: '+run.id+(identifier(detail?.call_id)?' · Last failed call: '+detail.call_id:'');details.append(reference);
  root.append(details);
  return root;
}
