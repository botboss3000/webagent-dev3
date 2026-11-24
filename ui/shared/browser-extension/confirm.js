// Asks whether to link this browser to another account than the one it was linked to last.
// The service worker opened this window and waits for the answer; closing it means no.
const $ = id => document.getElementById(id);
(async () => {
  const ask = await chrome.runtime.sendMessage({type: 'confirm-state'});
  if (!ask?.id) { window.close(); return; }
  const question = $('question');
  question.replaceChildren('Link to ', Object.assign(document.createElement('strong'), {textContent: ask.account}),
    ` on ${ask.host}? It was last linked to ${ask.previous}` + (ask.previous_host && ask.previous_host !== ask.host ? ` on ${ask.previous_host}.` : '.'));
  const answer = link => async () => {
    $('link').disabled = $('keep').disabled = true;
    await chrome.runtime.sendMessage({type: 'confirm-answer', id: ask.id, link});
    window.close();
  };
  $('link').onclick = answer(true);
  $('keep').onclick = answer(false);
  $('link').disabled = $('keep').disabled = false;
  $('keep').focus();
})();
