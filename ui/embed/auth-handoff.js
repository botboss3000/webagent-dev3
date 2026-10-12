// @code-index {"version":1,"feature":"chat-embed-auth","aliases":["embedded chat authorization","PKCE popup handoff","embed sign in"],"purpose":"Locates single-flight PKCE redemption, popup completion, bounded polling, and terminal popup/expiry handling.","span_lines":40,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"The one-use authorization handoff and popup/expiry boundaries are security invariants, not presentation preferences."}}
// Popup messages are the fast path; bounded polling recovers a lost notification.
// A single in-flight redemption preserves the server's one-use PKCE handoff.
/** Watches a popup authorization challenge. One redemption runs at a time; complete triggers immediate redemption, while one-second polling checks closure/expiry and retries every tenth tick. Returns complete/cancel controls. */
export function watchAuthorization({redeem,connected,ended,isClosed,isExpired,setIntervalFn=setInterval,clearIntervalFn=clearInterval}){
 let stopped=false,inFlight=null,ticks=0,timer;
 // Purpose: Stop future polling and redemption attempts.
 function cancel(){stopped=true;clearIntervalFn(timer)}
 // Purpose: Redeem once at a time; report errors only on explicit completion or terminal popup/expiry state.
 function attempt(final=false){
  if(stopped)return Promise.resolve();if(inFlight)return inFlight;
  inFlight=Promise.resolve().then(redeem).then(async value=>{if(stopped)return;cancel();try{await connected(value)}catch(error){ended(error)}}).catch(error=>{if(!stopped&&(final||isClosed()||isExpired())){cancel();ended(error)}}).finally(()=>{inFlight=null});return inFlight;
 }
 timer=setIntervalFn(()=>{if(stopped)return;if(isClosed()||isExpired())void attempt(true);else if(++ticks%10===0)void attempt()},1000);
 // Methods: complete requests immediate redemption; cancel stops polling without starting another request.
 return {complete:()=>attempt(true),cancel};
}
