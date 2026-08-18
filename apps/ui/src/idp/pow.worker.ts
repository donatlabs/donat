/// <reference lib="webworker" />
/**
 * Where the proof of work is actually computed.
 *
 * A second of arithmetic on the main thread is a second of a frozen page, so
 * this is the same shape Rauthy's own frontend uses: one message in with the
 * challenge, one message out with the solution.
 */
import { solvePow } from './pow';

export type PowWorkerResult = { solution: string } | { error: string };

self.onmessage = (event: MessageEvent<string>) => {
  let result: PowWorkerResult;
  try {
    result = { solution: solvePow(event.data) };
  } catch (error) {
    result = { error: error instanceof Error ? error.message : String(error) };
  }
  self.postMessage(result);
};
