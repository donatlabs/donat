/**
 * The password reset a link in an email leads to.
 *
 * The link the provider mails points at `GET /users/{id}/reset/{reset_id}`,
 * and that request does two things: it sets a binding cookie without which the
 * change is refused, and it answers with the provider's own page carrying the
 * values that change needs. The engine sends the browser here instead, and
 * this makes the same request itself — so the cookie is set on the same origin
 * exactly as before, and the values come from the same place.
 *
 * They arrive as `<template id="tpl_…">` elements in that HTML, which is how
 * the provider hands data to its own frontend. Reading them is not scraping a
 * page: it is reading the data channel the page is built on, and it is pinned
 * by tests against the real shape.
 */

export interface ResetInfo {
  userId: string;
  magicLinkId: string;
  csrfToken: string;
  needsMfa: boolean;
  policy?: {
    length_min: number;
    length_max: number;
    include_lower_case?: number;
    include_upper_case?: number;
    include_digits?: number;
    include_special?: number;
  };
}

export class ResetError extends Error {
  constructor(
    message: string,
    readonly expired = false,
  ) {
    super(message);
    this.name = 'ResetError';
  }
}

/**
 * Read one `tpl_*` value out of the provider's HTML.
 *
 * A parser rather than a regular expression over markup, because the browser
 * has one and it does not get confused by a value that contains a `<`.
 */
export function templateValue(html: string, id: string): string | undefined {
  const document_ = new DOMParser().parseFromString(html, 'text/html');
  const element = document_.getElementById(id);
  if (!element) return undefined;
  // A `<template>` is inert: its children live in a fragment, so the element's
  // own `textContent` is empty however plainly the markup reads.
  const source = element instanceof HTMLTemplateElement ? element.content : element;
  const value = source.textContent?.trim();
  return value ? value : undefined;
}

/**
 * The same values, bundled.
 *
 * Rauthy 0.36 stopped publishing a template per value and hands its own
 * frontend one `tpl_password_reset` carrying the lot as JSON. Both shapes are
 * read here, because which one arrives is the provider's version rather than
 * the deployment's choice — and reading only the older one is not a graceful
 * degradation but a lie: against 0.36 this page told people their link "has
 * been used already, or has expired" seconds after it was emailed.
 */
interface BundledReset {
  csrf_token?: string;
  magic_link_id?: string;
  user_id?: string;
  needs_mfa?: boolean;
  password_policy?: ResetInfo['policy'];
}

function bundledReset(html: string): BundledReset | undefined {
  const raw = templateValue(html, 'tpl_password_reset');
  if (!raw) return undefined;
  try {
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === 'object' ? (parsed as BundledReset) : undefined;
  } catch {
    // A template that is not JSON tells us nothing. Fall through to the
    // per-value ids rather than failing on a shape we did not expect.
    return undefined;
  }
}

/** Everything the reset form needs, from that one request. */
export function parseReset(html: string, fallbackUserId: string): ResetInfo {
  const bundle = bundledReset(html);
  const csrfToken = bundle?.csrf_token ?? templateValue(html, 'tpl_csrf_token');
  const magicLinkId = bundle?.magic_link_id ?? templateValue(html, 'tpl_magic_link_id');
  if (!csrfToken || !magicLinkId) {
    // The provider serves an error page for a link that was used, expired, or
    // never existed — it has no CSRF token in it, because there is nothing to
    // authorise.
    throw new ResetError('That link has been used already, or has expired.', true);
  }
  const policy = templateValue(html, 'tpl_password_policy');
  return {
    userId: bundle?.user_id ?? templateValue(html, 'tpl_user_id') ?? fallbackUserId,
    magicLinkId,
    csrfToken,
    needsMfa: bundle?.needs_mfa ?? templateValue(html, 'tpl_needs_mfa') === 'true',
    policy:
      bundle?.password_policy ??
      (policy ? (JSON.parse(policy) as ResetInfo['policy']) : undefined),
  };
}

export class ResetClient {
  constructor(
    private readonly base = '/auth/v1',
    private readonly fetchImpl: typeof fetch = (...args) => fetch(...args),
  ) {}

  /**
   * Follow the emailed link ourselves.
   *
   * `credentials: 'same-origin'` is the whole point: this is what plants the
   * binding cookie that the change below is checked against.
   */
  async open(userId: string, resetId: string): Promise<ResetInfo> {
    const response = await this.fetchImpl(
      `${this.base}/users/${encodeURIComponent(userId)}/reset/${encodeURIComponent(resetId)}`,
      { method: 'GET', credentials: 'same-origin', headers: { accept: 'text/html' } },
    );
    if (!response.ok) {
      throw new ResetError('That link has been used already, or has expired.', true);
    }
    return parseReset(await response.text(), userId);
  }

  /**
   * Set the password.
   *
   * The token goes in `x-pwd-csrf-token` rather than the ordinary CSRF header:
   * nobody is signed in here, so this is a token for one reset rather than for
   * a session.
   */
  async change(info: ResetInfo, password: string, mfaCode?: string): Promise<string> {
    const response = await this.fetchImpl(
      `${this.base}/users/${encodeURIComponent(info.userId)}/reset`,
      {
        method: 'PUT',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json', 'x-pwd-csrf-token': info.csrfToken },
        body: JSON.stringify({
          password,
          magic_link_id: info.magicLinkId,
          mfa_code: mfaCode,
        }),
      },
    );
    if (!response.ok) {
      let message = `The identity provider answered ${response.status}.`;
      try {
        const body = (await response.json()) as { message?: string };
        if (body.message) message = body.message;
      } catch {
        // No JSON body; the status is what there is.
      }
      throw new ResetError(message);
    }
    // Where to go next is the provider's to say, and it says it in a header.
    return response.headers.get('location') ?? '/account';
  }
}
