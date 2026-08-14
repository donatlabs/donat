/**
 * Keeping a solved proof of work ready.
 *
 * Two facts shape this. A solution is **single-use** — the provider's own
 * integration tests fetch and solve a fresh one before every attempt — and a
 * challenge **expires**, after 30 seconds by default. Solving takes one to
 * three seconds here, and asking an operator to wait that long *after* pressing
 * the button would make our page feel slower than the one it replaces.
 *
 * So the solver works ahead: it fetches and solves as soon as the page opens,
 * hands that solution over when the form is submitted, and immediately starts
 * preparing the next one. A held solution older than [`FRESH_MS`] is thrown
 * away rather than risked against the provider's expiry.
 */
import type { PowWorkerResult } from './pow.worker';

/** How long a prepared solution is trusted, against a 30-second expiry. */
export const FRESH_MS = 15_000;

/** Solving, injectable: a worker in the browser, the function itself in tests. */
export type Solve = (challenge: string) => Promise<string>;

/** Fetching a challenge, injectable for the same reason. */
export type FetchChallenge = () => Promise<string>;

/** Solve in a worker, so a slow challenge cannot freeze the page. */
export const solveInWorker: Solve = (challenge) =>
  new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./pow.worker.ts', import.meta.url), { type: 'module' });
    worker.onmessage = (event: MessageEvent<PowWorkerResult>) => {
      worker.terminate();
      if ('solution' in event.data) resolve(event.data.solution);
      else reject(new Error(event.data.error));
    };
    worker.onerror = (event) => {
      worker.terminate();
      reject(new Error(event.message || 'the proof-of-work worker failed'));
    };
    worker.postMessage(challenge);
  });

interface Attempt {
  startedAt: number;
  solution: Promise<string>;
}

export class PowSolver {
  private attempt: Attempt | undefined;

  constructor(
    private readonly fetchChallenge: FetchChallenge,
    private readonly solve: Solve = solveInWorker,
    private readonly now: () => number = () => Date.now(),
  ) {}

  /**
   * Start working on one, if nothing usable is in hand. Safe to call whenever
   * the page has a reason to think a login is coming.
   */
  prepare(): void {
    if (this.attempt && this.now() - this.attempt.startedAt < FRESH_MS) return;
    const startedAt = this.now();
    const solution = this.fetchChallenge().then(this.solve);
    // Nothing awaits this until `take()`, and an unhandled rejection in the
    // meantime is noise: the error is delivered there, where it can be shown.
    solution.catch(() => undefined);
    this.attempt = { startedAt, solution };
  }

  /**
   * A solution, for exactly one attempt. Consumes what was prepared — or waits
   * for a fresh one when nothing usable is in hand — and leaves the next one
   * already under way.
   */
  async take(): Promise<string> {
    const held = this.attempt;
    this.attempt = undefined;
    const usable = held && this.now() - held.startedAt < FRESH_MS ? held.solution : undefined;
    try {
      return await (usable ?? this.fetchChallenge().then(this.solve));
    } finally {
      this.prepare();
    }
  }
}
