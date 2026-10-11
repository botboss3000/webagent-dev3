// Header-authenticated SSE; the shared client owns sequence recovery.
export function createFetchStream(url,{headers=()=>({}),fetchFn=fetch,retryMs=1500}={}){
 const listeners=new Map(),abort=new AbortController();let timer,last='',closed=false;
 const source={onmessage:null,onerror:null,onopen:null,addEventListener(type,fn){if(!listeners.has(type))listeners.set(type,[]);listeners.get(type).push(fn)},close(){closed=true;clearTimeout(timer);abort.abort()}};
 const emit=(type,event)=>{source['on'+type]?.(event);for(const fn of listeners.get(type)||[])fn(event)};
 async function connect(){try{
  const response=await fetchFn(url,{headers:{...headers(),...(last?{'Last-Event-ID':last}:{})},signal:abort.signal,credentials:'omit'});
  if(!response.ok||!response.body)throw Error('Chat stream unavailable');emit('open',{});
  const reader=response.body.getReader(),decoder=new TextDecoder();let pending='',data=[],event='message',id='';
  function line(value){if(value===''){if(data.length){if(id)last=id;emit(event,{data:data.join('\n'),lastEventId:last});}data=[];event='message';id='';return}if(value.startsWith(':'))return;
   const colon=value.indexOf(':'),field=colon<0?value:value.slice(0,colon),text=colon<0?'':value.slice(colon+1).replace(/^ /,'');
   if(field==='data')data.push(text);else if(field==='event')event=text;else if(field==='id'&&!text.includes('\0'))id=text;}
  while(!closed){const chunk=await reader.read();if(chunk.done)break;pending+=decoder.decode(chunk.value,{stream:true});if(pending.length>16*1024*1024)throw Error('Chat event too large');let end;while((end=pending.indexOf('\n'))>=0){line(pending.slice(0,end).replace(/\r$/,''));pending=pending.slice(end+1)}}
 }catch(error){if(closed||error.name==='AbortError')return}if(!closed){emit('error',{});timer=setTimeout(connect,retryMs)}}
 queueMicrotask(connect);return source;
}
