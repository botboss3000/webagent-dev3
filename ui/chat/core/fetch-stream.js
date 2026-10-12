// @code-index {"version":1,"feature":"chat-transport-recovery","aliases":["chat fetch stream","SSE reconnect parser","Last-Event-ID recovery"],"purpose":"Adapts the authenticated fetch event stream and fixed retry loop; the chat client owns sequence-gap snapshot recovery. No chat JSON key is consumed; consider bounded retry timing and backoff settings without changing recovery or publication invariants.","span_lines":40,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider a bounded retry delay/backoff setting; sequence recovery and publication behavior remain fixed."]}}
/**
 * Adapt fetch streaming to the EventSource-like interface consumed by the chat client.
 * @param {string} url Stream endpoint; headers supplies the request headers required by the endpoint.
 * @param {{headers?:Function,fetchFn?:Function,retryMs?:number}} options Injectable transport and fixed retry delay.
 * @returns {object} Listener source with close(); reconnects with Last-Event-ID until closed.
 * Side effects: reads the response stream, emits events, and owns one abort controller and retry timer.
 * Ordering: sequence-gap detection and snapshot recovery belong to the shared client, not this parser.
 */
export function createFetchStream(url,{headers=()=>({}),fetchFn=fetch,retryMs=1500}={}){
 const listeners=new Map(),abort=new AbortController();let timer,last='',closed=false;
 const source={onmessage:null,onerror:null,onopen:null,/* Register a callback for the named SSE event. */addEventListener(type,fn){if(!listeners.has(type))listeners.set(type,[]);listeners.get(type).push(fn)},/* Abort the current fetch and prevent any scheduled reconnect. */close(){closed=true;clearTimeout(timer);abort.abort()}};
 // Deliver a parsed event to both property handlers and registered listeners.
 const emit=(type,event)=>{source['on'+type]?.(event);for(const fn of listeners.get(type)||[])fn(event)};
 // Reopen after transport failures, carrying the most recent SSE id for server replay.
 async function connect(){try{
  const response=await fetchFn(url,{headers:{...headers(),...(last?{'Last-Event-ID':last}:{})},signal:abort.signal,credentials:'omit'});
  if(!response.ok||!response.body)throw Error('Chat stream unavailable');emit('open',{});
  const reader=response.body.getReader(),decoder=new TextDecoder();let pending='',data=[],event='message',id='';
  // Accumulate SSE fields until a blank line dispatches the current event.
  function line(value){if(value===''){if(data.length){if(id)last=id;emit(event,{data:data.join('\n'),lastEventId:last});}data=[];event='message';id='';return}if(value.startsWith(':'))return;
   const colon=value.indexOf(':'),field=colon<0?value:value.slice(0,colon),text=colon<0?'':value.slice(colon+1).replace(/^ /,'');
   if(field==='data')data.push(text);else if(field==='event')event=text;else if(field==='id'&&!text.includes('\0'))id=text;}
  while(!closed){const chunk=await reader.read();if(chunk.done)break;pending+=decoder.decode(chunk.value,{stream:true});if(pending.length>16*1024*1024)throw Error('Chat event too large');let end;while((end=pending.indexOf('\n'))>=0){line(pending.slice(0,end).replace(/\r$/,''));pending=pending.slice(end+1)}}
 }catch(error){if(closed||error.name==='AbortError')return}if(!closed){emit('error',{});timer=setTimeout(connect,retryMs)}}
 queueMicrotask(connect);return source;
}
