import { expect, test } from '@playwright/test';
import { OPERATOR_ROLE, createAccount, gql, scratchEmail, setPassword, signIn } from './panel';

/**
 * What an operator does to somebody's account, checked by that somebody then
 * signing in.
 *
 * A password set through a form is only really set if it works at the login
 * screen, and a role granted is only really granted if the panel lets that
 * account in. Both of those cross the panel, the engine and the provider, so
 * neither can be asserted anywhere but here.
 */

test('a password set in the form is the password that signs in', async ({ page, browser }) => {
  await signIn(page);
  const email = scratchEmail('password');
  const account = await createAccount(page, { email, roles: [OPERATOR_ROLE] });

  try {
    // Through the form rather than the API: the field is optional, and an
    // empty one has to mean "leave it alone" rather than "blank it".
    await page.goto(`/users/${account.id}/edit`);
    const field = page.getByLabel(/set a new password/i);
    await expect(field).toBeVisible({ timeout: 30_000 });
    const password = 'E2e-fixture-2026';
    await field.fill(password);
    await page.getByRole('button', { name: /save|update/i }).first().click();
    await expect(page.getByText(/something went wrong/i)).toHaveCount(0);

    const other = await browser.newContext();
    const theirs = await other.newPage();
    await signIn(theirs, email, password);
    await expect(theirs).toHaveURL(/\/users/);
    await other.close();
  } finally {
    await account.remove();
  }
});

test('an account without the panel\'s role is told so, not shown an error', async ({
  page,
  browser,
}) => {
  await signIn(page);
  const email = scratchEmail('norole');
  // Deliberately no roles: this is the account that can authenticate and still
  // may not act as anything here.
  const account = await createAccount(page, { email, roles: [] });

  try {
    const password = 'E2e-norole-2026';
    await setPassword(page, { id: account.id, email, roles: [] }, password);

    const other = await browser.newContext();
    const theirs = await other.newPage();
    await signIn(theirs, email, password);

    // Signed in — the provider minted a token — and still refused here, with
    // the reason and the only action that helps.
    //
    // The panel asserts no role of its own, so the reason is not a mismatch
    // with one: it is that this account was granted nothing at all, which is
    // the case worth catching whether or not a stand names a role.
    await expect(theirs.getByTestId('wrong-role')).toBeVisible({ timeout: 30_000 });
    await expect(theirs.getByTestId('wrong-role')).toContainText(/no roles/i);
    await expect(theirs.getByTestId('wrong-role-sign-out')).toBeVisible();
    await expect(theirs.getByText(/something went wrong/i)).toHaveCount(0);
    await other.close();
  } finally {
    await account.remove();
  }
});

test('an account created in the panel appears in the list and can be removed', async ({ page }) => {
  await signIn(page);
  const email = scratchEmail('lifecycle');

  await page.goto('/users/create');
  await page.getByLabel(/^email/i).first().fill(email);
  await page.getByLabel(/first name/i).first().fill('E2E');
  await page.getByRole('button', { name: /save|create|submit/i }).first().click();

  await page.goto('/users');
  await expect(page.getByText(email).first()).toBeVisible({ timeout: 30_000 });

  const found = await gql<{ items: Array<{ id: string; email: string }> }>(
    page,
    `{ items: idp_users { id email } }`,
  );
  const created = found.items.find((item) => item.email === email);
  expect(created, 'the account reached the provider, not just the screen').toBeTruthy();

  await gql(page, `mutation ($id: ID!) { idp_user_delete(id: $id) }`, { id: created!.id });
  await page.goto('/users');
  await expect(page.getByText(email)).toHaveCount(0, { timeout: 30_000 });
});

test('the form refuses an address that is not one, before the provider has to', async ({ page }) => {
  await signIn(page);

  await page.goto('/users/create');
  await page.getByLabel(/^email/i).first().fill('dawdaw');
  await page.getByLabel(/first name/i).first().fill('E2E');
  await page.getByRole('button', { name: /save|create|submit/i }).first().click();

  // The provider answers such a thing with its validator's Debug output, which
  // is true and unreadable. Nothing should have been sent.
  await expect(page.getByText(/ValidationErrors/)).toHaveCount(0);
  await expect(page).toHaveURL(/\/users\/create/);
});
