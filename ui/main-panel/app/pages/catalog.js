export async function loadAppPages() {
  const response=await fetch(new URL('./catalog.json',import.meta.url),{cache:'no-store'});
  if(!response.ok)throw Error('App catalog unavailable');
  const catalog=await response.json(),ids=new Set();
  if(catalog.version!==2||!Array.isArray(catalog.pages))throw Error('Invalid App catalog');
  for(const page of catalog.pages){
    if(!/^[a-z][a-z0-9-]*$/.test(page.id)||ids.has(page.id)||!page.title||!page.panelId||!['directory','app_configuration'].includes(page.type))throw Error('Invalid App catalog');
    ids.add(page.id);
  }
  if(catalog.pages.filter(p=>p.type==='directory').length!==1||!catalog.pages.some(p=>p.id==='config'&&p.type==='directory'))throw Error('Invalid App directory');
  return [...catalog.pages].sort((a,b)=>(a.order||0)-(b.order||0));
}
