import { expect, test } from '@playwright/test';
import { gql, signIn } from './panel';

/**
 * The settings the panel serves, and one of them written to.
 *
 * Every screen here renders a field the engine serves for a configured
 * identity provider. A screen that loads is worth asserting because the
 * failure it guards against is silent: a field renamed in the built-in
 * declaration produces an error inside a card, not a crash.
 */

const SCREENS = [
  { path: '/users', heading: /users/i },
  { path: '/roles', heading: /roles/i },
  { path: '/groups', heading: /groups/i },
  { path: '/attributes', heading: /attributes/i },
  { path: '/scopes', heading: /scopes/i },
  { path: '/clients', heading: /applications/i },
  { path: '/blocked-ips', heading: /blocked/i },
  { path: '/sessions', heading: /sessions/i },
];

test.beforeEach(async ({ page }) => {
  await signIn(page);
});

for (const screen of SCREENS) {
  test(`${screen.path} loads without an error on it`, async ({ page }) => {
    await page.goto(screen.path);

    await expect(page.getByRole('heading', { name: screen.heading }).first()).toBeVisible();
    // The framework renders a refused query as a card saying so. Its absence
    // is the assertion: the engine served the field and the role could see it.
    await expect(page.getByText(/something went wrong/i)).toHaveCount(0);
  });
}

test('all eight are in the sidebar, under one section', async ({ page }) => {
  await page.goto('/users');

  const sidebar = page.getByRole('navigation').or(page.locator('[data-sidebar]')).first();
  await expect(sidebar.getByText('Identity').first()).toBeVisible();
  for (const label of [
    'Users',
    'Roles',
    'Groups',
    'Scopes',
    'Applications',
    'Attributes',
    'Blocked addresses',
    'Sessions',
  ]) {
    await expect(sidebar.getByText(label, { exact: true }).first()).toBeVisible();
  }
});

test('a role can be created, opened and removed again', async ({ page }) => {
  const name = `e2e-role-${Date.now()}`;

  try {
    await page.goto('/roles/create');
    await page.getByLabel(/^name/i).first().fill(name);
    await page.getByRole('button', { name: /^(create|save)$/i }).first().click();

    // Creating lands on the record, and this resource's engine offers no
    // single-record field — so opening it at all is the provider reading the
    // collection and finding the row. A regression there shows up here as
    // "Failed to load record" rather than as a wrong number somewhere.
    await page.waitForURL(/\/roles\/[^/]+$/, { timeout: 30_000 });
    await expect(page.getByText(/failed to load/i)).toHaveCount(0);
    await expect(page.getByText(name).first()).toBeVisible({ timeout: 30_000 });

    await page.goto('/roles');
    await expect(page.getByText(name).first()).toBeVisible({ timeout: 30_000 });

    await page.getByText(name).first().click();
    await page.getByRole('button', { name: /^delete$/i }).first().click();
    const confirm = page.getByRole('button', { name: /^(delete|confirm|yes)$/i }).last();
    if (await confirm.count()) await confirm.click();

    await page.goto('/roles');
    await expect(page.getByText(name)).toHaveCount(0, { timeout: 30_000 });
  } finally {
    // A failure anywhere above leaves the role behind, and the next run then
    // starts somewhere this one did not.
    const found = await gql<{ items: Array<{ id: string; name: string }> }>(
      page,
      `{ items: idp_roles { id name } }`,
    ).catch(() => ({ items: [] }));
    const left = found.items.find((role) => role.name === name);
    if (left) {
      await gql(page, `mutation ($id: ID!) { idp_role_delete(id: $id) }`, { id: left.id }).catch(
        () => undefined,
      );
    }
  }
});
