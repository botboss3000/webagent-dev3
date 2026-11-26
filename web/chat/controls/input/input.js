// The composer's text field. It carries no behaviour of its own: draft saving, Enter
// handling and autosizing are the composer's decisions, taken from config, so this
// control only renders the field that config describes.

export function create(settings = {}) {
  const field = document.createElement('textarea');
  field.id = 'chat-input';
  field.className = 'composer-input';
  field.rows = Number(settings.rows) > 0 ? Number(settings.rows) : 1;
  field.setAttribute('aria-label', settings.label || 'Message');
  if (typeof settings.placeholder === 'string' && settings.placeholder) {
    field.placeholder = settings.placeholder;
  }
  field.autocomplete = 'off';
  field.autocapitalize = 'off';
  field.spellcheck = true;
  return field;
}
