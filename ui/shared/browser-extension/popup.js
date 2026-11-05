const $ = id => document.getElementById(id);
async function render() {
  const state = await chrome.runtime.sendMessage({type: 'link-state'});
  $('status').textContent = !state?.user_id
    ? 'Not linked. Open WebAgent in this browser and sign in; it links automatically.'
    : `Linked to ${state.user_id} on ${new URL(state.origin).host}.` + (state.paused ? ' You have control; the agent is waiting.' : '');
  $('unlink').hidden = !state?.user_id;
  $('hand-back').hidden = !state?.paused;
}
$('unlink').onclick = async () => { await chrome.runtime.sendMessage({type: 'unlink'}); await render(); };
$('hand-back').onclick = async () => { await chrome.runtime.sendMessage({type: 'hand-back'}); await render(); };
render();
