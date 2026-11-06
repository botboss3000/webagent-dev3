/**
 * Decides whether a New-session control should start a conversation.
 *
 * An open conversation with no messages, display rows, runs, draft or staged
 * attachments is already the fresh session that control created, so it is reused
 * instead of leaving a duplicate blank conversation behind. A conversation that is
 * still loading also refuses, so a slow load cannot spawn a second session.
 */
export function shouldCreateSession({session, ready = false, lastSeq = 0, runs = 0, rows = 0, draft = '', attachments = 0} = {}) {
  if (!session) return true;
  if (!ready) return false;
  return lastSeq > 0 || runs > 0 || rows > 0 || attachments > 0
    || (typeof draft === 'string' && draft.trim() !== '');
}
