"""Private fixed-origin browser execution; Rust owns authentication and approval.

Trusted cookie metadata is a separate dispatcher field, never a model argument.
There is no caller-supplied JavaScript, origin, profile or filesystem path.
"""
from __future__ import annotations
import asyncio
import base64
import contextlib
import hashlib
import json
import logging
import os
import re
import shutil
import signal
import stat
import struct
import subprocess
import threading
import time
import uuid
import zlib
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

ORIGIN=os.environ.get("V3_PUBLIC_ORIGIN","https://dev.webagent.live")
PRIVATE_ROOT=Path("/run/v3/worker/browser")
# One read page; small enough that the result is never cut off in the agent's context.
MAX_TEXT=12000
MAX_READ_OFFSET=1_000_000
MAX_PNG=1024*1024
MAX_ARTIFACTS=40
MAX_SESSIONS=2
MAX_PRIVATE_SESSIONS=32
MAX_ARTIFACT_BYTES=64*1024*1024
IDLE_SECONDS=180
READ_FRESH_SECONDS=30
PAGE_TIMEOUT=10
CALL_TIMEOUT=20
# URL parameters the app itself writes. Invite and embed authentication parameters stay excluded.
APP_PARAMS={"tab","agent","agentView","agentScope","appView","session_id","article","path","job_id","theme","window"}
# Presets with a touch screen also emulate mobile media queries.
VIEWPORTS={"phone":(390,844),"phone_small":(360,640),"tablet":(768,1024),"desktop":(1280,900),"desktop_wide":(1440,900)}
TOUCH_VIEWPORTS={"phone","phone_small","tablet"}
# Cloudflare injects its analytics beacon at the edge; the app's own CSP blocks it. Not an app error.
IGNORED_PAGE_ERRORS=("static.cloudflareinsights.com",)
MAX_PAGE_ERRORS=20
MAX_ELEMENTS=50
MEASURE_PAGE=20
MAX_PIXELS=20
MAX_SCROLL=20000
WAIT_CONDITIONS={"visible","hidden","text","quiet","animations_done","network_idle"}
# WebDriver key codes; only navigation and activation keys, never text or shortcuts.
KEYS={"Tab":"\ue004","Shift+Tab":"\ue008\ue004","Enter":"\ue007","Escape":"\ue00c","Space":" ",
    "ArrowUp":"\ue013","ArrowDown":"\ue015","ArrowLeft":"\ue012","ArrowRight":"\ue014",
    "Home":"\ue011","End":"\ue010","PageUp":"\ue00e","PageDown":"\ue00f"}
COLOR_SCHEMES={"light":"light","dark":"dark","default":""}
REDUCED_MOTION={"reduce":"reduce","default":""}

# Fixed native scripts only. Model text can enter a CSS selector or literal
# send_keys argument, never script source. Every browser output is untrusted.
READ_SCRIPT=r"""
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
"""
MEASURE_SCRIPT=r"""
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
"""
ANIMATIONS_SCRIPT=r"""
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
"""
CHECK_SCRIPT=r"""
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
"""
SCROLL_SCRIPT=r"""
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
"""
FOCUS_SCRIPT=r"""
/* V3_FOCUS */
const n=document.activeElement;
if (!n||n===document.body) return {tag:'body',id:null,label:'',secret:false};
const secret=n.matches('input[type=password],input[type=file]')||n.closest('[data-secret],[data-sensitive]')!==null||
 /password|secret|token|credential|api.?key/i.test([n.id,n.name,n.autocomplete].join(' '));
return {tag:n.tagName.toLowerCase(),id:n.id||null,label:(n.getAttribute('aria-label')||(n.matches('input,textarea,select')?'':n.innerText)||'').trim().slice(0,60),secret:secret};
"""
PERFORMANCE_SCRIPT=r"""
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
"""
CENTER_SCRIPT=r"""
/* V3_CENTER */
const n=[...document.querySelectorAll(arguments[0])].find(x=>x.checkVisibility({checkVisibilityCSS:true}));
if (!n) return null;
const b=n.getBoundingClientRect();
return {x:Math.round(b.x+b.width/2),y:Math.round(b.y+b.height/2)};
"""
TARGET_SCRIPT="""
/* V3_TARGET */
const target=document.querySelector(arguments[0]);
// The target must lie inside a region the agent read; other parts of the page may change freely.
const observed=[...document.querySelectorAll(arguments[1]||'body')];
if (!target||!target.isConnected||!observed.some(region=>region.contains(target))) return {allowed:false};
const secret=target.matches('input[type=password],input[type=file]') || target.closest('[data-secret],[data-sensitive]')!==null ||
 /password|secret|token|credential|api.?key/i.test([target.id,target.name,target.autocomplete].join(' '));
// The visible label (never an input value) lets the result say what was actually used.
const label=(target.getAttribute('aria-label')||target.innerText||'').trim().slice(0,80);
return {allowed:true,secret:secret,href:target.closest('a[href]')?.href||'',tag:target.tagName.toLowerCase(),label:label};
"""
MASK_SCRIPT="""
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
"""
UNMASK_SCRIPT="""
/* V3_UNMASK */
document.querySelectorAll('[data-v3-private-mask]').forEach(node=>node.remove());
"""

class Rejected(Exception):pass
class Uncertain(Exception):pass
class StartupFailure(Rejected):pass
class AuthenticationFailure(Rejected):pass
class CapacityFailure(Rejected):pass
def canonical_id(value):
    """Require canonical UUID spelling for private session and artifact ownership."""
    try:return isinstance(value,str) and str(uuid.UUID(value))==value
    except (ValueError,AttributeError):return False
def validate_trusted(base,trusted):
    """Validate worker-supplied current-caller authority and the bounded authentication lease."""
    if not isinstance(trusted,dict):raise Rejected("Trusted browser authorization required")
    sid=trusted.get("session_id")
    if not canonical_id(sid) or Path(base).name!=sid:raise Rejected("Browser session ownership mismatch")
    if not isinstance(trusted.get("user_id"),str) or not re.fullmatch(r"[a-z0-9_-]{1,64}",trusted["user_id"]):
        raise Rejected("Invalid trusted caller")
    if not isinstance(trusted.get("agent_id"),str) or not re.fullmatch(r"[a-z0-9_-]{1,64}",trusted["agent_id"]):
        raise Rejected("Invalid trusted agent")
    if not canonical_id(trusted.get("cookie")):raise Rejected("Invalid trusted authentication")
    if not isinstance(trusted.get("generation"),str) or not 1<=len(trusted["generation"])<=128:
        raise Rejected("Invalid trusted generation")
    expires=trusted.get("expires_at")
    if type(expires) not in (int,float) or not time.time()<expires<=time.time()+601:
        raise Rejected("Browser authorization expired or exceeds lease")
    if trusted.get("allowed_session_ids")!=[sid]:raise Rejected("Browser routes require the current session")
    return dict(trusted)
def validate_path(path,trusted):
    """Confine navigation to the fixed development origin and implemented current-session routes."""
    if not isinstance(path,str) or not 1<=len(path)<=2048 or any(ord(c)<32 for c in path) or "\\" in path:
        raise Rejected("Invalid development route")
    parsed=urlsplit(path)
    if parsed.scheme or parsed.netloc or parsed.fragment or not parsed.path.startswith("/") or "%" in parsed.path:
        raise Rejected("Only exact relative development routes are supported")
    if "//" in parsed.path or any(part in (".","..") for part in parsed.path.split("/")):
        raise Rejected("Invalid development route")
    pairs=parse_qsl(parsed.query,keep_blank_values=True)
    if len(pairs)!=len(dict(pairs)):raise Rejected("Duplicate route parameters")
    query=dict(pairs)
    if parsed.path=="/app":
        if set(query)-APP_PARAMS:raise Rejected("Unsupported app route parameters")
        if any(not 1<=len(value)<=256 for value in query.values()):raise Rejected("Invalid app route parameter")
        if not re.fullmatch(r"[a-z0-9-]{1,40}",query.get("tab","chat")):raise Rejected("Unsupported development page")
        if "session_id" in query and query["session_id"]!=trusted["session_id"]:raise Rejected("Another chat session is unavailable")
        if "agent" in query and query["agent"]!=trusted["agent_id"]:raise Rejected("Another agent is unavailable")
        if not re.fullmatch(r"[a-z0-9_-]{1,40}",query.get("agentView","config")):raise Rejected("Unsupported agent view")
        # Avoid an unconstrained restored conversation on a bare app route.
        query.setdefault("session_id",trusted["session_id"])
        return urlunsplit(("","",parsed.path,urlencode(query),""))
    if query:raise Rejected("API navigation does not accept query parameters")
    if parsed.path=="/api/v3/auth/me":return parsed.path
    prefix="/api/v3/sessions/"+trusted["session_id"]
    if parsed.path in {prefix,prefix+"/abilities",prefix+"/mode"}:return parsed.path
    if re.fullmatch(re.escape(prefix)+r"/runs/[a-f0-9-]{36}/diagnostics",parsed.path):
        if canonical_id(parsed.path.split("/")[-2]):return parsed.path
    raise Rejected("Unsupported current-session read route")
def validate_args(name,args):
    """Reject unknown or action-inapplicable fields before browser effects."""
    if not isinstance(args,dict):raise Rejected("Browser arguments must be an object")
    if name in {"login_as_current_user","login_as_current_user_v3"}:
        allowed={"path"};action="login"
    elif name=="dev_browser_action":
        allowed={"action","path","selector","text","preset","offset","condition","timeout_ms","quiet_ms","dx","dy","key",
            "color_scheme","reduced_motion","styles","points","expected_sequence"};action=args.get("action")
        if action not in {"navigate","read","click","type","screenshot","viewport","close","measure","wait_for","animations",
            "scroll","press","emulate","pixels","performance"}:raise Rejected("Unsupported browser action")
    elif name=="dev_browser_record":
        allowed={"action"};action=args.get("action")
        if action not in {"start","stop","status"}:raise Rejected("Unsupported recorder action")
    else:raise Rejected("Unknown Diagnostics browser tool")
    if set(args)-allowed:raise Rejected("Unknown browser argument")
    permitted={"login":{"path"},"navigate":{"path"},"read":{"selector","offset"},"click":{"selector"},
        "type":{"selector","text"},"screenshot":set(),"viewport":{"preset"},"close":set(),"start":set(),"stop":set(),"status":set(),
        "measure":{"selector","offset","styles"},"pixels":{"points","selector"},"performance":set(),"wait_for":{"condition","selector","text","timeout_ms","quiet_ms"},"animations":{"selector"},
        "scroll":{"selector","dx","dy"},"press":{"key","selector"},"emulate":{"color_scheme","reduced_motion"}}
    for key in list(args):
        if key not in ({"action"}|permitted[action]):
            if key=="expected_sequence" and type(args[key]) is int and args[key]==0:
                del args[key];continue
            if args[key] not in ("",None):raise Rejected("Arguments do not apply to this action")
            del args[key]
    if action in {"read","animations","scroll","press","wait_for","pixels"} and args.get("selector") in ("",None):args.pop("selector",None)
    if action=="login" and args.get("path") in ("",None):args.pop("path",None)
    if "selector" in args and (not isinstance(args["selector"],str) or not 1<=len(args["selector"])<=256 or "\0" in args["selector"]):
        raise Rejected("Invalid bounded CSS selector")
    if "text" in args and (not isinstance(args["text"],str) or len(args["text"].encode())>4096 or "\0" in args["text"]):
        raise Rejected("Literal text exceeds limit")
    if action in {"click","type"} and not args.get("selector"):raise Rejected("Action requires a selector")
    if action=="type" and "text" not in args:raise Rejected("Type requires literal text")
    if action=="viewport" and args.get("preset") not in VIEWPORTS:raise Rejected("Viewport preset must be one of "+", ".join(VIEWPORTS))
    if "offset" in args and (type(args["offset"]) is not int or not 0<=args["offset"]<=MAX_READ_OFFSET):raise Rejected("Read offset must be a whole number of characters")
    if action=="measure":
        if not args.get("selector"):raise Rejected("Measure requires a selector")
        if "styles" in args and type(args["styles"]) is not bool:raise Rejected("styles must be true or false")
    if action=="pixels":
        points=args.get("points")
        if points in (None,[]) and not args.get("selector"):raise Rejected("Pixels needs points or a selector")
        if points not in (None,[]) and (not isinstance(points,list) or len(points)>MAX_PIXELS or any(not isinstance(p,dict) or set(p)!={"x","y"}
            or type(p["x"]) is not int or type(p["y"]) is not int or not 0<=p["x"]<1600 or not 0<=p["y"]<1200 for p in points)):
            raise Rejected("points must be up to 20 objects with whole-pixel x and y inside the viewport")
    if action=="wait_for":
        if args.get("condition") not in WAIT_CONDITIONS:raise Rejected("Wait condition must be one of "+", ".join(sorted(WAIT_CONDITIONS)))
        if args["condition"] in {"visible","hidden"} and not args.get("selector"):raise Rejected("This wait condition requires a selector")
        if args["condition"]=="text" and not args.get("text"):raise Rejected("A text wait requires the text to wait for")
        for key,low,high in (("timeout_ms",100,10000),("quiet_ms",100,3000)):
            if key in args and (type(args[key]) is not int or not low<=args[key]<=high):raise Rejected(key+" must be a whole number from "+str(low)+" to "+str(high))
    if action=="scroll":
        for key in ("dx","dy"):
            if key in args and (type(args[key]) is not int or not -MAX_SCROLL<=args[key]<=MAX_SCROLL):raise Rejected("Scroll distances must be whole pixels within 20000")
        if not args.get("selector") and not args.get("dx") and not args.get("dy"):raise Rejected("Scroll needs a selector to bring into view or a distance")
    if action=="press" and args.get("key") not in KEYS:raise Rejected("Key must be one of "+", ".join(KEYS))
    if action=="emulate":
        if not ({"color_scheme","reduced_motion"}&set(args)):raise Rejected("Emulate needs color_scheme or reduced_motion")
        if "color_scheme" in args and args["color_scheme"] not in COLOR_SCHEMES:raise Rejected("color_scheme must be light, dark or default")
        if "reduced_motion" in args and args["reduced_motion"] not in REDUCED_MOTION:raise Rejected("reduced_motion must be reduce or default")
    return action
def png_metadata(data):
    """Verify PNG bytes and dimensions before storing or serving a private screenshot."""
    if not isinstance(data,bytes) or not 24<=len(data)<=MAX_PNG or data[:8]!=b"\x89PNG\r\n\x1a\n" or data[12:16]!=b"IHDR":
        raise Rejected("Screenshot is not a bounded PNG")
    width,height=struct.unpack(">II",data[16:24])
    if not 1<=width<=1600 or not 1<=height<=1200:raise Rejected("Screenshot dimensions exceed viewport bounds")
    return {"media_type":"image/png","bytes":len(data),"width":width,"height":height,
        "sha256":hashlib.sha256(data).hexdigest()}
def png_pixel(data):
    """The first pixel of a PNG. One pixel has no left or upper neighbour, so every row filter leaves it unchanged."""
    if data[:8]!=b"\x89PNG\r\n\x1a\n" or data[12:16]!=b"IHDR" or data[24]!=8 or data[25] not in (2,6):
        raise Rejected("Unsupported pixel capture")
    position,idat=8,b""
    while position<len(data):
        length=struct.unpack(">I",data[position:position+4])[0];kind=data[position+4:position+8]
        if kind==b"IDAT":idat+=data[position+8:position+8+length]
        position+=12+length
    row=zlib.decompress(idat)
    return tuple(row[1:4])
def process_stamp(pid):
    """Read process start time to avoid signalling a reused PID during stale-profile cleanup."""
    try:
        text=Path("/proc")/str(pid)/"stat"
        return text.read_text().rsplit(")",1)[1].split()[19]
    except (OSError,IndexError):return None
def cleanup_prior(directory):
    """Reap only a recorded matching browser process; never restore prior authenticated state."""
    record=directory/"driver.json"
    if not record.exists():return
    if record.is_symlink() or record.stat().st_size>1024:raise Rejected("Invalid browser process receipt")
    data=json.loads(record.read_text())
    pid=data.get("pid")
    if type(pid) is int and pid>1 and process_stamp(pid)==data.get("stamp"):
        try:
            if os.getpgid(pid)==pid:
                os.killpg(pid,signal.SIGTERM)
                deadline=time.monotonic()+1
                while process_stamp(pid)==data.get("stamp") and time.monotonic()<deadline:time.sleep(0.05)
                if process_stamp(pid)==data.get("stamp"):os.killpg(pid,signal.SIGKILL)
        except OSError:pass
    record.unlink()
def chrome_factory(profile,origin,*,offline=False):
    # Selenium's debug transport logging includes cookie command bodies.
    # This private worker suppresses those libraries instead of trusting log redaction.
    """Start fixed-origin Chromium with private storage and scrubbed partial-startup cleanup."""
    for name in ("selenium","selenium.webdriver.remote.remote_connection","urllib3","urllib3.connectionpool"):
        logger=logging.getLogger(name);logger.setLevel(logging.CRITICAL);logger.propagate=False
    from selenium import webdriver
    from selenium.webdriver.chrome.service import Service
    parsed=urlsplit(origin);port=parsed.port or (443 if parsed.scheme=="https" else 80)
    options=webdriver.ChromeOptions()
    options.binary_location="/usr/bin/chromium"
    for argument in ("--headless=new","--no-sandbox","--disable-dev-shm-usage",
        "--disable-background-networking","--disable-extensions","--disable-sync",
        "--no-first-run","--no-default-browser-check","--disk-cache-size=1048576",
        "--media-cache-size=1048576","--window-size=1280,900"):
        options.add_argument(argument)
    options.add_argument("--user-data-dir="+str(profile))
    # Unrelated origins cannot receive browser network traffic. Only the fixed
    # origin bypasses an intentionally unavailable local proxy.
    options.add_argument("--proxy-server=http://127.0.0.1:9")
    options.add_argument("--proxy-bypass-list="+("<-loopback>" if offline else "<-loopback>;"+parsed.hostname+":"+str(port)))
    if offline:
        options.add_argument("--host-resolver-rules=MAP * ~NOTFOUND")
        options.add_argument("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
        options.add_argument("--disable-quic")
    # Reads report uncaught page errors from this console log.
    options.set_capability("goog:loggingPrefs",{"browser":"ALL"})
    options.add_experimental_option("prefs",{"download_restrictions":3,
        "credentials_enable_service":False,"profile.password_manager_enabled":False})
    service=Service(executable_path="/usr/bin/chromedriver",log_path="/dev/null",
        popen_kw={"start_new_session":False},
        env={"PATH":"/usr/bin:/bin","HOME":str(profile),"TMPDIR":"/tmp",
             "XDG_CONFIG_HOME":str(profile/"config"),"XDG_CACHE_HOME":str(profile/"cache"),"LANG":"C.UTF-8"})
    driver=None
    try:
        driver=webdriver.Chrome(service=service,options=options)
        driver.set_page_load_timeout(PAGE_TIMEOUT)
        driver.set_script_timeout(5)
        driver.set_window_size(1280,900)
        return driver
    except Exception:
        if driver is not None:
            with contextlib.suppress(Exception):driver.quit()
        with contextlib.suppress(Exception):service.stop()
        raise StartupFailure("Private browser startup failed") from None

class BrowserState:
    def __init__(self,sid,user,generation,cookie,expires,driver,directory):
        """Initialize private state and synchronization without opening a browser."""
        self.sid=sid;self.user=user;self.generation=generation
        self.cookie=cookie;self.csrf="";self.expires=expires;self.driver=driver;self.directory=directory
        self.lock=threading.RLock();self.cancelled=threading.Event()
        self.touched=time.monotonic();self.observed=None;self.observed_url=""
        self.observed_selector="";self.observed_at=0;self.viewport=None
        self.media={};self.last_action="";self.last_action_at=None

# @code-index {"version":1,"feature":"diagnostics-private-browser","aliases":["login as current user","private development browser"],"purpose":"Confines Diagnostics browser actions and artifacts to worker-bound current-caller authority; resource defaults live in this package and worker provisioning","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Current-caller ownership and fixed-origin confinement are enforced security invariants."}}
class BrowserService:
    def __init__(self,private_root=PRIVATE_ROOT,*,test_origin=None,driver_factory=None):
        """Initialize private state and synchronization without opening a browser."""
        self.origin=ORIGIN
        if test_origin is not None:
            parsed=urlsplit(test_origin)
            if parsed.scheme!="http" or parsed.hostname not in {"127.0.0.1","localhost","::1"} or parsed.path or parsed.query or parsed.fragment:
                raise ValueError("Test browser origin must be an explicit loopback HTTP origin")
            self.origin=test_origin.rstrip("/")
        self.root=Path(private_root)
        self.factory=driver_factory or chrome_factory
        self.states={}
        self.guard=threading.RLock()
        self.recordings={}
        self.record_guards={}
    def directory(self,sid,user):
        """Resolve an owner-bound private session directory outside the coding workspace."""
        self.root.mkdir(mode=0o700,parents=True,exist_ok=True)
        if self.root.is_symlink():raise Rejected("Invalid private browser root")
        directory=self.root/sid
        children=list(self.root.iterdir())
        if not directory.exists() and len(children)>=MAX_PRIVATE_SESSIONS:
            raise Rejected("Private browser storage session quota reached")
        directory.mkdir(mode=0o700,exist_ok=True)
        if directory.is_symlink() or not directory.is_dir():raise Rejected("Invalid private browser session")
        os.chmod(directory,0o700)
        owner=directory/"owner.json"
        if owner.exists():
            if owner.is_symlink() or owner.stat().st_size>1024:raise Rejected("Invalid browser ownership record")
            if json.loads(owner.read_text()).get("user_id")!=user:raise Rejected("Private browser belongs to another caller")
        else:
            with owner.open("x") as output:json.dump({"user_id":user,"session_id":sid},output)
            os.chmod(owner,0o600)
        artifacts=directory/"artifacts";artifacts.mkdir(mode=0o700,exist_ok=True)
        if artifacts.is_symlink():raise Rejected("Invalid artifact directory")
        return directory
    def safe_url(self,state):
        """Check the observed URL after browser effects and reject cross-origin or cross-session state."""
        parsed=urlsplit(state.driver.current_url)
        origin=urlunsplit((parsed.scheme,parsed.netloc,"","",""))
        if origin!=self.origin:raise Uncertain("Browser left the fixed development origin; inspect the action before retrying")
        return validate_path(urlunsplit(("","",parsed.path,parsed.query,"")),{
            "session_id":state.sid,"agent_id":self._agent(state)})
    def _agent(self,state):
        """Return the worker-bound agent for URL confinement."""
        return state.agent
    def kill(self,state):
        """Invalidate observations and close the owned driver while preserving private artifacts."""
        state.cancelled.set();state.observed=None
        try:
            process=state.driver.service.process
            if process and process.poll() is None and os.getpgid(process.pid)==process.pid:
                os.killpg(process.pid,signal.SIGTERM)
        except (AttributeError,OSError):pass
        with contextlib.suppress(Exception):state.driver.quit()
        with contextlib.suppress(OSError):(state.directory/"driver.json").unlink()
        profile=state.directory/"chrome"
        if profile.exists() and not profile.is_symlink():
            with contextlib.suppress(OSError):shutil.rmtree(profile)
    def drop(self,sid):
        """Remove one profile under the service lock and close its driver."""
        with self.guard:
            state=self.states.pop(sid,None)
            if state:self.kill(state)
    def purge(self):
        """Close idle or expired profiles before admitting new browser work."""
        for sid,state in list(self.states.items()):
            if state.expires<=time.time() or time.monotonic()-state.touched>IDLE_SECONDS:self.drop(sid)
    def open(self,trusted):
        """Authenticate a new private profile as the current caller and reuse only matching authority."""
        sid=trusted["session_id"];self.purge()
        state=self.states.get(sid)
        if state and (state.user!=trusted["user_id"]):raise Rejected("Browser caller mismatch")
        if state and (state.cookie!=trusted["cookie"] or state.generation!=trusted["generation"] or state.agent!=trusted["agent_id"]):
            self.drop(sid);state=None
        if state:return state
        if len(self.states)>=MAX_SESSIONS:raise CapacityFailure("Private browser capacity reached")
        directory=self.directory(sid,trusted["user_id"])
        cleanup_prior(directory)
        profile=directory/"chrome"
        if profile.is_symlink():raise Rejected("Invalid browser profile")
        if profile.exists():shutil.rmtree(profile) # Restart never restores an authenticated profile.
        profile.mkdir(mode=0o700)
        try:driver=self.factory(profile,self.origin)
        except Exception:
            with contextlib.suppress(OSError):shutil.rmtree(profile)
            raise StartupFailure("Private browser startup failed") from None
        state=BrowserState(sid,trusted["user_id"],trusted["generation"],trusted["cookie"],
            trusted["expires_at"],driver,directory)
        state.agent=trusted["agent_id"]
        try:
            try:
                process=driver.service.process
                if process and process.poll() is None:
                    with (directory/"driver.json").open("x") as output:
                        json.dump({"pid":process.pid,"stamp":process_stamp(process.pid)},output)
                    os.chmod(directory/"driver.json",0o600)
            except AttributeError:pass
            driver.get(self.origin+"/health/ready")
            first=urlsplit(driver.current_url)
            if urlunsplit((first.scheme,first.netloc,"","",""))!=self.origin:raise Rejected("Initial browser origin mismatch")
            driver.add_cookie({"name":"v3_session","value":state.cookie,"path":"/","httpOnly":True,
                "secure":self.origin.startswith("https:"),"sameSite":"Strict","expiry":int(state.expires)})
            driver.get(self.origin+"/api/v3/auth/me")
            if driver.current_url!=self.origin+"/api/v3/auth/me":raise Rejected("Identity verification redirected")
            identity=json.loads(driver.find_element("tag name","body").text[:16384])
            if identity.get("user_id")!=state.user:raise Rejected("Authenticated browser identity mismatch")
            csrf=identity.get("csrf_token","")
            if not isinstance(csrf,str) or len(csrf)>128:raise Rejected("Invalid browser identity metadata")
            state.csrf=csrf
            driver.get(self.origin+validate_path("/app",trusted))
            self.safe_url(state)
            self.states[sid]=state
            return state
        except Exception:
            self.kill(state)
            raise AuthenticationFailure("Private browser identity verification failed") from None
    def artifact(self,state,data):
        """Store bounded PNG bytes with ownership, digest and quota metadata."""
        meta=png_metadata(data);folder=state.directory/"artifacts"
        existing=list(folder.iterdir())
        if len(existing)>=MAX_ARTIFACTS*2:raise Rejected("Private screenshot quota reached")
        total=0
        for image in self.root.glob("*/artifacts/*.png"):
            if image.is_symlink():raise Rejected("Invalid private artifact")
            total+=image.stat().st_size
        if total+len(data)>MAX_ARTIFACT_BYTES:raise Rejected("Private screenshot byte quota reached")
        artifact=str(uuid.uuid4());image=folder/(artifact+".png");manifest=folder/(artifact+".json")
        fd=os.open(image,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
        try:
            with os.fdopen(fd,"wb",closefd=False) as output:output.write(data);output.flush()
        finally:os.close(fd)
        record={**meta,"artifact_id":artifact,"session_id":state.sid,"user_id":state.user,"created_at":int(time.time())}
        with manifest.open("x") as output:json.dump(record,output)
        os.chmod(manifest,0o600)
        return {**meta,"artifact_id":artifact,
            "path":"/api/v3/sessions/"+state.sid+"/administrator/artifacts/"+artifact}
    def page_errors(self,state):
        """Uncaught errors and console errors since the last read, credentials redacted before truncation."""
        try:entries=state.driver.get_log("browser")
        except Exception:return None
        errors=[]
        for entry in entries:
            if not isinstance(entry,dict) or entry.get("level")!="SEVERE":continue
            if any(noise in str(entry.get("message","")) for noise in IGNORED_PAGE_ERRORS):continue
            text=str(entry.get("message","")).replace(state.cookie,"[redacted]")
            if state.csrf:text=text.replace(state.csrf,"[redacted]")
            errors.append(text[:300])
        return errors[-MAX_PAGE_ERRORS:]
    def apply_viewport(self,state):
        """Re-apply the chosen preset and media features; device emulation does not reliably survive navigation."""
        if state.viewport:
            width,height=VIEWPORTS[state.viewport];touch=state.viewport in TOUCH_VIEWPORTS
            state.driver.execute_cdp_cmd("Emulation.setDeviceMetricsOverride",{"width":width,"height":height,"deviceScaleFactor":1,"mobile":touch})
            state.driver.execute_cdp_cmd("Emulation.setTouchEmulationEnabled",{"enabled":touch})
        if state.media:
            state.driver.execute_cdp_cmd("Emulation.setEmulatedMedia",{"features":[
                {"name":"prefers-color-scheme","value":COLOR_SCHEMES[state.media.get("color_scheme","default")]},
                {"name":"prefers-reduced-motion","value":REDUCED_MOTION[state.media.get("reduced_motion","default")]}]})
    def redact(self,state,text):
        """Remove the browser's own credentials from page-derived text."""
        text=str(text).replace(state.cookie,"[redacted]")
        return text.replace(state.csrf,"[redacted]") if state.csrf else text
    def acted(self,state,action):
        """Start the clock wait_for reports as since_last_action_ms."""
        state.last_action=action;state.last_action_at=time.monotonic()
    def wait_for(self,state,args):
        """Poll one fixed condition every 50 ms until it holds or the bounded timeout ends."""
        started=time.monotonic();deadline=started+args.get("timeout_ms",5000)/1000;met=False
        while True:
            met=state.driver.execute_script(CHECK_SCRIPT,args["condition"],args.get("selector",""),args.get("text",""),args.get("quiet_ms",500)) is True
            if met or time.monotonic()>=deadline or state.cancelled.is_set():break
            time.sleep(0.05)
        now=time.monotonic()
        return {"condition":args["condition"],"met":met,"elapsed_ms":round((now-started)*1000),"last_action":state.last_action or None,
            "since_last_action_ms":round((now-state.last_action_at)*1000) if state.last_action_at else None}
    def pixels(self,state,args):
        """Rendered colours at viewport points, each from a 1x1 capture with the screenshot masks applied."""
        points=args.get("points") or []
        if not points:
            center=state.driver.execute_script(CENTER_SCRIPT,args["selector"])
            if not isinstance(center,dict):raise Rejected("No rendered element matches the selector")
            points=[{"x":int(center["x"]),"y":int(center["y"])}]
        self.apply_viewport(state)
        try:
            state.driver.execute_script(MASK_SCRIPT,state.cookie,state.csrf)
            samples=[]
            for point in points:
                shot=state.driver.execute_cdp_cmd("Page.captureScreenshot",{"format":"png","captureBeyondViewport":False,
                    "clip":{"x":point["x"],"y":point["y"],"width":1,"height":1,"scale":1}})
                rgb=png_pixel(base64.b64decode(shot["data"]))
                samples.append({"x":point["x"],"y":point["y"],"color":"#%02x%02x%02x"%rgb,"rgb":list(rgb)})
        finally:
            with contextlib.suppress(Exception):state.driver.execute_script(UNMASK_SCRIPT)
        return {"pixels":samples,"masked_color":"#555555"}
    def screenshot(self,state):
        """Mask credentials and editable content, capture a PNG, and invalidate the prior observation."""
        self.apply_viewport(state)
        try:
            state.driver.execute_script(MASK_SCRIPT,state.cookie,state.csrf)
            return self.artifact(state,state.driver.get_screenshot_as_png())
        finally:
            with contextlib.suppress(Exception):state.driver.execute_script(UNMASK_SCRIPT)
            state.observed=None
    def fresh_target(self,state,selector):
        """Require a recent read of this URL whose region holds the target, then re-check the target itself at dispatch."""
        if state.observed is None or time.monotonic()-state.observed_at>READ_FRESH_SECONDS or state.driver.current_url!=state.observed_url:
            raise Rejected("Read the current page (a region containing the target) before each input action")
        target=state.driver.execute_script(TARGET_SCRIPT,selector,state.observed_selector)
        if not target.get("allowed"):raise Rejected("Target is not in the region last read; read a selector that contains it")
        if target.get("secret"):raise Rejected("Target contains private credentials")
        if target.get("href"):
            parsed=urlsplit(target["href"])
            if urlunsplit((parsed.scheme,parsed.netloc,"","",""))!=self.origin:raise Rejected("External navigation is unavailable")
            validate_path(urlunsplit(("","",parsed.path,parsed.query,"")),{"session_id":state.sid,"agent_id":state.agent})
        element=state.driver.find_element("css selector",selector)
        if not element.is_displayed() or not element.is_enabled():raise Rejected("Input target is not visible and enabled")
        label=str(target.get("label","")).replace(state.cookie,"[redacted]")
        if state.csrf:label=label.replace(state.csrf,"[redacted]")
        return element,{"tag":str(target.get("tag","")),"label":label}
    def run(self,name,args,base,trusted):
        """Execute one validated action with serialized profile access and no automatic input replay."""
        trusted=validate_trusted(base,trusted);action=validate_args(name,args)
        # Reject a route before starting Chromium or causing navigation.
        if action in {"login","navigate"}:validate_path(args.get("path","/app"),trusted)
        with self.guard:
            self.purge()
            if action=="close":
                record=self.recordings.get(trusted["session_id"])
                if record:record["stop"].set()
                self.drop(trusted["session_id"]);return {"status":"success","closed":True}
            state=self.open(trusted)
        with state.lock:
            if state.cancelled.is_set() or state.expires<=time.time():raise Rejected("Browser authorization expired")
            # Navigation may leave a page whose URL is outside the routes; other actions may not act on one.
            state.touched=time.monotonic()
            if action not in {"login","navigate"}:self.safe_url(state)
            if action in {"login","navigate"}:
                route=validate_path(args.get("path","/app"),trusted)
                state.observed=None;state.driver.get(self.origin+route);self.acted(state,"navigate");self.apply_viewport(state);self.safe_url(state)
                result={"path":route,"identity_verified":True,"user_id":state.user}
            elif action=="read":
                read=state.driver.execute_script(READ_SCRIPT,args.get("selector","body"),state.cookie,MAX_TEXT,args.get("offset",0))
                if not isinstance(read,dict) or not isinstance(read.get("text"),str):raise Rejected("Invalid bounded DOM result")
                read["text"]=read["text"][:MAX_TEXT].replace(state.cookie,"[redacted]")
                if state.csrf:read["text"]=read["text"].replace(state.csrf,"[redacted]")
                # Any read that found its region permits input inside that region until navigation or input.
                if not read.get("missing"):
                    state.observed=True;state.observed_url=state.driver.current_url
                    state.observed_selector=args.get("selector","body");state.observed_at=time.monotonic()
                else:state.observed=None
                # Errors first, so a long page never pushes them out of view.
                result={"page_errors":self.page_errors(state),**read,"fresh_for_input":state.observed is not None}
            elif action in {"click","type"}:
                target,used=self.fresh_target(state,args["selector"]);state.observed=None
                try:
                    if action=="click":target.click()
                    else:target.send_keys(args["text"])
                    self.acted(state,action);self.safe_url(state)
                except Exception:raise Uncertain("Input outcome is uncertain; inspect the page before another action") from None
                result={"action":action,"applied":True,"target":used,"read_required":True}
            elif action=="screenshot":result=self.screenshot(state)
            elif action=="viewport":
                # Device emulation changes width, touch and mobile media queries; navigate again so boot-time layout reruns.
                state.viewport=args["preset"];self.apply_viewport(state)
                width,height=VIEWPORTS[state.viewport]
                state.observed=None
                result={"viewport":args["preset"],"width":width,"height":height,"navigate_required":True}
            elif action=="measure":
                # Styles are about 2 KB per element, so a styled page holds fewer elements.
                result=state.driver.execute_script(MEASURE_SCRIPT,args["selector"],5 if args.get("styles") else MEASURE_PAGE,args.get("offset",0),args.get("styles",False))
                if not isinstance(result,dict) or not isinstance(result.get("elements"),list):raise Rejected("Invalid bounded layout result")
                for row in result["elements"]+result["checks"]["under_44px"]["smallest"]+result["checks"]["content_overflowing"]["first"]:
                    row["label"]=self.redact(state,row.get("label",""))
            elif action=="pixels":result=self.pixels(state,args)
            elif action=="performance":
                result=state.driver.execute_script(PERFORMANCE_SCRIPT)
                if not isinstance(result,dict):raise Rejected("Invalid bounded performance result")
            elif action=="animations":
                result=state.driver.execute_script(ANIMATIONS_SCRIPT,args.get("selector",""),MAX_ELEMENTS)
                if not isinstance(result,dict) or not isinstance(result.get("animations"),list):raise Rejected("Invalid bounded animation result")
            elif action=="wait_for":result=self.wait_for(state,args)
            elif action=="scroll":
                result=state.driver.execute_script(SCROLL_SCRIPT,args.get("selector",""),args.get("dx",0),args.get("dy",0))
                if not isinstance(result,dict):raise Rejected("Invalid bounded scroll result")
                if result.get("missing"):raise Rejected("Scroll target does not exist")
                self.acted(state,"scroll")
            elif action=="press":
                # A key is input: it needs a fresh read like a click, and never reaches a credential field.
                if args.get("selector"):target,used=self.fresh_target(state,args["selector"])
                else:
                    if state.observed is None or time.monotonic()-state.observed_at>READ_FRESH_SECONDS or state.driver.current_url!=state.observed_url:
                        raise Rejected("Read the current page before pressing a key")
                    focus=state.driver.execute_script(FOCUS_SCRIPT)
                    if not isinstance(focus,dict) or focus.get("secret"):raise Rejected("The focused element contains private credentials")
                    target=state.driver.switch_to.active_element;used={"tag":str(focus.get("tag","")),"label":self.redact(state,focus.get("label",""))}
                state.observed=None
                try:
                    target.send_keys(KEYS[args["key"]]);self.acted(state,"press");self.safe_url(state)
                except Exception:raise Uncertain("Key outcome is uncertain; inspect the page before another action") from None
                focus=state.driver.execute_script(FOCUS_SCRIPT)
                focused={"tag":str(focus.get("tag","")),"id":focus.get("id"),"label":"" if focus.get("secret") else self.redact(state,focus.get("label",""))} if isinstance(focus,dict) else None
                result={"action":"press","key":args["key"],"applied":True,"target":used,"focused":focused,"read_required":True}
            elif action=="emulate":
                for key in ("color_scheme","reduced_motion"):
                    if key in args:state.media[key]=args[key]
                self.apply_viewport(state);state.observed=None
                result={"color_scheme":state.media.get("color_scheme","default"),"reduced_motion":state.media.get("reduced_motion","default")}
            else:raise Rejected("Recorder actions use the recorder interface")
            if state.cancelled.is_set():raise Uncertain("Browser action cancelled; inspect the outcome before retrying")
            return {"status":"success","application":"v3","backend":"private_chromium","session_id":state.sid,
                "source_kind":"untrusted_browser_observation",**result}
    async def call(self,name,args,base,trusted):
        """Bound synchronous browser work and publish only sanitized known or uncertain outcomes."""
        sid=trusted.get("session_id") if isinstance(trusted,dict) else None
        operation=asyncio.create_task(asyncio.to_thread(self.run,name,args,base,trusted))
        try:return await asyncio.wait_for(asyncio.shield(operation),CALL_TIMEOUT)
        except asyncio.CancelledError:
            if isinstance(sid,str):await asyncio.to_thread(self.drop,sid)
            raise
        except asyncio.TimeoutError:
            if isinstance(sid,str):await asyncio.to_thread(self.drop,sid)
            return {"status":"uncertain","error_kind":"browser_timeout","output":"Browser operation timed out; inspect before retrying"}
        except Uncertain:
            if isinstance(sid,str):await asyncio.to_thread(self.drop,sid)
            return {"status":"uncertain","error_kind":"browser_uncertain","output":"Browser changed origin or input outcome is uncertain; no automatic replay"}
        except (StartupFailure,AuthenticationFailure,CapacityFailure) as error:
            kind={StartupFailure:"browser_startup_failed",AuthenticationFailure:"browser_authentication_failed",CapacityFailure:"browser_capacity"}[type(error)]
            return {"status":"error","error_kind":kind,"output":"Private browser operation failed; credentials were not returned"}
        except Rejected as error:
            # Rejection messages are fixed strings in this file, never page or credential text.
            return {"status":"error","error_kind":"browser_invalid_request","output":"Private browser request rejected: "+str(error)}
        except Exception:
            return {"status":"error","error_kind":"browser_backend_failed","output":"Private browser backend failed; credentials were not returned"}
        finally:
            if not operation.done():
                operation.add_done_callback(lambda task:task.exception() if not task.cancelled() else None)
    async def record_loop(self,base,trusted,record):
        """Capture bounded private PNG frames until stop, quota, lease or backend failure."""
        try:
            for _ in range(30):
                if record["stop"].is_set() or time.time()>=trusted["expires_at"]:break
                await asyncio.sleep(1)
                if record["stop"].is_set():break
                result=await self.call("dev_browser_action",{"action":"screenshot"},base,trusted)
                if result.get("status")!="success":
                    record["status"]="stopped";break
                record["artifacts"].append({k:result[k] for k in ("artifact_id","path","media_type","bytes","width","height","sha256")})
            record["status"]="stopped"
        except asyncio.CancelledError:record["status"]="stopped"
        finally:record["completed_at"]=int(time.time())
    async def record(self,args,base,trusted):
        """Serialize recorder state changes independently of per-frame browser access."""
        sid=trusted.get("session_id") if isinstance(trusted,dict) else None
        if not canonical_id(sid):
            return {"status":"error","error_kind":"browser_record_rejected","output":"Recorder request rejected"}
        lock=self.record_guards.setdefault(sid,asyncio.Lock())
        async with lock:return await self._record(args,base,trusted)
    async def _record(self,args,base,trusted):
        """Start or stop a caller-bound recording and return bounded artifact metadata."""
        try:
            trusted=validate_trusted(base,trusted);action=validate_args("dev_browser_record",args);sid=trusted["session_id"]
            record=self.recordings.get(sid)
            if record and record["user"]!=trusted["user_id"]:raise Rejected("Recorder caller mismatch")
            if record and (record["cookie"]!=trusted["cookie"] or record["generation"]!=trusted["generation"]):
                record["stop"].set();record["task"].cancel()
                with contextlib.suppress(asyncio.CancelledError):await record["task"]
                await asyncio.to_thread(self.drop,sid)
                record["status"]="stopped"
            if action=="start":
                if record and record["status"]=="recording":raise Rejected("Recorder already running")
                opened=await self.call("login_as_current_user",{},base,trusted)
                if opened.get("status")!="success":return opened
                record={"user":trusted["user_id"],"generation":trusted["generation"],"cookie":trusted["cookie"],
                    "status":"recording","artifacts":[],"started_at":int(time.time()),
                    "stop":asyncio.Event()}
                self.recordings[sid]=record
                record["task"]=asyncio.create_task(self.record_loop(base,trusted,record))
            elif action=="stop" and record:
                record["stop"].set()
                await record["task"]
            if record is None:return {"status":"success","recording":False,"frames":0,"artifacts":[]}
            return {"status":"success","recording":record["status"]=="recording","frames":len(record["artifacts"]),
                "artifacts":list(record["artifacts"]),"started_at":record["started_at"],
                "completed_at":record.get("completed_at"),"format":"bounded_png_frames",
                "max_frames":30,"max_seconds":30,"video":False}
        except Exception:return {"status":"error","error_kind":"browser_record_rejected","output":"Recorder request rejected"}
    def read_artifact(self,base,trusted,artifact):
        """Authenticate private artifact ownership and verify stored bytes before returning a PNG."""
        trusted=validate_trusted(base,trusted)
        if not canonical_id(artifact):raise Rejected("Invalid artifact ID")
        folder=self.directory(trusted["session_id"],trusted["user_id"])/"artifacts"
        meta=folder/(artifact+".json");image=folder/(artifact+".png")
        if meta.is_symlink() or meta.stat().st_size>2048:raise Rejected("Invalid artifact record")
        record=json.loads(meta.read_text())
        if record.get("user_id")!=trusted["user_id"] or record.get("session_id")!=trusted["session_id"]:
            raise Rejected("Artifact ownership mismatch")
        fd=os.open(image,os.O_RDONLY|os.O_NOFOLLOW)
        try:
            info=os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink!=1 or info.st_size>MAX_PNG:raise Rejected("Invalid private artifact")
            data=os.read(fd,MAX_PNG+1)
        finally:os.close(fd)
        actual=png_metadata(data)
        if any(record.get(k)!=actual[k] for k in actual):raise Rejected("Artifact digest changed")
        return data
    async def close_all(self,base):
        """Stop recording and close one session profile during trusted worker quiescence."""
        sid=Path(base).name
        if not canonical_id(sid):return
        record=self.recordings.get(sid)
        if record:
            record["stop"].set();record["task"].cancel()
            with contextlib.suppress(asyncio.CancelledError):await record["task"]
        await asyncio.to_thread(self.drop,sid)

_SERVICE=BrowserService()
async def execute(name,args,base,trusted):
    """Dispatch browser or recording work to the package-owned persistent service."""
    if name=="dev_browser_record":return await _SERVICE.record(args,base,trusted)
    return await _SERVICE.call(name,args,base,trusted)
def read_artifact(base,trusted,artifact):
    """Authenticate private artifact ownership and verify stored bytes before returning a PNG."""
    return _SERVICE.read_artifact(base,trusted,artifact)
async def close_all(base):
    """Stop recording and close one session profile during trusted worker quiescence."""
    await _SERVICE.close_all(base)
