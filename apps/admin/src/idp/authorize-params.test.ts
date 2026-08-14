import { describe, expect, it } from 'vitest';
import { loginRequest, parseAuthorizeParams, providerPageUrl } from './authorize-params';

/**
 * The engine builds the authorization request; this page only carries it. So
 * what is checked here is that nothing is dropped, invented or reshaped on the
 * way through — a lost `code_challenge` would turn a PKCE-protected login into
 * one without, and the engine would (correctly) refuse the callback.
 */
const search =
  '?response_type=code&client_id=panel&redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fauth%2Fcallback' +
  '&scope=openid+profile&state=abc123&code_challenge=chal&code_challenge_method=S256';

describe('parseAuthorizeParams', () => {
  it('reads the request the engine sent', () => {
    expect(parseAuthorizeParams(search)).toEqual({
      clientId: 'panel',
      redirectUri: 'http://localhost:8080/auth/callback',
      scopes: ['openid', 'profile'],
      state: 'abc123',
      nonce: undefined,
      codeChallenge: 'chal',
      codeChallengeMethod: 'S256',
      resource: undefined,
      loginHint: undefined,
    });
  });

  it('has nothing to render without a client and a destination', () => {
    expect(parseAuthorizeParams('?client_id=panel')).toBeUndefined();
    expect(parseAuthorizeParams('')).toBeUndefined();
  });

  it('ignores a challenge method it does not recognise', () => {
    const params = parseAuthorizeParams(search.replace('code_challenge_method=S256', 'code_challenge_method=sha1'));
    expect(params?.codeChallengeMethod).toBeUndefined();
  });
});

describe('loginRequest', () => {
  const params = parseAuthorizeParams(search)!;

  it('carries every parameter through to the provider', () => {
    expect(loginRequest(params, { email: 'a@b.test', password: 'secret', pow: 'solved' })).toEqual({
      email: 'a@b.test',
      password: 'secret',
      pow: 'solved',
      client_id: 'panel',
      redirect_uri: 'http://localhost:8080/auth/callback',
      scopes: ['openid', 'profile'],
      state: 'abc123',
      nonce: undefined,
      code_challenge: 'chal',
      code_challenge_method: 'S256',
    });
  });

  it('omits the password on the first attempt, as the provider expects', () => {
    expect(loginRequest(params, { email: 'a@b.test', pow: 'solved' })).not.toHaveProperty('password');
  });

  it('percent-encodes state, which has to survive a redirect back to the engine', () => {
    const withState = parseAuthorizeParams(search.replace('state=abc123', 'state=a%2Bb%2Fc'))!;
    expect(loginRequest(withState, { email: 'a@b.test', pow: 'p' }).state).toBe('a%2Bb%2Fc');
  });

  it('drops a PKCE challenge that arrived without a method rather than sending half of one', () => {
    const partial = { ...params, codeChallengeMethod: undefined };
    const request = loginRequest(partial, { email: 'a@b.test', pow: 'p' });
    expect(request.code_challenge).toBeUndefined();
  });
});

describe('providerPageUrl', () => {
  it('hands the same request to the provider\'s own page', () => {
    expect(providerPageUrl('/auth/v1', search)).toBe(`/auth/v1/oidc/authorize${search}`);
  });
});
