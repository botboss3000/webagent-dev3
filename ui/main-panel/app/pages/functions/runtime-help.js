'use strict';
// Documentation is kept outside the strict runtime JSON so it cannot affect validation.
export const runtimeHelp = {
 'schema_version':{purpose:'Identifies the runtime configuration format. The supported version is 2.',used:'Configuration loading and migration (src/runtime_config.rs)',effect:'configuration load'},
 'scheduler.max_active_runs':{purpose:'Caps running agent runs across accounts. Blank or 0 removes this scheduler cap; 1–16 sets a limit. Account and queue limits still apply. Paused work may continue to hold a slot.',used:'Loop startup semaphore and queued-run dispatcher (src/app.rs; src/engine/loop/scheduler.rs)',effect:'loop restart'},
 'scheduler.max_unresolved_runs':{purpose:'Caps an account’s unfinished runs, including queued, active and paused work. Valid range: 1–200, and at least the positive concurrency limit.',used:'API message admission (src/api/chat.rs)',effect:'API restart'},
 'scheduler.poll_interval_ms':{purpose:'Milliseconds between queue scans. Lower values check for queued work more frequently. Valid range: 10–60,000.',used:'Loop dispatcher sleep interval (src/engine/loop/scheduler.rs)',effect:'loop restart'},
 'scheduler.recovery_interval_ms':{purpose:'Milliseconds between recovery scans for interrupted or expired run leases. Recovery still respects approvals and uncertain effects. Valid range: 1,000–3,600,000.',used:'Loop recovery scheduling (src/engine/loop/scheduler.rs; src/engine/loop/recovery.rs)',effect:'loop restart'},
 'context.max_context_bytes':{purpose:'Maximum logical JSON bytes allowed in model context. Valid range: 1,024–262,144. Model-specific ceilings may reduce it; this is a byte budget, not a provider token count.',used:'Model context budgeting and workflow checks (src/runtime_config.rs; src/engine/loop/run.rs; src/workflow.rs)',effect:'next context/model boundary'},
 'context.model_profiles':{purpose:'Optional estimates keyed by provider/model. Profiles can tighten the byte ceiling. An empty map uses the normal byte ceiling and built-in model limits.',used:'Model context budget calculation (src/runtime_config.rs)',effect:'next context/model boundary'},
 'context.model_profiles.*.context_tokens':{purpose:'Estimated total model context tokens, from 1 to 10,000,000. Must exceed output_reserve_tokens.',used:'Model context budget calculation (src/runtime_config.rs)',effect:'next context/model boundary'},
 'context.model_profiles.*.output_reserve_tokens':{purpose:'Tokens reserved for output within the estimated context window. This does not set the provider’s output-token limit.',used:'Model context budget calculation (src/runtime_config.rs)',effect:'next context/model boundary'},
 'context.model_profiles.*.bytes_per_token':{purpose:'Estimated logical bytes per token, from 1 to 8. Used only as a budgeting proxy.',used:'Model context budget calculation (src/runtime_config.rs)',effect:'next context/model boundary'},
 'cache.document_budget_bytes':{purpose:'Per-process retained document-cache byte budget, up to 268,435,456. Zero disables retained document caching; it does not delete stored data.',used:'Store document cache initialized at startup (src/app.rs; src/storage/cache.rs)',effect:'API and loop restart'},
 'cache.context_budget_bytes':{purpose:'Per-process retained context-cache byte budget, up to 67,108,864. Zero disables retained context caching; it does not delete conversation history.',used:'Store context cache initialized at startup (src/app.rs; src/storage/cache.rs)',effect:'API and loop restart'},
 'tools.max_parallel_calls':{purpose:'Maximum parallel tool calls, from 1 to 4. Tool-specific settings and permissions can impose a lower limit.',used:'Tool dispatch concurrency budget (src/engine/tools/dispatch.rs; src/tool_execution.rs)',effect:'next tool dispatch or batch'},
 'tools.parallel_modes':{purpose:'Legacy per-tool mode map accepting concurrent or reject. The current execution path uses the agent’s tool execution settings instead; this map is still validated but is not consulted by dispatch.',used:'Legacy configuration validation only (src/runtime_config.rs)',effect:'no execution change in the current dispatcher'},
 'workflow.max_turns':{purpose:'Maximum loop turns in a run, from 1 to 50. Reaching the limit stops further turns.',used:'Run workflow limit checks (src/engine/loop/run.rs; src/workflow.rs)',effect:'next workflow boundary'},
 'workflow.max_attempts':{purpose:'Maximum run attempts, from 1 to 3. Limits retries and recovery attempts without authorizing uncertain external writes.',used:'Run workflow limit checks (src/engine/loop/run.rs; src/workflow.rs)',effect:'next workflow boundary'},
 'workflow.max_publication_bytes':{purpose:'Maximum final assistant reply size in UTF-8 bytes, from 1 to 262,144. Oversized output cannot be published.',used:'Final reply validation (src/engine/output/publication.rs; src/engine/loop/run.rs)',effect:'next publication check'},
 'workflow.modules':{purpose:'Ordered gate modules grouped by workflow checkpoint. Only registered modules at supported checkpoints are accepted. Modules cannot bypass native permissions or approvals.',used:'Workflow policy and gate registry (src/workflow.rs; src/gates/mod.rs)',effect:'next workflow checkpoint'}
};
export function renderRuntimeReference(host, config) {
 host.replaceChildren();
 const note=document.createElement('p');note.textContent='Current saved values from config/loop-runtime.json. The five settings above are editable here; other values are shown for reference. Only scheduler concurrency treats blank or zero as unlimited.';host.append(note);
 const table=document.createElement('table');const header=document.createElement('tr');
 for(const text of ['Setting / saved value','Function','Where used / when applied']){const th=document.createElement('th');th.scope='col';th.textContent=text;header.append(th);}
 const thead=document.createElement('thead');thead.append(header);table.append(thead);
 const body=document.createElement('tbody');
 for(const [key,help] of Object.entries(runtimeHelp)){
  if(key.includes('.*.'))continue;
  const value=key.split('.').reduce((value,part)=>value?.[part],config);
  const row=document.createElement('tr');
  for(const text of [key+' = '+JSON.stringify(value),help.purpose,help.used+'. Applies: '+help.effect+'.']){const cell=document.createElement('td');cell.textContent=text;row.append(cell);}
  body.append(row);
  if(key==='context.model_profiles'){
   for(const subkey of ['context_tokens','output_reserve_tokens','bytes_per_token']){
    const subhelp=runtimeHelp[key+'.*.'+subkey],row=document.createElement('tr');
    for(const text of [key+'.<provider/model>.'+subkey,subhelp.purpose,subhelp.used+'. Applies: '+subhelp.effect+'.']){const cell=document.createElement('td');cell.textContent=text;row.append(cell);}
    body.append(row);
   }
  }
 }
 table.append(body);host.append(table);
}
