'use strict';
// Agent-local budgets use the roster revision fence and are pinned by new runs.
// The expanded model rows edit them in place; this owns their data, defaults and save rules.
const USAGE='Input budget controls estimated space for the complete model request, including instructions, history and tools. The compaction trigger counts the whole request, including instructions, tool definitions, history and the verbatim tail; only eligible older history is summarized. Automatic compaction starts at the earlier of the agent’s trigger or the effective System model input budget. Summary chunks must also fit that budget. Thresholds and budgets are in provider tokens. Each model call converts them with the bytes per token measured from the previous call’s reported usage; before any call has reported usage, it uses this model’s token estimate.';
const SCOPE='Maximum output tokens is sent as a cap on generated output. Input plus output must fit this connection’s context window. These agent-local settings are pinned for new runs and apply to chat, summaries, naming and review calls. Defaults reserve 8,192 tokens for output. Clear a value to restore its default.';
export async function mountModelLimits(adapter) {
  let data;
  const find=id=>data?.models.find(m=>m.id===id);
  const defaults=m=>({input:m.context_tokens?m.context_tokens-m.default_output_tokens:null,output:m.default_output_tokens??null});
  const providerText=m=>m.context_tokens?((m.transport==='openai_subscription'?'Subscription context: ':'API context: ')+m.context_tokens.toLocaleString()+' tokens · published API max output: '+m.max_output_tokens.toLocaleString()+' tokens. '+(m.transport==='openai_subscription'?'Public API context is '+m.published_context_tokens.toLocaleString()+' tokens. ':'')+'Input plus output must fit this connection’s context window.'):'Provider limits are not known. Enter limits supported by this model.';
  function budget(id){
    const m=find(id);if(!m)return null;const d=defaults(m);
    return {input:m.limits?.input_tokens??d.input,output:m.limits?.output_tokens??d.output,
      ratio:m.characters_per_token?m.characters_per_token+' bytes per token':'4 bytes per token (default)',
      help:{wide:true,html:[providerText(m),USAGE,SCOPE].map(t=>'<p>'+t+'</p>').join('')}};
  }
  async function reload(){data=await adapter.loadModelLimits();}
  // `value` null restores that field's default; both at default stores no override.
  async function save(id,field,value){
    const m=find(id);if(!m)throw new Error('Model limits are unavailable');
    const d=defaults(m),now=budget(id),next={input:now.input,output:now.output,[field]:value??d[field]};
    if(!next.input||!next.output)throw new Error('Enter limits supported by this model');
    if(next.input>(m.context_tokens??10000000)||next.output>(m.max_output_tokens??1000000))throw new Error('Above this model’s limit');
    if(m.context_tokens&&next.input+next.output>m.context_tokens)throw new Error('Input plus output exceeds this connection’s context window');
    const limits=next.input===d.input&&next.output===d.output?null:{input_tokens:next.input,output_tokens:next.output};
    await adapter.saveModelLimits(id,limits);await reload();
  }
  await reload();return {reload,budget,save};
}
