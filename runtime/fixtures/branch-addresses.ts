// A plugin of any language calls a Session's or a line's data face by id: the
// address records ids only, and every call looks the target up again
// (ADR 027 D4, #2127). `session(...)` is the runtime's entry for a process
// part; `line(...)` is the one a Session part uses, and it names no Session.
import { line, session } from '../../npm/branch-services.js'
import type { Withdrawal } from '../../npm/branch-services.js'

export async function readThroughAddresses(sessionId: string, inputId: string): Promise<void> {
  const lineAddress = session(sessionId).line('main')
  const ending: boolean = await lineAddress.branchLine.ending()
  const cursor: number | bigint | null = await lineAddress.branchLine.cursor('read')
  const withdrawn: Withdrawal = await lineAddress.inputQueue.withdraw({ input_id: inputId })
  void [ending, cursor, withdrawn]

  const own = line('main')
  const ownEnding: boolean = await own.branchLine.ending()
  void ownEnding
}

// @ts-expect-error a Session address reaches a line, not a line's faces.
export const queue = session('session').inputQueue
// @ts-expect-error a Session part's line address names no Session.
export const foreign = line('main', 'other')
