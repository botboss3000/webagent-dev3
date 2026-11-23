// Revision-fenced, caller-owned preferences; browser storage is not authoritative.
export function createPagePreferences({request,isCurrent=()=>true,onError=()=>{}}){
  let revision=0,pages=['config'],ready=false,epoch=0,queue=Promise.resolve();
  const load=async()=>{const snapshot=await request('/user/app-page-favorites');if(!isCurrent())return ['config'];revision=snapshot.revision;pages=snapshot.page_ids;ready=true;return pages.slice();};
  return {load,ready:()=>ready,
    save(order,visibleIds){
      const generation=epoch,selected=order.slice(),visible=new Set(visibleIds);
      const task=queue.then(async()=>{
        if(!isCurrent()||!ready||generation!==epoch)return null;
        const next=[...selected,...pages.filter(id=>!visible.has(id))];
        try{
          const saved=await request('/user/app-page-favorites','PUT',{page_ids:next,expected_revision:revision});
          if(!isCurrent())return null;
          revision=saved.revision;pages=saved.page_ids;onError('');return null;
        }catch(error){
          if(!isCurrent())return null;
          ready=false;epoch++;onError('App favorites were not saved: '+error.message);
          try{return await load();}catch{onError('App favorites could not be reloaded. Reload to retry.');return null;}
        }
      });queue=task.catch(()=>{});return task;
    }
  };
}
