import { describe, expect, it, vi } from 'vitest';
import { FRESH_MS, PowSolver } from './pow-solver';

/**
 * The solver exists for one reason — a solution is single-use and expires — so
 * that is what these check: never hand the same one out twice, and never hand
 * out one old enough for the provider to have forgotten it.
 */
function solver(options: { now?: () => number } = {}) {
  let issued = 0;
  const challenges: string[] = [];
  const instance = new PowSolver(
    () => {
      issued += 1;
      const challenge = `challenge-${issued}`;
      challenges.push(challenge);
      return Promise.resolve(challenge);
    },
    (challenge) => Promise.resolve(`${challenge}:solved`),
    options.now,
  );
  return { instance, challenges, issued: () => issued };
}

describe('PowSolver', () => {
  it('solves ahead, so a submit does not wait for it', async () => {
    const { instance, issued } = solver();
    instance.prepare();
    expect(issued()).toBe(1);

    await expect(instance.take()).resolves.toBe('challenge-1:solved');
  });

  it('never hands the same solution out twice', async () => {
    const { instance } = solver();
    instance.prepare();

    const first = await instance.take();
    const second = await instance.take();
    expect(second).not.toBe(first);
  });

  it('works without preparation, for a page that submits immediately', async () => {
    const { instance } = solver();
    await expect(instance.take()).resolves.toBe('challenge-1:solved');
  });

  it('throws away a solution the provider would call expired', async () => {
    const clock = vi.fn(() => 0);
    const { instance } = solver({ now: clock });
    instance.prepare();

    clock.mockReturnValue(FRESH_MS + 1);
    // The prepared one is stale, so a fresh challenge is fetched instead.
    await expect(instance.take()).resolves.toBe('challenge-2:solved');
  });

  it('keeps preparing after a failure, so a retry is not slower', async () => {
    let attempt = 0;
    const instance = new PowSolver(
      () => {
        attempt += 1;
        return attempt === 1 ? Promise.reject(new Error('the provider said no')) : Promise.resolve('ok');
      },
      (challenge) => Promise.resolve(`${challenge}:solved`),
    );
    instance.prepare();

    await expect(instance.take()).rejects.toThrow('the provider said no');
    await expect(instance.take()).resolves.toBe('ok:solved');
  });
});
