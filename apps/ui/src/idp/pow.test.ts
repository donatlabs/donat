import { describe, expect, it } from 'vitest';
import { sha256 } from '@noble/hashes/sha2.js';
import { difficultyOf, hasLeadingZeroBits, MAX_DIFFICULTY, PowError, solvePow } from './pow';

/**
 * The proof of work is the one piece of the provider's login this panel had to
 * re-implement rather than call, so it is tested against the property the
 * provider verifies — not against a fixture of our own making.
 *
 * The challenges below are shaped like the provider's
 * (`1:<difficulty>:<expires>:<salt>:<hash>:`) but carry a low difficulty: the
 * default of 19–20 bits takes seconds, which is fine for a person waiting to
 * sign in and not fine for a test suite.
 */
const challenge = (difficulty: number) =>
  `1:${difficulty}:1802682422:Rhs5wflYb9mpiDQX:F+CSBSpalGG6FvfSUYjN8zw95z/LYd7jnnu+lYhA3wI:`;

describe('hasLeadingZeroBits', () => {
  it('counts bits, not bytes', () => {
    expect(hasLeadingZeroBits(new Uint8Array([0b0000_1111]), 4)).toBe(true);
    expect(hasLeadingZeroBits(new Uint8Array([0b0000_1111]), 5)).toBe(false);
    expect(hasLeadingZeroBits(new Uint8Array([0, 0b0001_1111]), 11)).toBe(true);
    expect(hasLeadingZeroBits(new Uint8Array([0, 0b0001_1111]), 12)).toBe(false);
  });

  it('accepts whole zero bytes', () => {
    expect(hasLeadingZeroBits(new Uint8Array([0, 0, 1]), 16)).toBe(true);
    expect(hasLeadingZeroBits(new Uint8Array([0, 1, 0]), 16)).toBe(false);
  });
});

describe('difficultyOf', () => {
  it('reads the difficulty the provider asked for', () => {
    expect(difficultyOf(challenge(20))).toBe(20);
    expect(difficultyOf(challenge(11))).toBe(11);
  });

  it('refuses anything that is not a version 1 challenge', () => {
    expect(difficultyOf('2:20:1:s:h:')).toBeUndefined();
    expect(difficultyOf('')).toBeUndefined();
    expect(difficultyOf('1:xx:1:s:h:')).toBeUndefined();
    // Outside spow's own 10..=98 range.
    expect(difficultyOf('1:09:1:s:h:')).toBeUndefined();
  });
});

describe('solvePow', () => {
  it('answers with the challenge and a counter that satisfies it', () => {
    const input = challenge(12);
    const solution = solvePow(input);

    expect(solution.startsWith(input)).toBe(true);
    const counter = solution.slice(input.length);
    expect(counter).toMatch(/^\d+$/);
    // The provider re-hashes exactly this string and checks the same bits.
    expect(hasLeadingZeroBits(sha256(new TextEncoder().encode(solution)), 12)).toBe(true);
  });

  it('finds the smallest counter, as the reference implementation does', () => {
    const input = challenge(10);
    const counter = Number.parseInt(solvePow(input).slice(input.length), 10);
    const encoder = new TextEncoder();
    for (let earlier = 0; earlier < counter; earlier += 1) {
      expect(hasLeadingZeroBits(sha256(encoder.encode(input + earlier)), 10)).toBe(false);
    }
  });

  it('refuses a challenge it does not understand instead of looping', () => {
    expect(() => solvePow('nonsense')).toThrow(PowError);
  });

  it('refuses a difficulty it could not finish, rather than hanging the tab', () => {
    expect(() => solvePow(challenge(MAX_DIFFICULTY + 1))).toThrow(/solves up to/);
  });
});
