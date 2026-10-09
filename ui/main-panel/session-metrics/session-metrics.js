export async function start(mount) {
  const table = mount.querySelector('.session-metrics-table-wrap table');
  const body = table?.querySelector('tbody');
  if (!table || !body) return;

  try {
    const response = await fetch(new URL('./metrics.json', import.meta.url));
    if (!response.ok) throw new Error(`Metrics request failed: ${response.status}`);
    const records = await response.json();
    const columns = [...table.querySelectorAll('thead th')].map((th) => th.textContent.trim());
    const fields = [
      'recordId', 'task', 'step', 'sessionId', 'runId', 'parentRunId', 'agent', 'model', 'status',
      'submitted', 'started', 'finished', 'wallDuration', 'processes', 'totalTurns', 'totalMessages',
      'toolCalls', 'failedCalls', 'retryCalls', 'appSteps', 'inputTokens', 'outputTokens', 'cachedTokens',
      'cost', 'build', 'tests', 'uiCheck', 'verifiedOutcome', 'evidence', 'optimizationNotes'
    ];
    const fragment = document.createDocumentFragment();
    for (const record of records) {
      const row = document.createElement('tr');
      for (let index = 0; index < fields.length; index += 1) {
        const cell = document.createElement('td');
        cell.textContent = String(record[fields[index]] ?? '');
        row.append(cell);
      }
      fragment.append(row);
    }
    body.replaceChildren(fragment);
  } catch (error) {
    console.error('Unable to load session metrics', error);
    body.replaceChildren();
  }
}
