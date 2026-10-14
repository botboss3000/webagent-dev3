// @code-index {"version":1,"feature":"chat-ui-config","aliases":["chat configuration","chat surface settings"],"purpose":"Applies the V3 chat surface profile from data/config/chat.json (runtime /state/config/chat.json, or V3_CHAT_UI_CONFIG_PATH), including chat_common, desktop/mobile/widget overlays, and chat_display.default. Config: chat_common.composer.keyboard_fade controls keyboard opacity transitions through viewport.js; profile overlays apply.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_display/default","/chat_common","/chat_desktop","/chat_mobile","/chat_widget","/chat_common/composer/keyboard_fade"],"profile_overrides":true}],"opportunities":[]}}
import { applyBubblePaint } from '../core/bubble-paint.js';
import { applyChatPillLayout, disposeChatPillLayout } from './chat-pill-config.js';
import { initFooterHandle } from './footer-handle.js';
// V3 reads the v3/data/config/chat.json schema. Unknown controls are
// skipped until their v3 features exist; the controls below are live in v3.
let chatRoot = null;
// The optional chatRoot keeps widget queries scoped; document lookup supports the panel.
const byId = id => chatRoot?.querySelector('#'+id) || document.getElementById(id);
let surfaceActions = {};
let sharedControls = {};
/** Registers session-owned modular controls, used by both header and footer profiles. */
export function setSharedChatControls(nodes){sharedControls=nodes;currentSurface="";if(documentConfig)applyChatConfig();}
/** Supplies widget actions consumed by the generated chat header controls. */
export function setChatSurfaceActions(actions){surfaceActions=actions;}
/** Returns only supported panel/widget defaults from the loaded profile. */
export function defaultChatDisplay(){const value=documentConfig?.chat_display?.default;return value==="panel"||value==="widget"?value:null;}
/** Merges common and widget-specific profile values for the native widget. */
export function widgetProfile(){return mergeProfile(documentConfig?.chat_common,documentConfig?.chat_widget);}
/** Invalidates the cached surface choice and reapplies the active profile. */
export function refreshChatSurface(){currentSurface='';applyChatConfig();}
let documentConfig = null;
let headerNodes = null;
let currentSurface = '';
let currentFooterMode = '';
let listenersBound = false;
let panelObserver = null;
let observedPanel = null;
let geometryFrame = null;

/** Coalesces geometry notifications; height-only changes keep the resolved layout cache. */
function scheduleChatGeometry() {
  if (geometryFrame !== null) return;
  geometryFrame = requestAnimationFrame(() => { geometryFrame = null; applyChatConfig(); });
}

/** Updates transient controls independently of configured row structure. */
function applyComposerState() {
  if (!footerNodes) return;
  const {pill, input, stop, continue: continuation, send, mic} = footerNodes;
  const text = !!input.value.trim() || !!Number(pill.dataset.attachmentCount);
  pill.classList.toggle('has-text', text);
  send.style.display = text ? '' : 'none'; mic.style.display = text ? 'none' : '';
  stop.hidden = !stop.dataset.run || stop.dataset.configEnabled === 'false';
  stop.style.display = stop.hidden ? 'none' : '';
  continuation.hidden = !!stop.dataset.run || text || continuation.dataset.configEnabled === 'false';
  continuation.style.display = continuation.hidden ? 'none' : '';
}

/** Recursively overlays object values while replacing arrays and scalar values. */
export function mergeProfile(base, override) {
  const merged = { ...(base || {}) };
  for (const [key, value] of Object.entries(override || {})) {
    merged[key] = value && typeof value === 'object' && !Array.isArray(value)
      ? mergeProfile(base?.[key], value) : value;
  }
  return merged;
}

/** Resolves portal or panel-width context to the configured profile key. */
function surfaceName() {
  if (window.__CHAT_PORTAL__) return 'chat_widget';
  return (byId('chat-panel')?.clientWidth || window.innerWidth) <= 768 ? 'chat_mobile' : 'chat_desktop';
}

/** Returns the common profile merged with the current panel/mobile surface profile. */
export function currentChatProfile() {
  if (!documentConfig?.chat_common) return null;
  return mergeProfile(documentConfig.chat_common, documentConfig[surfaceName()]);
}

// SISTER-PANEL: v2 chat-controls-config sizing; row defaults precede overrides.
/** Applies configured dimensions to a header/footer control and its icon. */
function sizeControl(node, config) {
  if (!node || !config) return;
  const size = config.container_size || config.box_size;
  const icon = node.querySelector('svg') || node.querySelector('i');
  const iconSize = config.element_size || config.size || (size && size !== 'auto' ? Math.floor(parseFloat(size) * .7) + 'px' : null);
  if (icon && iconSize) for (const key of ['width','height']) icon.style.setProperty(key,iconSize,'important');
  if (size && size !== 'auto') {
    for (const key of ['height','min-height']) node.style.setProperty(key,size,'important');
    if (config.square !== false) { node.style.setProperty('width',size,'important'); node.style.padding='0'; node.style.justifyContent='center'; }
  }
  const trigger = node.querySelector('.session-dropdown-trigger');
  if (trigger) { trigger.style.height='100%'; trigger.style.minHeight='0'; trigger.style.boxSizing='border-box'; }
  if (config.font_size) { node.style.fontSize=config.font_size; if(trigger) trigger.style.fontSize=config.font_size; }
  if (config.border_radius) {
    const sessionPill = node.dataset.headerControl === 'session_dropdown';
    // A pill stays semicircular even when its rendered height exceeds the profile size.
    const radius = config.border_radius === 'pill' ? (sessionPill ? '999px' : `calc(${size || '40px'} / 2)`) : config.border_radius;
    node.style.setProperty('border-radius', sessionPill ? `var(--session-selector-open-radius, ${radius})` : radius, sessionPill ? 'important' : '');
  }
  if (config.min_width) node.style.minWidth=config.min_width;
  if (config.max_width) node.style.maxWidth=config.max_width;
}

/** Resolves cached shell controls or creates the supported navigation/action nodes. */
function headerControl(name) {
  if (!headerNodes) {
    const select = byId('sessions');
    headerNodes = {
      agent_name: byId('chat-header-agent-name').closest('.chat-header-name-row'),
      session_dropdown: byId('session-dropdown'),
      new_session: byId('new'),
    };
    for (const [name, delta, label, symbol] of [
      ['session_prev', -1, 'Previous conversation', 'arrow-big-left'],
      ['session_next', 1, 'Next conversation', 'arrow-big-right'],
    ]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'session-nav-btn';
      button.setAttribute('aria-label', label);
      button.title = label;
      button.innerHTML = '<i data-lucide="' + symbol + '"></i>';
      button.onclick = () => {
        const next = select.selectedIndex + delta;
        if (next < 0 || next >= select.options.length) return;
        select.selectedIndex = next;
        select.dispatchEvent(new Event('change', { bubbles: true }));
      };
      headerNodes[name] = button;
    }
    const minimize = document.createElement('button');
    minimize.type = 'button';
    minimize.className = 'chat-header-minimize';
    minimize.setAttribute('aria-label', 'Minimize chat');
    minimize.title = 'Minimize chat';
    minimize.innerHTML = '<i data-lucide="minus"></i>';
    minimize.onclick = () => surfaceActions.minimize ? surfaceActions.minimize() : byId('chat-toggle-btn')?.click();
    headerNodes.minimize = minimize;
    const close=document.createElement('button');close.type='button';close.className='chat-header-minimize';close.title='Close chat';close.setAttribute('aria-label','Close chat');close.textContent='×';close.onclick=()=>surfaceActions.close?.();headerNodes.close=close;
    const title=document.createElement('span');title.textContent=surfaceActions.title||'WebAgent';headerNodes.title=title;
    const icon=document.createElement('span');icon.textContent='◉';icon.setAttribute('aria-hidden','true');headerNodes.icon=icon;
    const status=document.createElement('span');status.textContent='';headerNodes.status=status;
    const dot=document.createElement('span');dot.className='agent-status-dot';headerNodes.dot=dot;
  }
  return sharedControls[name] || headerNodes[name];
}

/** Rebuilds configured header rows from known controls; disabled/missing controls are skipped. */
function applyHeader(profile) {
  const header = byId('chat-header');
  const config = profile.chat_header || {};
  header.hidden = config.enabled === false;
  if (header.hidden) return;
  header.style.setProperty('--chat-header-max-width', config.max_width || profile.content_max_width || '100%');
  const rows = Array.isArray(config.rows) ? config.rows : [];
  headerControl('agent_name');
  headerNodes.session_dropdown.configure?.(config.controls?.session_dropdown);
  headerNodes.session_dropdown._sessionDropdownController?.close({immediate:true});
  header.replaceChildren();
  for (const row of rows) {
    const line = document.createElement('div');
    line.className = 'chat-header-row v3-config-header-row';
    for (const zoneName of ['left', 'center', 'right']) {
      const zone = document.createElement('span');
      zone.className = 'chat-header-zone chat-header-zone-' + zoneName;
      for (const name of row[zoneName] || []) {
        const node = headerControl(name);
        const controlConfig = { ...config.row_sizes?.primary, ...config.controls?.[name] };
        if (name === 'agent_name') controlConfig.square = false;
        if (!node || controlConfig.enabled === false) continue;
        node.hidden = false;
        node.dataset.headerControl = name;
        sizeControl(node, controlConfig);
        zone.append(node);
      }
      line.append(zone);
    }
    if (line.querySelector('.chat-header-zone:not(:empty)')) header.append(line);
  }
}

let footerNodes = null;
let applyingFooter = false;
let footerControls = null;
/** Applies footer mode/layout and binds its one-time shell controls; guard prevents recursive refresh. */
function applyFooter(profile, mode) {
  if (applyingFooter) return;
  applyingFooter = true;
  try {
    const footer = profile[mode + '_footer'] || {};
    const pill = footer.chat_pill || {};
    const surface = byId('chat-panel');
    const area = byId('chat-input-area');
    if (!footerNodes) {
      footerNodes = { pill: byId('chat-input-row'), input: byId('chat-input'),
        stats: byId('chat-pill-stats-row'), pillButtons: document.querySelector('#chat-input-row .chat-pill-buttons'),
        mic: byId('chat-voice-btn'), send: byId('chat-send'), attach: byId('chat-attach-btn'),
        stop: byId('chat-stop-btn'), continue: byId('chat-continue-btn') };
      for (const [node, feature] of [[footerNodes.attach,'Attachments'],[footerNodes.mic,'Voice']]) {
        node.type = 'button';
        node.disabled = byId('chat-input-row').dataset.composerAvailable !== 'true';
        node.title = feature === 'Attachments' ? 'Attach files (or paste / drop onto the chat pill)' : 'Voice conversation';
      }
      footerNodes.send.type = 'submit';
      footerNodes.stop.type = 'button';
      footerNodes.continue.type = 'button';
      footerNodes.continue.disabled = false;
      // Provider usage and measured context are supplied by the v3 stats adapter.
      byId('chat-token-bar').hidden = false;
      footerControls = Object.fromEntries(Array.from(byId('chat-footer-row').querySelectorAll('[data-header-control]')).map(n => [n.dataset.headerControl,n]));
      for (const name of ['changes','browser_notifications']) {
        const node = document.createElement('button'); node.type = 'button';
        node.className = name === 'changes' ? 'chat-changes-btn' : 'chat-browser-notifications-btn';
        node.dataset.headerControl = name;
        node.innerHTML = '<i data-lucide="'+(name === 'changes' ? 'files' : 'bell')+'" style="width:16px;height:16px;"></i>';
        footerControls[name] = node;
      }
      for (const [name,node] of Object.entries(footerControls)) {
        node.type = 'button';
        if(['mode','model_changer'].includes(name))continue;
        node.disabled = true;node.classList.add('v3-backend-pending');
        node.title = name.replaceAll('_',' ') + ' — backend port pending';
      }
      footerNodes.stats.style.display = '';
      initFooterHandle();
      const handle = byId('chat-footer-handle');
      handle.tabIndex = 0; handle.setAttribute('role','button');
      handle.addEventListener('keydown', event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); handle.click(); } });
    }
    area.classList.toggle('footer-mode-idle', mode === 'idle');
    area.classList.toggle('footer-mode-active', mode === 'active');
    for (const [token,value] of [['--chat-pill-width',pill.pill_width],['--chat-pill-current-width',pill.pill_width],['--chat-pill-radius',pill.pill_radius]]) if(value)surface.style.setProperty(token,value);
    area.style.setProperty('--idle-pill-width',profile.idle_footer?.chat_pill?.pill_width || '500px');
    area.style.setProperty('--idle-margin-bottom',profile.idle_footer?.margin_bottom || '24px');
    area.style.setProperty('bottom',footer.margin_bottom || '0px','important');
    const contentProfile = currentChatProfile();
    surface.style.setProperty('--chat-surface-max-width',contentProfile.content_max_width || '1000px');
    if(profile.messages?.pill_placeholder)footerNodes.input.placeholder=profile.messages.pill_placeholder;
    applyChatPillLayout(footerNodes,pill);
    footerNodes.stop.dataset.configEnabled=String(pill.controls?.stop?.enabled !== false);
    footerNodes.continue.dataset.configEnabled=String(pill.controls?.continue?.enabled !== false);
    applyComposerState();
    const row=byId('chat-footer-row');
    const below=footer.below_pill || {};
    row.replaceChildren();
    for(const spec of below.rows || []) {
      const line=document.createElement('div'); line.className='chat-header-row';
      for(const zoneName of ['left','center','right']) {
        const zone=document.createElement('span'); zone.className='chat-header-zone chat-header-zone-'+zoneName;
        for(const name of spec[zoneName] || []) {
          const node=sharedControls[name] || footerControls[name]; const cfg=below.controls?.[name] || {};
          if(!node || cfg.enabled === false)continue;
          node.hidden=false;sizeControl(node,cfg);zone.append(node);
        }
        line.append(zone);
      }
      row.append(line);
    }
    row.hidden=below.enabled === false;
    row.classList.toggle('idle-collapsed',mode === 'idle'&&!footer.below_pill);
    row.classList.toggle('v3-footer-configured',!!footer.below_pill);
    byId('chat-footer-handle').style.display=mode === 'idle' || row.hidden ? 'none' : '';
    window.lucide?.createIcons();
  } finally { applyingFooter=false; }
}

/** Selects the current profile and updates bubble, transcript, header, and footer presentation. */
export function applyChatConfig() {
  const profile = currentChatProfile();
  if (!profile) return; window.__chatKeyboardFadeConfigure?.(profile.composer?.keyboard_fade);
  const surface = surfaceName();
  const focused = document.activeElement;
  const inComposer = byId('chat-input-area')?.contains(focused) || focused?.closest('.v3-context-panel:not([hidden]), .v3-chat-model-list:not([hidden])');
  const mode = byId('chat-input').value || focused === byId('chat-input') || (currentFooterMode === 'active' && inComposer) ? 'active' : 'idle';
  if (applyingFooter) return;
  if (surface === currentSurface && mode === currentFooterMode) {
    applyComposerState();
    return;
  }
  const surfaceChanged = surface !== currentSurface;
  currentSurface = surface;
  currentFooterMode = mode;
  if (surfaceChanged) {
    for (const bubble of document.querySelectorAll('#chat-messages-inner .chat-bubble.v3-config-bubble')) {
      const role = bubble.classList.contains('user') ? 'user' : 'agent';
      applyBubblePaint(bubble, profile.message_bubbles?.[role] || {});
    }
  }
  const panel = byId('chat-panel');
  const contentProfile = currentChatProfile();
  panel.style.setProperty('--v3-content-max-width', contentProfile.content_max_width || '100%');
  panel.style.setProperty('--v3-transcript-top',(profile.transcript_padding?.top_px ?? 25)+'px');
  panel.style.setProperty('--v3-transcript-bottom',(profile.transcript_padding?.bottom_px ?? 50)+'px');
  const tailClearance = profile.transcript_tail_clearance || {};
  // Rejects negative/non-numeric spacing before writing the transcript CSS tokens.
  const tailPixels = (value, fallback) => Number.isFinite(Number(value)) && Number(value) >= 0 ? Number(value) : fallback;
  panel.style.setProperty('--v3-tail-gap', tailPixels(tailClearance.gap_px, 24) + 'px');
  panel.style.setProperty('--v3-tail-min', tailPixels(tailClearance.min_px, 100) + 'px');
  // Match v2's stationary message-scroller mask; zero top fade avoids a seam.
  const messages = byId('chat-messages');
  const top = Math.max(0, Number(profile.fade?.top) || 0);
  const bottom = Math.max(0, Number(profile.fade?.bottom) || 0);
  messages?.style.setProperty('--chat-fade-top', top + 'px');
  messages?.style.setProperty('--chat-fade-top-stops', top > 0 ? `transparent 0, #000 ${top}px` : '#000 0');
  messages?.style.setProperty('--chat-fade-bottom', bottom + 'px');
  if (surfaceChanged) applyHeader(profile);
  applyFooter(profile, mode);
}

/** Loads or accepts UI config, binds delegated listeners once, and reapplies on remount. */
export async function initChatConfig({config:provided,root=null} = {}) {
  chatRoot=root;
  let data=provided;
  if(!data){
  const response = await fetch('/api/v3/auth/ui-config', { credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) return;
  data = await response.json();
  }
  const config = data.chat_ui || data;
  if (!config?.chat_common) return;
  // A remount replaces the partial; never reuse detached header/pill nodes.
  if (footerNodes && footerNodes.input !== byId('chat-input')) { disposeChatPillLayout(footerNodes.pill); headerNodes=null; footerNodes=null; footerControls=null; currentSurface=''; currentFooterMode=''; }
  documentConfig = config;
  currentSurface = '';
  currentFooterMode = '';
  applyChatConfig();
  const panel = byId('chat-panel');
  if (observedPanel !== panel) {
    panelObserver?.disconnect();
    if (geometryFrame !== null) { cancelAnimationFrame(geometryFrame); geometryFrame = null; }
    observedPanel = panel;
    panelObserver = new ResizeObserver(scheduleChatGeometry);
    panelObserver.observe(panel);
  }
  if (!listenersBound) {
    listenersBound = true;
    window.addEventListener('resize', scheduleChatGeometry);
    // Defer focusout until the destination has focus; a synchronous textarea
    // blur must not detach a stats/mode/model control before its click fires.
    // Delegation also survives chat-partial remounts.
    // Pointer controls belong to the composer interaction: keep its caret
    // and selection instead of letting a button blur an empty composer.
    // Keyboard focus and editable popup fields retain their normal behavior.
    document.addEventListener('pointerdown', event => {
      if (event.button !== 0 || document.activeElement !== byId('chat-input')) return;
      const control = event.target.closest?.('button, [role="button"], #chat-footer-handle, [data-header-control]');
      if (!control || !control.closest('#chat-input-area, #chat-header, .v3-context-panel, .v3-chat-model-list, .session-dropdown-menu')) return;
      event.preventDefault();
    }, true);
    document.addEventListener('focusin', applyChatConfig);
    document.addEventListener('focusout', () => setTimeout(applyChatConfig, 0));
    document.addEventListener('input', event => { if(event.target === byId('chat-input'))applyChatConfig(); });
  }
}
