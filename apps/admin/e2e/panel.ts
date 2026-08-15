import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, type APIRequestContext, type Page } from '@playwright/test';

/**
 * The two things every end-to-end test needs: a way in, and a way to arrange
 * the world before it looks at one.
 *
 * Setup goes through the panel's **own** GraphQL rather than round the back to
 * the identity provider directly. It is slower and it is the point: a fixture
 * that reaches past the thing under test can leave it broken and still pass.
 */

/**
 * The operator the default stack ships with.
 *
 * The password is generated per machine by `make env`, so there is no default
 * to hard-code here: it is read out of the repository's own `.env`, which is
 * the file the stack under test was started from. `PANEL_PASSWORD` overrides
 * it for a deployment somewhere else.
 */
function operatorPassword(): string {
  if (process.env.PANEL_PASSWORD) return process.env.PANEL_PASSWORD;
  const env = resolve(import.meta.dirname, '../../../.env');
  const found = existsSync(env)
    ? /^DONAT_ADMIN_PASSWORD=(.*)$/m.exec(readFileSync(env, 'utf8'))?.[1]?.trim()
    : undefined;
  if (!found) {
    throw new Error(
      `no operator password: ${env} has no DONAT_ADMIN_PASSWORD. ` +
        'Run `make env` and start the stack, or set PANEL_PASSWORD.',
    );
  }
  return found;
}

export const OPERATOR = {
  email: process.env.PANEL_EMAIL ?? 'operator@example.com',
  password: operatorPassword(),
};

/** The role the panel acts as, which is also the role a fixture must grant. */
export const OPERATOR_ROLE = process.env.PANEL_ROLE ?? 'rauthy_admin';

/**
 * Sign in through the real screen.
 *
 * Two steps, because that is what the provider asks for: an email, then — once
 * it has answered that it wants one — a password. The wait before the password
 * field is the proof of work being solved in a worker, and, after this suite's
 * own wrong-password cases, the delay the provider imposes on an address that
 * has been failing. Waiting it out is the point: a login that got faster after
 * repeated failures would be the bug.
 */
export async function signIn(
  page: Page,
  email = OPERATOR.email,
  password = OPERATOR.password,
): Promise<void> {
  await page.goto('/auth/login');
  await page.getByTestId('idp-email').fill(email);
  await page.getByTestId('idp-submit').click();

  await expect(page.getByTestId('idp-password')).toBeVisible({ timeout: 90_000 });
  await page.getByTestId('idp-password').fill(password);
  await page.getByTestId('idp-submit').click();

  await page.waitForURL((url) => !String(url).includes('/idp/authorize'), { timeout: 90_000 });
}

/** One GraphQL document, as the signed-in operator. */
export async function gql<T = unknown>(
  page: Page,
  query: string,
  variables: Record<string, unknown> = {},
): Promise<T> {
  const answer = await page.evaluate(
    async ([document, args, role]) => {
      const response = await fetch('/v1/graphql', {
        method: 'POST',
        credentials: 'include',
        headers: { 'content-type': 'application/json', 'X-Donat-Role': role as string },
        body: JSON.stringify({ query: document, variables: args }),
      });
      return response.json();
    },
    [query, variables, OPERATOR_ROLE] as const,
  );
  const body = answer as { data?: T; errors?: Array<{ message: string }> };
  if (body.errors?.length) {
    throw new Error(`GraphQL refused: ${body.errors.map((error) => error.message).join('; ')}`);
  }
  return body.data as T;
}

/** An email nothing else in the suite will collide with. */
export function scratchEmail(what: string): string {
  return `e2e-${what}-${Date.now()}@example.test`;
}

/**
 * Create an account and hand back its id, along with the way to remove it.
 *
 * `roles` is deliberately explicit: a test about somebody who may use the
 * panel and a test about somebody who may not differ by exactly this.
 */
export async function createAccount(
  page: Page,
  options: { email: string; givenName?: string; roles: string[] },
): Promise<{ id: string; remove: () => Promise<void> }> {
  const created = await gql<{ item: { id: string } }>(
    page,
    `mutation ($input: IdpUserCreateInput!) {
       item: idp_user_create(input: $input) { id email }
     }`,
    {
      input: {
        email: options.email,
        given_name: options.givenName ?? 'E2E',
        family_name: 'Fixture',
        roles: options.roles,
      },
    },
  );
  const id = created.item.id;
  return {
    id,
    remove: async () => {
      await gql(page, `mutation ($id: ID!) { idp_user_delete(id: $id) }`, { id });
    },
  };
}

/** Give an account a password, which is the only way a fixture can sign in as it. */
export async function setPassword(
  page: Page,
  account: { id: string; email: string; roles: string[] },
  password: string,
): Promise<void> {
  await gql(
    page,
    `mutation ($id: ID!, $input: IdpUserInput!) {
       item: idp_user_update(id: $id, input: $input) { id }
     }`,
    {
      id: account.id,
      input: {
        email: account.email,
        given_name: 'E2E',
        family_name: 'Fixture',
        roles: account.roles,
        enabled: true,
        email_verified: true,
        password,
      },
    },
  );
}

/** Unused here, but the shape a request-context fixture would take. */
export type Api = APIRequestContext;
