// A plugin of any language calls a Session's or a line's data face by id: an
// address records ids only, and every call looks the target up again
// (ADR 027 D4, #2127). `sessions` is the process entry and finds the Session
// through Agents; `sessionScope` is the Session part's own entry, and it
// names no Session.
import type { Sessions, SessionScope, Withdrawal } from '../../npm/branch-services.js'

export async function readThroughAddresses(
  sessions: Sessions,
  sessionScope: SessionScope,
  sessionId: string,
  inputId: string,
): Promise<void> {
  const queue = sessions.session(sessionId).sessionScope
  const withdrawn: Withdrawal = await queue.inputQueue.withdraw({ input_id: inputId })
  const line = sessions.session(sessionId).line('main')
  const ending: boolean = await line.branchLine.ending()
  void [withdrawn, ending]

  const own: Withdrawal = await sessionScope.line('main').inputQueue.withdraw({ input_id: inputId })
  const ownEnding: boolean = await sessionScope.line('main').branchLine.ending()
  void [own, ownEnding]
}

// @ts-expect-error a Session address reaches a line, not a line's faces.
export const queue = (sessions: Sessions) => sessions.session('session').inputQueue
// @ts-expect-error a Session part's line address names no Session.
export const foreign = (scope: SessionScope) => scope.line('main', 'other')
