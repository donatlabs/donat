/**
 * Proof of work — the client's half of the provider's bot defence.
 *
 * The provider hands out a challenge and refuses a login that does not carry a
 * solution to it. That is not something a login page may skip, so this is a
 * faithful port of the algorithm in `spow` (the crate Rauthy uses, Apache-2.0),
 * not a re-design of it:
 *
 *   challenge = "1:<difficulty>:<expires>:<salt>:<hash>:"
 *   solution  = challenge + counter, for the smallest counter ≥ 0 whose
 *               SHA-256(challenge + counter) begins with `difficulty` zero bits
 *
 * The provider verifies in O(1) — it re-derives the challenge from its own
 * secret and checks the zero bits — so the asymmetry is the whole point.
 *
 * **Why JavaScript.** Rauthy's own page solves this in WebAssembly, ~10× faster
 * than this does. Vendoring that module would mean a Rust toolchain and
 * `wasm-pack` in the panel's build, and the panel is deliberately a plain npm
 * project outside the Cargo workspace. At the provider's default difficulty of
 * 19–20 bits this takes roughly one to three seconds — which is why the solver
 * runs in a worker and starts before the operator has finished typing (see
 * `pow-solver.ts`), so the cost is paid during typing rather than after submit.
 */
import { sha256 } from '@noble/hashes/sha2.js';

const encoder = new TextEncoder();

/**
 * The ceiling this implementation will attempt.
 *
 * Each bit doubles the work: 24 bits is around half a minute here, and the 99
 * that `spow` permits would never finish. A deployment that raises
 * `POW_DIFFICULTY` past this gets a clear error instead of a hung tab, and the
 * fix is either to lower it or to let the provider serve its own page.
 */
export const MAX_DIFFICULTY = 24;

/** Does `hash` begin with `count` zero **bits**? */
export function hasLeadingZeroBits(hash: Uint8Array, count: number): boolean {
  const whole = count >> 3;
  for (let index = 0; index < whole; index += 1) {
    if (hash[index] !== 0) return false;
  }
  const remainder = count & 7;
  if (remainder === 0) return true;
  return hash[whole] >> (8 - remainder) === 0;
}

/** The difficulty a challenge declares, or `undefined` if it is not one. */
export function difficultyOf(challenge: string): number | undefined {
  // `1:20:1702682422:Rhs5wflYb9mpiDQX:F+CSBSpalGG6FvfSUYjN8zw95z/…:`
  if (challenge.length < 5 || challenge[0] !== '1') return undefined;
  const difficulty = Number.parseInt(challenge.slice(2, 4), 10);
  if (!Number.isInteger(difficulty) || difficulty < 10 || difficulty > 98) return undefined;
  return difficulty;
}

export class PowError extends Error {}

/**
 * Solve a challenge. Blocking and CPU-bound by construction — call it in a
 * worker, not on the interface's thread.
 */
export function solvePow(challenge: string): string {
  const difficulty = difficultyOf(challenge);
  if (difficulty === undefined) {
    throw new PowError('the provider sent a proof-of-work challenge in an unknown format');
  }
  if (difficulty > MAX_DIFFICULTY) {
    throw new PowError(
      `the provider asks for ${difficulty} bits of proof of work; this page solves up to ${MAX_DIFFICULTY}`,
    );
  }

  for (let counter = 0; ; counter += 1) {
    const candidate = challenge + counter;
    if (hasLeadingZeroBits(sha256(encoder.encode(candidate)), difficulty)) {
      return candidate;
    }
  }
}
