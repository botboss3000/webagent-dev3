// Injected into each agent tab through the debugger (never into the user's other tabs).
// Shows the agent's pointer, click ripples and typing, and hands control to the user as
// soon as they click or type themselves. Its host is aria-hidden so page reads skip it,
// and it ignores pointer events except on its own buttons.
(() => {
  if (window.__webagentOverlay) return;
  const host = document.createElement('webagent-overlay');
  host.setAttribute('aria-hidden', 'true');
  host.style.cssText = 'all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none;';
  const root = host.attachShadow({mode: 'closed'});
  root.innerHTML = `<style>
    .cursor{position:fixed;left:0;top:0;width:22px;height:22px;transition:transform .35s cubic-bezier(.3,.7,.4,1);transform:translate(-40px,-40px);filter:drop-shadow(0 1px 2px rgba(0,0,0,.5))}
    .ripple{position:fixed;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;border:3px solid #7c5cff;animation:r .5s ease-out forwards}
    @keyframes r{from{transform:scale(.3);opacity:1}to{transform:scale(1.6);opacity:0}}
    .outline{position:fixed;border:2px solid #7c5cff;border-radius:4px;transition:all .2s;pointer-events:none;display:none}
    .bar{position:fixed;left:50%;bottom:16px;transform:translateX(-50%);max-width:min(92vw,560px);display:flex;gap:10px;align-items:center;
      padding:8px 12px;border-radius:12px;background:#1f1d2b;color:#fff;font:13px/1.35 system-ui,sans-serif;box-shadow:0 4px 16px rgba(0,0,0,.35);pointer-events:none}
    .bar.user{pointer-events:auto}
    .dot{width:8px;height:8px;border-radius:50%;background:#7c5cff;flex:none}
    .bar.user .dot{background:#3fb950}
    .text{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
    button{font:inherit;border:1px solid #7c5cff;background:transparent;color:#fff;border-radius:8px;padding:4px 10px;cursor:pointer;flex:none}
    button:hover{background:#7c5cff}
    [hidden]{display:none!important}
  </style>
  <svg class="cursor" viewBox="0 0 24 24"><path d="M3 2l7 19 2.5-7.5L20 11z" fill="#7c5cff" stroke="#fff" stroke-width="1.5"/></svg>
  <div class="outline"></div>
  <div class="bar"><span class="dot"></span><span class="text">WebAgent is ready</span><button type="button" hidden>Hand back to agent</button></div>`;
  const cursor = root.querySelector('.cursor'), outline = root.querySelector('.outline');
  const bar = root.querySelector('.bar'), text = root.querySelector('.text'), handBack = root.querySelector('button');
  // The agent's own input arrives as trusted events too; it announces each one first.
  let agentUntil = 0, userControl = false;
  const notify = message => { try { window.__webagentUser?.(message); } catch {} };
  const setUser = on => {
    userControl = on;
    bar.classList.toggle('user', on);
    handBack.hidden = !on;
    text.textContent = on ? 'You have control. The agent waits.' : 'WebAgent is working here';
  };
  const takeOver = event => {
    if (!event.isTrusted || userControl || Date.now() < agentUntil) return;
    if (event.composedPath().includes(host)) return;
    setUser(true);
    notify('took-over');
  };
  for (const type of ['pointerdown', 'keydown', 'wheel']) window.addEventListener(type, takeOver, true);
  handBack.addEventListener('click', () => { setUser(false); notify('resume'); });
  // Injected before the page's own scripts on each navigation, possibly before <html> exists.
  const attach = () => { if (!host.isConnected) document.documentElement.append(host); };
  const start = () => { attach(); new MutationObserver(attach).observe(document.documentElement, {childList: true}); };
  if (document.documentElement) start(); else document.addEventListener('DOMContentLoaded', start, {once: true});
  window.__webagentOverlay = {
    expect(ms = 800) { agentUntil = Date.now() + ms; },
    move(x, y) { cursor.style.transform = `translate(${x - 3}px,${y - 2}px)`; },
    ripple(x, y) {
      const ring = document.createElement('div'); ring.className = 'ripple';
      ring.style.left = x + 'px'; ring.style.top = y + 'px'; root.append(ring);
      setTimeout(() => ring.remove(), 600);
    },
    box(rect) {
      if (!rect) { outline.style.display = 'none'; return; }
      Object.assign(outline.style, {display: 'block', left: rect.x - 3 + 'px', top: rect.y - 3 + 'px',
        width: rect.width + 6 + 'px', height: rect.height + 6 + 'px'});
    },
    caption(message) { if (!userControl) text.textContent = message; },
    user(on) { setUser(on); },
    get userControl() { return userControl; },
  };
})();
