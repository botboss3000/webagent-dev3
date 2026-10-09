'use strict';
// V2 chat-ui/session-load landing: live rects, exact tail shortfall, local settle.
export function mountTranscriptAnchor({scroller,inner,header,getProfile}) {
  const abort=new AbortController();
  let focusedId=null,deadline=0,active=false,following=true,intentUntil=0,snapTimer=0,snapFallback=0,snapping=false,frame=0;
  const px=(value,fallback)=>Number.isFinite(Number(value))&&Number(value)>=0?Number(value):fallback;
  const config=()=>getProfile()?.user_turn_anchor||{};
  // Durable row IDs keep the cooldown across transcript reprojections. Only
  // reader movement rearms a consumed anchor; layout and smooth snaps cannot.
  const consumed={up:new Set(),down:new Set()};
  const setting=(name,fallback)=>px(config()[name],fallback);
  // Up approaches a pin from below (larger scrollTop); down from above.
  const directionSetting=(direction,name,fallback)=>px(config()[direction]?.[name],setting(name,fallback));
  const users=()=>Array.from(inner.querySelectorAll('.chat-bubble.user[data-public-id]'));
  const pad=()=>Math.max(px(config().landing_gap_px,62),header.getBoundingClientRect().bottom-scroller.getBoundingClientRect().top+setting('header_clearance_px',8));
  const target=row=>Math.max(0,scroller.scrollTop+row.getBoundingClientRect().top-scroller.getBoundingClientRect().top-pad());
  function reachable(top){const shortfall=top-Math.max(0,scroller.scrollHeight-scroller.clientHeight);if(shortfall>0){const extra=px(inner.style.getPropertyValue('--v3-anchor-extra'),0);inner.style.setProperty('--v3-anchor-extra',(extra+shortfall)+'px');void inner.offsetHeight;}}
  function finishSnap(){snapping=false;clearTimeout(snapFallback);snapFallback=0;}
  function release(){focusedId=null;following=false;clearTimeout(snapTimer);finishSnap();scroller.scrollTo({top:scroller.scrollTop,behavior:'instant'});intentUntil=performance.now()+setting('scroll_intent_ms',2000);}
  function land(){if(!focusedId||config().enabled===false)return;const row=users().find(row=>row.dataset.publicId===focusedId);if(!row)return;if(!active&&performance.now()>deadline){focusedId=null;return;}const top=target(row);reachable(top);scroller.scrollTop=top;consumed.up.add(row.dataset.publicId);consumed.down.add(row.dataset.publicId);}
  function changed(){cancelAnimationFrame(frame);frame=requestAnimationFrame(()=>{if(focusedId)land();else if(following)scroller.scrollTop=scroller.scrollHeight;});}
  function focus(row=users().at(-1)){if(!row||config().enabled===false){following=true;changed();return;}focusedId=row.dataset.publicId;deadline=performance.now()+setting('focus_hold_ms',6000);following=false;changed();}
  function reset(){release();consumed.up.clear();consumed.down.clear();inner.style.removeProperty('--v3-anchor-extra');following=true;active=false;intentUntil=0;}
  function rearm() {
    const current=scroller.scrollTop,max=Math.max(0,scroller.scrollHeight-scroller.clientHeight);
    // Keep the rearm boundary outside the capture margin even with overlapping
    // settings, so leaving the landing by a few pixels never recaptures it.
    for(const row of users()) {
      const offset=current-Math.min(max,target(row));
      for(const direction of ['up','down']) {
        const distance=Math.max(directionSetting(direction,'snap_rearm_distance_px',100),directionSetting(direction,'snap_distance_px',20));
        // Each side must leave its own boundary. Crossing the opposite side
        // neither consumes nor rearms this direction's cooldown.
        if((direction==='up'?offset:-offset)>=distance)consumed[direction].delete(row.dataset.publicId);
      }
    }
  }
  function settle() {
    if(config().enabled===false||config().snap_enabled===false||snapping)return;
    const current=scroller.scrollTop,max=Math.max(0,scroller.scrollHeight-scroller.clientHeight);
    let nearest=null;
    for(const row of users()) {
      const id=row.dataset.publicId;
      const top=Math.min(max,target(row)),distance=Math.abs(top-current);
      const direction=current>=top?'up':'down';
      if(consumed[direction].has(id)||config()[direction]?.snap_enabled===false)continue;
      if(distance<=directionSetting(direction,'snap_distance_px',20)&&(!nearest||distance<nearest.distance))nearest={top,distance,id,direction};
    }
    if(!nearest)return;
    consumed[nearest.direction].add(nearest.id);
    if(nearest.distance<=directionSetting(nearest.direction,'snap_tolerance_px',2))return;
    snapping=true;
    scroller.scrollTo({top:nearest.top,behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'instant':'smooth'});
    snapFallback=setTimeout(finishSnap,directionSetting(nearest.direction,'snap_complete_fallback_ms',1000));
  }
  for(const type of ['wheel','touchstart','pointerdown'])scroller.addEventListener(type,release,{passive:true,signal:abort.signal});
  scroller.addEventListener('keydown',event=>{if(['ArrowUp','ArrowDown','PageUp','PageDown','Home','End',' '].includes(event.key)&&!event.target.closest('input,textarea,[contenteditable=true]'))release();},{signal:abort.signal});
  scroller.addEventListener('scroll',()=>{if(snapping||performance.now()>intentUntil)return;rearm();following=scroller.scrollHeight-scroller.clientHeight-scroller.scrollTop<=setting('tail_follow_distance_px',12);clearTimeout(snapTimer);snapTimer=setTimeout(settle,px(config().snap_settle_ms,120));},{passive:true,signal:abort.signal});
  scroller.addEventListener('scrollend',finishSnap,{signal:abort.signal});
  const observer=new ResizeObserver(changed);observer.observe(inner);observer.observe(header);observer.observe(scroller);
  const tick=setInterval(()=>{if(focusedId)land();},300);
  return {reset,focus,changed,published(item){if(item.role==='user')focus();else changed();},updateRuns(items){active=items.some(run=>['queued','running','waiting_approval','waiting_gate'].includes(run.status));if(active&&focusedId)deadline=performance.now()+setting('focus_hold_ms',6000);},destroy(){reset();abort.abort();observer.disconnect();clearInterval(tick);cancelAnimationFrame(frame);}};
}
