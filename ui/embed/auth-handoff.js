// Popup messages are the fast path; bounded polling recovers a lost notification.
// A single in-flight redemption preserves the server's one-use PKCE handoff.
export function watchAuthorization({redeem,connected,ended,isClosed,isExpired,setIntervalFn=setInterval,clearIntervalFn=clearInterval}){
 let stopped=false,inFlight=null,ticks=0,timer;
 function cancel(){stopped=true;clearIntervalFn(timer)}
 function attempt(final=false){
  if(stopped)return Promise.resolve();if(inFlight)return inFlight;
  inFlight=Promise.resolve().then(redeem).then(async value=>{if(stopped)return;cancel();try{await connected(value)}catch(error){ended(error)}}).catch(error=>{if(!stopped&&(final||isClosed()||isExpired())){cancel();ended(error)}}).finally(()=>{inFlight=null});return inFlight;
 }
 timer=setIntervalFn(()=>{if(stopped)return;if(isClosed()||isExpired())void attempt(true);else if(++ticks%10===0)void attempt()},1000);
 return {complete:()=>attempt(true),cancel};
}
