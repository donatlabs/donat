import { expect, test } from '@playwright/test';
import { OPERATOR, signIn } from './panel';

/**
 * Everything that happens around a login.
 *
 * The screen is this panel's and the protocol is the provider's, so these are
 * the cases where the two have to agree: which field appears when, what a
 * refusal means, and what is handed back to the provider rather than
 * half-implemented here.
 */

test('an operator signs in and lands in the panel', async ({ page }) => {
  await signIn(page);

  await expect(page).toHaveURL(/\/users/);
  await expect(page.getByRole('heading', { name: /users/i }).first()).toBeVisible();
});

test('the engine sends the browser to this panel\'s own screen, carrying the request', async ({
  page,
}) => {
  await page.goto('/auth/login');

  // The engine mints `state` and the PKCE challenge and redirects here with
  // them; losing either would turn a protected login into an unprotected one.
  await expect(page).toHaveURL(/\/idp\/authorize\?/);
  const url = new URL(page.url());
  expect(url.searchParams.get('client_id')).toBeTruthy();
  expect(url.searchParams.get('state')).toBeTruthy();
  expect(url.searchParams.get('code_challenge')).toBeTruthy();
  expect(url.searchParams.get('code_challenge_method')).toBe('S256');
});

test('the password is asked for only after the provider says it wants one', async ({ page }) => {
  await page.goto('/auth/login');

  await expect(page.getByTestId('idp-password')).toHaveCount(0);
  await page.getByTestId('idp-email').fill(OPERATOR.email);
  await page.getByTestId('idp-submit').click();

  await expect(page.getByTestId('idp-password')).toBeVisible({ timeout: 90_000 });
  // A first refusal is not a wrong password — the provider answers an unknown
  // account the same way — so nothing is claimed about it.
  await expect(page.getByTestId('idp-error')).toHaveCount(0);
});

test('a wrong password says so, and offers a reset', async ({ page }) => {
  await page.goto('/auth/login');
  await page.getByTestId('idp-email').fill(OPERATOR.email);
  await page.getByTestId('idp-submit').click();
  await expect(page.getByTestId('idp-password')).toBeVisible({ timeout: 90_000 });

  await page.getByTestId('idp-password').fill('not the password');
  await page.getByTestId('idp-submit').click();

  await expect(page.getByTestId('idp-error')).toContainText(/did not match/i, { timeout: 90_000 });
  await expect(page.getByTestId('idp-reset')).toBeVisible();
});

test('a reset link can be asked for, and the answer never says who exists', async ({ page }) => {
  await page.goto('/auth/login');
  await page.getByTestId('idp-email').fill(OPERATOR.email);
  await page.getByTestId('idp-submit').click();
  await expect(page.getByTestId('idp-password')).toBeVisible({ timeout: 90_000 });
  await page.getByTestId('idp-password').fill('not the password');
  await page.getByTestId('idp-submit').click();
  await expect(page.getByTestId('idp-reset')).toBeVisible({ timeout: 90_000 });

  await page.getByTestId('idp-reset').click();

  // Deliberately the same sentence whether or not the account is real: the
  // provider answers identically, and so should the page.
  await expect(page.getByTestId('idp-notice')).toContainText(/if that account exists/i, {
    timeout: 90_000,
  });
});

test('signing out ends the session and the panel asks for one again', async ({ page }) => {
  await signIn(page);
  await expect(page).toHaveURL(/\/users/);

  await page.goto('/auth/logout');
  // The shell redirects out of this page while it is still loading, and
  // Playwright calls an interrupted navigation an error. The interruption is
  // the behaviour under test, so it is the destination that matters.
  await page.goto('/users').catch(() => undefined);

  // No session, so the shell sends the browser back to the sign-in screen
  // rather than rendering a page whose every request would be refused.
  await page.waitForURL(/\/(login|idp\/authorize)/, { timeout: 90_000 });
});

/**
 * Creating an account from the sign-in screen.
 *
 * Whether anyone may is the provider's decision and it announces it nowhere,
 * so the panel is told by configuration and the provider is asked anyway. Both
 * answers are worth a test: this deployment keeps registration closed, and
 * "closed" has to arrive as a sentence rather than as a stack trace.
 */
test('creating an account is offered only when this build says so', async ({ page }) => {
  await page.goto('/auth/login');

  const open = page.getByTestId('idp-signup-open');
  test.skip(
    (await open.count()) === 0,
    'this build has VITE_DONAT_IDP_REGISTRATION off, which is the default',
  );

  await open.click();
  await page.getByTestId('idp-signup-email').fill(`e2e-signup-${Date.now()}@example.test`);
  await page.getByTestId('idp-signup-given').fill('New');
  await page.getByTestId('idp-signup-submit').click();

  // Either the provider took it, or it refuses self-registration — and both
  // are said in a sentence rather than left to the console.
  const notice = page.getByTestId('idp-notice');
  const error = page.getByTestId('idp-error');
  await expect(notice.or(error)).toBeVisible({ timeout: 90_000 });
  if (await error.count()) {
    await expect(error).toContainText(/does not let people|refused|too many/i);
  } else {
    await expect(notice).toContainText(/email/i);
  }
});
