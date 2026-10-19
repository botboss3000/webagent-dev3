// @code-index {"version":1,"feature":"chat-recovery-details","aliases":["run recovery details","uncertain tool outcome","interrupted run details"],"purpose":"Renders inert detail text and explicit user choices for interrupted tool runs; the supplied callback and runtime retain recovery authority. No chat JSON key is consumed; only presentation preferences are candidates for configuration.","span_lines":40,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider recovery detail labels or disclosure preferences; recovery authorization and retry consent remain enforced."]}}
/**
 * Render the details and user decision controls for one interrupted tool run.
 * The expanded Set retains disclosure state by run ID; button and decide are
 * panel adapters; the decision callback submits choices to authenticated runtime recovery.
 * Arguments and interruption metadata are rendered as inert text; the warning keeps
 * uncertain effects visible so a retry is never implied by this presentation.
 * Returns a detached details element for the caller to attach to its run card.
 */
export function createRecoveryDetails(run,{expanded,button,decide}) {
  const details=document.createElement('details');details.className='recovery-details';details.open=expanded.has(run.id);
  details.addEventListener('toggle',()=>{if(details.open)expanded.add(run.id);else expanded.delete(run.id);});
  const p=run.pending||{},labels={timeout:'Timed out',cancelled:'Cancelled',transport_error:'Connection interrupted',uncertain:'Outcome unknown'};
  const failure=p.failure?.version===1?p.failure:(!run.pending&&run.tool_failure?.version===1?run.tool_failure:null);
  const stages={preparation:'Preparing execution',authorization:'Checking authorization',connect:'Connecting to tool service',request_write:'Sending request',response_read:'Reading response',response_validation:'Validating response',execution:'Executing tool',execution_task:'Running execution task'};
  const reasons={internal_error:'Internal failure',invalid_request:'Invalid request',authorization_rejected:'Authorization rejected',scope_changed:'Authorization scope changed',authority_stopped:'Authority stopped',legacy_authorization_rejected:'Broker authorization rejected',deployment_authorization_rejected:'Deployment authorization rejected',socket_unavailable:'Tool service unavailable',connection_refused:'Tool service refused connection',io_failure:'Connection interrupted',disconnected_response:'Connection interrupted',malformed_response:'Invalid tool response',response_too_large:'Tool response exceeded limit',response_id_mismatch:'Response identity mismatch',cancelled:'Execution cancelled',task_panicked:'Execution task failed',task_terminated:'Execution task terminated'};
  const detail=failure&&typeof failure.stage==='string'&&typeof failure.code==='string'&&Object.hasOwn(stages,failure.stage)&&Object.hasOwn(reasons,failure.code)?failure:null;
  const identifier=value=>typeof value==='string'&&value.length<=128&&/^[A-Za-z0-9_:.-]+$/.test(value)?value:'';
  const label=detail?reasons[detail.code]:labels[p.interruption?.reason];
  const summary=document.createElement('summary');summary.textContent=(p.name||identifier(detail?.tool)||'Tool')+' — '+(label||'Outcome unknown')+' · Details';details.append(summary);
  const warning=document.createElement('p');warning.textContent='The action may have completed. Verify its outcome before authorizing a retry.';details.append(warning);
  const info=document.createElement('p');info.textContent='Reason: '+(label||'Not recorded for this interruption')+(detail?' · Stage: '+stages[detail.stage]:' · Detailed failure evidence unavailable')+(p.arguments?.timeout_seconds!=null?' · Timeout limit: '+p.arguments.timeout_seconds+' seconds':'');details.append(info);
  if(detail){const timing=document.createElement('p');const delivery={not_sent:'Request not sent',possibly_sent:'Request may have been sent',response_received:'Response received'};timing.textContent=(delivery[detail.delivery]||'Delivery unknown')+(Number.isSafeInteger(detail.elapsed_ms)&&detail.elapsed_ms>=0?' · '+detail.elapsed_ms+' ms':'');details.append(timing);}
  if(p.arguments){const args=document.createElement('pre');args.textContent=JSON.stringify(p.arguments,null,2);details.append(args);}
  const reference=document.createElement('p');reference.className='recovery-reference';reference.textContent='Run: '+run.id+((p.call_id||identifier(detail?.call_id))?' · Call: '+(p.call_id||identifier(detail?.call_id)):'');details.append(reference);
  const output=document.createElement('textarea');output.placeholder='Known result to accept';output.setAttribute('aria-label','Known action result');details.append(output);
  const actions=document.createElement('div');actions.className='recovery-actions';
  for(const decision of ['accept','retry','abandon'])actions.append(button({accept:'Accept known result',retry:'Authorize retry',abandon:'Abandon run'}[decision],()=>decide(decision,output.value)));
  details.append(actions);return details;
}
