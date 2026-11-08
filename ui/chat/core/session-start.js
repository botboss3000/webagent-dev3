/**
 * Decides whether a New-session control should start a conversation.
 *
 * An open conversation with no messages, display rows, runs, draft or staged
 * attachments is already the fresh session that control created, so it is reused
 * instead of leaving a duplicate blank conversation behind. A conversation that is
 * still loading also refuses, so a slow load cannot spawn a second session; one
 * that failed to load (deleted or unknown) does not, so New always recovers.
 */
export function shouldCreateSession({session, ready = false, failed = false, lastSeq = 0, runs = 0, rows = 0, draft = '', attachments = 0} = {}) {
  if (!session) return true;
  // Still loading: wait. Failed to load (deleted or unknown): start a new one.
  if (!ready) return failed === true;
  return lastSeq > 0 || runs > 0 || rows > 0 || attachments > 0
    || (typeof draft === 'string' && draft.trim() !== '');
}
