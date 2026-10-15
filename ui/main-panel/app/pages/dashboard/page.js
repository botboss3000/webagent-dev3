// Frontend-only V2 dashboard port. The static catalog owns tile identity and size;
// no metrics, layout persistence or server actions are connected in this phase.
 // @code-index {"version":1,"feature":"app-dashboard","purpose":"Config: ui/main-panel/app/pages/dashboard/cards.json -> cards; frontend tile inventory and pending backend state","span_lines":64,"configuration":{"status":"configured","references":[{"path":"ui/main-panel/app/pages/dashboard/cards.json","pointers":["/cards"]}],"opportunities":[]},"aliases":["dashboard tiles"]}
/** Mount all legacy tiles without contacting legacy or V3 metric endpoints. */
export async function mountDashboard(host) {
  // Page descriptors accept sibling filenames only; this module owns its nested stylesheet.
  const stylesheet=new URL('./page.css',import.meta.url).href;
  if(!document.querySelector('link[data-dashboard-style]')){
    const link=document.createElement('link');link.rel='stylesheet';link.href=stylesheet;
    link.dataset.dashboardStyle='true';document.head.append(link);
  }
  const response=await fetch(new URL('./cards.json',import.meta.url),{cache:'no-store'});
  if(!response.ok)throw Error('Dashboard tile catalog unavailable');
  const catalog=await response.json();
  if(catalog.version!==1||!Array.isArray(catalog.cards))throw Error('Invalid dashboard tile catalog');
  const ids=new Set();
  for(const card of catalog.cards){
    if(!/^[a-z][a-z0-9_]*$/.test(card.id)||ids.has(card.id)||!card.label||!Number.isInteger(card.w)||card.w<1||card.w>12||!Array.isArray(card.fields)||!Array.isArray(card.sections))throw Error('Invalid dashboard tile');
    ids.add(card.id);
  }
  const node=(tag,text,className)=>{const el=document.createElement(tag);if(text!==undefined)el.textContent=text;if(className)el.className=className;return el;};
  host.replaceChildren();host.classList.add('v3-dashboard');
  const heading=node('h2','Dashboard');
  const notice=node('p','Frontend port from V2. Red tiles need V3 backend work. No live metrics are shown.');
  const toolbar=node('div',undefined,'dashboard-toolbar');
  const count=node('span');count.setAttribute('role','status');
  const add=node('button','Add card');add.type='button';add.setAttribute('aria-expanded','false');
  const reset=node('button','Restore all tiles');reset.type='button';
  const edit=node('button','Edit layout');edit.type='button';edit.setAttribute('aria-pressed','false');
  const windowSelect=node('select');windowSelect.disabled=true;windowSelect.setAttribute('aria-label','Time window — backend work needed');
  for(const label of ['Last 5 min','Last hour','Last 24 hours','Last 7 days'])windowSelect.append(node('option',label));
  windowSelect.selectedIndex=1;
  toolbar.append(count,windowSelect,edit,add,reset);
  const picker=node('div',undefined,'dashboard-picker');picker.hidden=true;picker.setAttribute('aria-label','Add dashboard card');
  const grid=node('div',undefined,'dashboard-grid');
  host.append(heading,notice,toolbar,picker,grid);
  let selected=catalog.cards.map(card=>card.id),editing=false;
  /** Repaint local layout only; persistence remains explicitly pending. */
  function paint(){
    grid.replaceChildren();picker.replaceChildren();count.textContent=selected.length+' tiles · layout changes last until reload';
    for(const [index,id] of selected.entries()){
      const card=catalog.cards.find(item=>item.id===id);
      const tile=node('article',undefined,'dashboard-tile');tile.dataset.card=id;tile.style.setProperty('--tile-span',card.w);
      const title=node('h3',card.label),badge=node('p','Backend work needed','dashboard-pending');
      const body=node('div',undefined,'dashboard-preview');
      if(card.id==='metric_chart'||card.id==='db_latency_chart'){const plot=node('div','No time-series data connected','dashboard-chart-empty');body.append(plot);}
      for(const label of card.fields){const row=node('div',undefined,'dashboard-field');row.append(node('span',label),node('span','—'));body.append(row);}
      if(!card.fields.length)body.append(node('div','—','dashboard-value'));
      const needs=node('p',card.id==='git_changes'?'Needs V3 Git status and action adapters.':'Needs V3 '+card.sections.join(', ')+' data.','dashboard-needs');
      tile.append(title,badge,body,needs);
      if(editing){
        const actions=node('div',undefined,'dashboard-tile-actions');
        for(const [label,delta] of [['Move earlier',-1],['Move later',1],['Remove tile',0]]){
          const button=node('button',label);button.type='button';button.setAttribute('aria-label',label+': '+card.label);
          button.disabled=delta<0?index===0:delta>0?index===selected.length-1:false;
          button.onclick=()=>{if(delta){[selected[index],selected[index+delta]]=[selected[index+delta],selected[index]];}else selected.splice(index,1);paint();};actions.append(button);
        }tile.append(actions);
      }grid.append(tile);
    }
    for(const card of catalog.cards){const button=node('button',card.label);button.type='button';button.disabled=selected.includes(card.id);button.onclick=()=>{selected.push(card.id);picker.hidden=true;add.setAttribute('aria-expanded','false');paint();};picker.append(button);}
  }
  add.onclick=()=>{picker.hidden=!picker.hidden;add.setAttribute('aria-expanded',String(!picker.hidden));};
  reset.onclick=()=>{selected=catalog.cards.map(card=>card.id);paint();};
  edit.onclick=()=>{editing=!editing;edit.setAttribute('aria-pressed',String(editing));edit.textContent=editing?'Done editing':'Edit layout';paint();};
  host.onkeydown=event=>{if(event.key==='Escape'){picker.hidden=true;add.setAttribute('aria-expanded','false');add.focus();}};
  paint();
}
