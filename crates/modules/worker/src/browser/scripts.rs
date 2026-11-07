//! The fixed native page scripts, byte-identical to the Python Browser Control adapter they
//! replace. Model text only ever reaches them as arguments, never as script source.

/// Rendered text of a selector's outermost matches, credentials redacted.
pub const READ: &str = r#"
/* V3_READ */
// Every match of the selector, outermost only, in document order; rendered text only.
const roots=[...document.querySelectorAll(arguments[0]||'body')].filter((node,i,all)=>!all.some(other=>other!==node&&other.contains(node)));
if (!roots.length) return {text:'',missing:true,truncated:false,matches:0};
const offset=arguments[3],end=offset+arguments[2];
let text='',count=0,truncated=false;
for (const root of roots) {
 const walk=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);let node;
 while ((node=walk.nextNode())) {
  if (++count>20000) {truncated=true;break;}
  const parent=node.parentElement;
  if (!parent || parent.closest('script,style,input,textarea,[data-secret],[data-sensitive]')) continue;
  // checkVisibility also covers hidden, display:none and visibility:hidden ancestors.
  if (!parent.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})) continue;
  const chunk=node.nodeValue.replaceAll(arguments[1],'[redacted]').trim();
  if (!chunk) continue;
  text+=chunk+'\n';
  if (text.length>end) {truncated=true;break;}
 }
 if (truncated) break;
}
return {text:text.slice(offset,end),missing:false,truncated:truncated,matches:roots.length,
 next_offset:truncated?Math.min(end,text.length):null};
"#;

/// Layout facts for rendered matches; page-level checks first.
pub const MEASURE: &str = r#"
/* V3_MEASURE */
// Rendered matches only. `checks` sorts first in results, so its page-level facts survive any cut.
const sel=arguments[0],limit=arguments[1],offset=arguments[2],withStyles=arguments[3];
const vw=innerWidth,vh=innerHeight,r=v=>Math.round(v*10)/10,root=document.documentElement;
const all=[...document.querySelectorAll(sel)];
const nodes=all.filter(n=>n.checkVisibility({checkVisibilityCSS:true}));
const props=['display','position','zIndex','fontSize','fontWeight','lineHeight','color','backgroundColor','borderRadius',
 'borderTopWidth','paddingTop','paddingRight','paddingBottom','paddingLeft','marginTop','marginRight','marginBottom','marginLeft',
 'overflowX','overflowY','opacity','textOverflow','whiteSpace'];
const label=n=>(n.closest('[data-secret],[data-sensitive]')||n.matches('input,textarea,select'))?'':
 (n.getAttribute('aria-label')||n.innerText||n.getAttribute('title')||'').trim().replace(/\s+/g,' ').slice(0,60);
const rows=nodes.map(n=>{const b=n.getBoundingClientRect();
 return {node:n,tag:n.tagName.toLowerCase(),id:n.id||null,label:label(n),box:{x:r(b.x),y:r(b.y),width:r(b.width),height:r(b.height)},
  in_viewport:b.left>=-0.5&&b.top>=-0.5&&b.right<=vw+0.5&&b.bottom<=vh+0.5,
  overflow:{x:Math.max(0,n.scrollWidth-n.clientWidth),y:Math.max(0,n.scrollHeight-n.clientHeight)}};});
const brief=x=>({label:x.label,id:x.id,width:x.box.width,height:x.box.height});
const small=rows.filter(x=>x.box.width<44||x.box.height<44).sort((a,b)=>Math.min(a.box.width,a.box.height)-Math.min(b.box.width,b.box.height));
const clipped=rows.filter(x=>x.overflow.x>0||x.overflow.y>0);
const page=rows.slice(offset,offset+limit).map(x=>{const out={tag:x.tag,id:x.id,label:x.label,box:x.box,in_viewport:x.in_viewport,overflow:x.overflow};
 if(withStyles){const st=getComputedStyle(x.node),style={};for(const p of props)style[p]=st[p];out.style=style;}return out;});
return {checks:{viewport:{width:vw,height:vh},page_overflow_x:Math.max(0,root.scrollWidth-root.clientWidth),
  matches:all.length,rendered:rows.length,under_44px:{count:small.length,smallest:small.slice(0,5).map(brief)},
  outside_viewport:rows.filter(x=>!x.in_viewport).length,content_overflowing:{count:clipped.length,first:clipped.slice(0,5).map(brief)}},
 elements:page,next_offset:offset+limit<rows.length?offset+limit:null};
"#;

/// Running animations and transitions, optionally under a selector.
pub const ANIMATIONS: &str = r#"
/* V3_ANIMATIONS */
const scope=arguments[0]?[...document.querySelectorAll(arguments[0])]:null;
if (scope&&!scope.length) return {missing:true,count:0,animations:[]};
const list=scope?[...new Set(scope.flatMap(n=>n.getAnimations({subtree:true})))]:document.getAnimations();
const name=n=>!n?'':n.tagName.toLowerCase()+(n.id?'#'+n.id:'')+(typeof n.className==='string'&&n.className.trim()?'.'+n.className.trim().split(/\s+/).slice(0,2).join('.'):'');
return {missing:false,count:list.length,animations:list.slice(0,arguments[1]).map(a=>{
 const e=a.effect,t=e&&e.getTiming?e.getTiming():{},c=e&&e.getComputedTiming?e.getComputedTiming():{};
 return {type:a.constructor.name,name:a.animationName||a.transitionProperty||a.id||'',target:name(e&&e.target),
  play_state:a.playState,duration_ms:typeof t.duration==='number'?t.duration:null,delay_ms:t.delay||0,easing:t.easing||'',
  iterations:t.iterations===Infinity?'infinite':t.iterations,progress:c.progress==null?null:Math.round(c.progress*1000)/1000,
  current_ms:a.currentTime==null?null:Math.round(a.currentTime)};})};
"#;

/// One poll of a `wait_for` condition.
pub const CHECK: &str = r#"
/* V3_CHECK */
// One poll of a wait condition; the first call starts the page-change clock.
if (!window.__v3Quiet) {
 window.__v3Quiet={last:performance.now()};
 new MutationObserver(()=>{window.__v3Quiet.last=performance.now();}).observe(document.documentElement,
  {subtree:true,childList:true,attributes:true,characterData:true});
}
const condition=arguments[0],selector=arguments[1],text=arguments[2],quiet=arguments[3],now=performance.now();
const pick=()=>[...document.querySelectorAll(selector||'body')];
const shown=n=>n.checkVisibility({checkOpacity:true,checkVisibilityCSS:true});
if (condition==='visible') return pick().some(shown);
if (condition==='hidden') return !pick().some(shown);
if (condition==='text') return pick().some(n=>shown(n)&&n.innerText.includes(text));
if (condition==='quiet') return now-window.__v3Quiet.last>=quiet;
// Endless animations such as spinners never finish, so only finite running ones count.
if (condition==='animations_done') return (selector?pick().flatMap(n=>n.getAnimations({subtree:true})):document.getAnimations())
 .every(a=>a.playState!=='running'||(a.effect&&a.effect.getTiming().iterations===Infinity));
if (condition==='network_idle') {
 const last=Math.max(0,...performance.getEntriesByType('resource').map(e=>e.responseEnd));
 return document.readyState==='complete'&&now-last>=quiet;
}
return false;
"#;

/// The app's boot stage: `ready`, `signed_out` or `booting`.
pub const READY: &str = r#"
/* V3_BOOT */
// The app's own boot signals (ui/boot/boot.js): revealed once the app is ready, curtain removed after.
const shown=n=>n&&n.checkVisibility({checkOpacity:true,checkVisibilityCSS:true});
if (shown(document.querySelector('input[name=password]'))) return 'signed_out';
if (window.__boot&&window.__boot.revealed&&!document.getElementById('boot-curtain')) return 'ready';
return 'booting';
"#;

/// Scrolls the target's nearest scroll container, or the page.
pub const SCROLL: &str = r#"
/* V3_SCROLL */
// The app scrolls inside panels, so act on the target's nearest scroll container and say which it was.
const el=arguments[0]?document.querySelector(arguments[0]):null,dx=arguments[1],dy=arguments[2];
if (arguments[0]&&!el) return {missing:true};
const scroller=n=>{for(let x=n;x&&x!==document.body&&x!==document.documentElement;x=x.parentElement){
 const st=getComputedStyle(x);if(/(auto|scroll)/.test(st.overflowY+' '+st.overflowX)&&(x.scrollHeight>x.clientHeight||x.scrollWidth>x.clientWidth))return x;}
 return document.scrollingElement;};
const s=el?scroller(el):document.scrollingElement;
if (el&&!dx&&!dy) el.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'});
else s.scrollBy({left:dx,top:dy,behavior:'instant'});
const name=n=>n===document.scrollingElement?'page':n.tagName.toLowerCase()+(n.id?'#'+n.id:'')+(typeof n.className==='string'&&n.className.trim()?'.'+n.className.trim().split(/\s+/)[0]:'');
const b=el?el.getBoundingClientRect():null;
return {missing:false,container:name(s),scroll_x:Math.round(s.scrollLeft),scroll_y:Math.round(s.scrollTop),
 max_x:Math.max(0,s.scrollWidth-s.clientWidth),max_y:Math.max(0,s.scrollHeight-s.clientHeight),
 target_box:b?{x:Math.round(b.x),y:Math.round(b.y),width:Math.round(b.width),height:Math.round(b.height)}:null};
"#;

/// The focused element, with whether it is a credential field.
pub const FOCUS: &str = r#"
/* V3_FOCUS */
const n=document.activeElement;
if (!n||n===document.body) return {tag:'body',id:null,label:'',secret:false};
const secret=n.matches('input[type=password],input[type=file]')||n.closest('[data-secret],[data-sensitive]')!==null||
 /password|secret|token|credential|api.?key/i.test([n.id,n.name,n.autocomplete].join(' '));
return {tag:n.tagName.toLowerCase(),id:n.id||null,label:(n.getAttribute('aria-label')||(n.matches('input,textarea,select')?'':n.innerText)||'').trim().slice(0,60),secret:secret};
"#;

/// Browser-recorded load milestones for the current document.
pub const PERFORMANCE: &str = r#"
/* V3_PERFORMANCE */
// Browser-recorded milestones for the current document, in ms from the start of navigation.
return new Promise(resolve=>{
 const nav=performance.getEntriesByType('navigation')[0],paint=Object.fromEntries(performance.getEntriesByType('paint').map(e=>[e.name,e.startTime]));
 const found={lcp:null,cls:0,long:0,longest:0},r=v=>v==null?null:Math.round(v);
 const watch=(type,fn)=>{try{new PerformanceObserver(list=>list.getEntries().forEach(fn)).observe({type,buffered:true});}catch(error){}};
 watch('largest-contentful-paint',e=>{found.lcp=e.startTime;});
 watch('layout-shift',e=>{if(!e.hadRecentInput)found.cls+=e.value;});
 watch('longtask',e=>{found.long++;found.longest=Math.max(found.longest,e.duration);});
 setTimeout(()=>resolve({url_path:location.pathname,since_navigation_ms:r(performance.now()),
  time_to_first_byte_ms:r(nav&&nav.responseStart),dom_content_loaded_ms:r(nav&&nav.domContentLoadedEventEnd),load_ms:r(nav&&nav.loadEventEnd||null),
  first_paint_ms:r(paint['first-paint']),first_contentful_paint_ms:r(paint['first-contentful-paint']),largest_contentful_paint_ms:r(found.lcp),
  layout_shift_score:Math.round(found.cls*1000)/1000,long_tasks:found.long,longest_task_ms:r(found.longest),
  resources:performance.getEntriesByType('resource').length}),100);
});
"#;

/// The centre of the first rendered match.
pub const CENTER: &str = r#"
/* V3_CENTER */
const n=[...document.querySelectorAll(arguments[0])].find(x=>x.checkVisibility({checkVisibilityCSS:true}));
if (!n) return null;
const b=n.getBoundingClientRect();
return {x:Math.round(b.x+b.width/2),y:Math.round(b.y+b.height/2)};
"#;

/// Re-checks an input target at dispatch against the region last read.
pub const TARGET: &str = r#"
/* V3_TARGET */
const credential=n=>n.matches('input')&&['password','email','text','tel','number',''].includes(n.getAttribute('type')||'')&&
 (n.type==='password'||n.type==='email'||/user|e-?mail|login|otp|one.?time|code|pass/i.test([n.id,n.name,n.autocomplete].join(' ')));
const target=document.querySelector(arguments[0]);
// The target must lie inside a region the agent read; other parts of the page may change freely.
const observed=[...document.querySelectorAll(arguments[1]||'body')];
if (!target||!target.isConnected||!observed.some(region=>region.contains(target))) return {allowed:false};
const secret=target.matches('input[type=password],input[type=file]') || target.closest('[data-secret],[data-sensitive]')!==null ||
 /password|secret|token|credential|api.?key/i.test([target.id,target.name,target.autocomplete].join(' '));
// The visible label (never an input value) lets the result say what was actually used.
const label=(target.getAttribute('aria-label')||target.innerText||'').trim().slice(0,80);
return {allowed:true,secret:secret,credential:credential(target),href:target.closest('a[href]')?.href||'',tag:target.tagName.toLowerCase(),label:label};
"#;

/// Whether an element is a sign-in field a vault value may go into.
pub const CREDENTIAL: &str = r#"
/* V3_CREDENTIAL */
const credential=n=>n.matches('input')&&['password','email','text','tel','number',''].includes(n.getAttribute('type')||'')&&
 (n.type==='password'||n.type==='email'||/user|e-?mail|login|otp|one.?time|code|pass/i.test([n.id,n.name,n.autocomplete].join(' ')));
return credential(arguments[0]);
"#;

/// Whether a visible password field asks the user to sign in.
pub const SIGNIN: &str = r#"
/* V3_SIGNIN */
// A visible password field means the site is asking to sign in.
return [...document.querySelectorAll('input[type=password]')].some(n=>n.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}));
"#;

/// Covers credentials and editable content before a capture.
pub const MASK: &str = r#"
/* V3_MASK */
const masks=[];
const candidates=[...document.querySelectorAll('input,textarea,[data-secret],[data-sensitive]')];
const walk=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT);
let node,count=0;
while ((node=walk.nextNode()) && ++count<=5000) {
 if ((node.nodeValue.includes(arguments[0]) || (arguments[1] && node.nodeValue.includes(arguments[1]))) && node.parentElement) candidates.push(node.parentElement);
}
for (const node of candidates.slice(0,2000)) {
 const rect=node.getBoundingClientRect();
 if (!rect.width||!rect.height) continue;
 const mask=document.createElement('div');
 mask.setAttribute('data-v3-private-mask','1');
 Object.assign(mask.style,{position:'fixed',left:rect.left+'px',top:rect.top+'px',
  width:rect.width+'px',height:rect.height+'px',background:'#555',zIndex:'2147483647'});
 document.documentElement.appendChild(mask);masks.push(mask);
}
return masks.length;
"#;

/// Removes the capture masks.
pub const UNMASK: &str = r#"
/* V3_UNMASK */
document.querySelectorAll('[data-v3-private-mask]').forEach(node=>node.remove());
"#;
