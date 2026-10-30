// Composer drafts are browser-local, one per session; '' is the new session that is not yet created.
// shortcut: drafts never expire and survive logout; add expiry or logout clearing if storage or shared devices matter.
const key=id=>'v3.chat.draft.'+(id||'new');
/** Reads a session's saved draft; missing or blocked storage reads as empty. */
export function readDraft(id,storage=globalThis.localStorage){try{return storage.getItem(key(id))||'';}catch{return '';}}
/** Saves a session's draft, removing it when blank; blocked storage is ignored. */
export function saveDraft(id,text,storage=globalThis.localStorage){try{if(text.trim())storage.setItem(key(id),text);else storage.removeItem(key(id));}catch{}}
/** Moves the new-session draft into a just-created session that has no draft of its own. */
export function adoptNewDraft(id,storage=globalThis.localStorage){const text=readDraft('',storage);if(!text||readDraft(id,storage))return;saveDraft(id,text,storage);saveDraft('','',storage);}
