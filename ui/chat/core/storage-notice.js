// Tells the signed-in user, once per page load, that their storage is nearly full.
// The server owns the limit and threshold (storage-policy.json); this only presents them.
import {showNotification} from './chat-action-panel.js';
const MIB=1024*1024;
export function createStorageNotice({request,show=showNotification}){
  let checked=false;
  return async function check(){
    if(checked)return;checked=true;
    try{
      const status=await request('/storage');
      if(status?.near_limit!==true||!Number.isSafeInteger(status.used_bytes)||!Number.isSafeInteger(status.limit_bytes))return;
      const used=Math.ceil(status.used_bytes/MIB),limit=Math.floor(status.limit_bytes/MIB);
      const percent=Math.min(100,Math.round(status.used_bytes*100/status.limit_bytes));
      show({key:'storage-limit',title:'Storage nearly full',timeout:0,
        message:`You are using ${used} MB of ${limit} MB (${percent}%). Delete sessions or attachments you no longer need; uploads stop at the limit.`});
    }catch{/* The notice is advisory; a failed read shows nothing. */}
  };
}
