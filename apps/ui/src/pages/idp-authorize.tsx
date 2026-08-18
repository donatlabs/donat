import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from 'react';
import { useLocation } from 'react-router';
import {
  AuthPage,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
  Input,
  Label,
} from '@refinest/ui-shadcn';
import { IdpClient, type AuthorizeOutcome } from '../idp/client';
import { PowSolver } from '../idp/pow-solver';
import type { Terms, WebauthnLoginResponse } from '../idp/types';
import { IdpTerms } from './idp-terms';
import { signWithPasskey } from '../idp/webauthn';
import {
  loginRequest,
  parseAuthorizeParams,
  type AuthorizeParams,
} from '../idp/authorize-params';
import { IDP_BASE, IDP_REGISTRATION } from '../env';

/**
 * The sign-in screen — our markup, the provider's protocol.
 *
 * The engine redirects here instead of to the identity provider's own login
 * page (`DONAT_OIDC.authorization_endpoint` points at this route), and this
 * page then speaks to the provider exactly as its own page does: establish a
 * session, solve its proof of work, `POST /oidc/authorize`, and follow the
 * `Location` it answers with — which goes to the engine's `/auth/callback`.
 * Authorization code with PKCE, unchanged; the panel never holds a token and
 * the provider still owns every credential.
 *
 * **Two steps, on purpose.** The email is asked for first and the password only
 * after the provider says it wants one. That is the provider's design, not a
 * preference: an account with a passkey and no password never sees a password
 * field, and a browser cannot autofill one that is not on the page yet. It is
 * also why this is not the framework's own `<LoginForm>`, which asks for both
 * at once — the markup here follows that component's structure closely so the
 * two look like one interface.
 *
 * **Everything that can happen on the way in happens here.** A passkey is
 * signed on this page, new terms are read on this page, and a reset link from
 * an email lands on one of ours. Two answers are not sign-in steps at all: an
 * application demanding a second factor the account has not got, and an
 * account the provider wants updated first. Its own login page does not handle
 * those either — it points at the account screen, and so do we, except that
 * the account screen is now ours as well.
 */

type Handoff = 'update' | 'mfa';

const HANDOFF_REASON: Record<Handoff, string> = {
  update: 'This account has to be updated before signing in.',
  mfa: 'This application requires a second factor, and this account has none set up yet.',
};

export interface IdpAuthorizeFormProps {
  params: AuthorizeParams;
  client: IdpClient;
  solver: PowSolver;
  /**
   * Offer to create an account. Whether anyone may is the provider's decision
   * and it announces it nowhere, so this is configuration — see `env.ts`.
   */
  registration?: boolean;
}

export function IdpAuthorizeForm({
  params,
  client,
  solver,
  registration = false,
}: IdpAuthorizeFormProps): ReactElement {
  const [starting, setStarting] = useState(true);
  const [busy, setBusy] = useState(false);
  const [email, setEmail] = useState(params.loginHint ?? '');
  const [password, setPassword] = useState('');
  const [needsPassword, setNeedsPassword] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [handoff, setHandoff] = useState<Handoff | undefined>();
  const [passkey, setPasskey] = useState<WebauthnLoginResponse | undefined>();
  const [termsCode, setTermsCode] = useState<string | undefined>();
  const [terms, setTerms] = useState<Terms | undefined>();
  const [retryAt, setRetryAt] = useState<number | undefined>();
  const [offerReset, setOfferReset] = useState(false);
  const [signingUp, setSigningUp] = useState(false);
  const [givenName, setGivenName] = useState('');
  const [familyName, setFamilyName] = useState('');
  const passwordRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let cancelled = false;
    client
      .session()
      .then(() => {
        if (cancelled) return;
        setStarting(false);
        // Start the proof of work now, so it is ready by the time anyone has
        // finished typing. See `pow-solver.ts`.
        solver.prepare();
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setStarting(false);
        setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [client, solver]);

  useEffect(() => {
    if (needsPassword) passwordRef.current?.focus();
  }, [needsPassword]);

  const apply = useCallback(
    (outcome: AuthorizeOutcome) => {
      switch (outcome.kind) {
        case 'redirect':
          window.location.replace(outcome.location);
          return;
        case 'passkey':
          // Not a step of ours: the browser is about to ask for the key, and
          // the effect below is where that happens.
          setPasskey(outcome.challenge);
          return;
        case 'update-required':
          setHandoff('update');
          return;
        case 'terms-required':
          // The text itself is a second call; the effect below fetches it.
          setTermsCode(outcome.code);
          return;
        case 'mfa-required':
          setHandoff('mfa');
          return;
        case 'password-expired':
          setError('This password has expired and has to be reset.');
          setOfferReset(true);
          return;
        case 'rate-limited':
          setRetryAt(outcome.notBefore);
          setError(
            outcome.notBefore
              ? `Too many attempts. Try again after ${new Date(outcome.notBefore * 1000).toLocaleTimeString()}.`
              : 'Too many attempts. Try again later.',
          );
          setPassword('');
          return;
        case 'rejected':
          setError(outcome.message);
          return;
        case 'unauthorized':
          // The provider answers the same way to an unknown email and to a
          // missing password, so the first one of these means "ask for a
          // password" and any later one means the credentials were wrong.
          if (!needsPassword) {
            setNeedsPassword(true);
            return;
          }
          setError('That email and password did not match.');
          setOfferReset(true);
      }
    },
    [needsPassword],
  );

  /**
   * The passkey ceremony.
   *
   * It runs as soon as the provider asks for one, because it *is* the prompt:
   * the browser puts up its own dialogue, and a button of ours in front of it
   * would only be a button in front of a button. What is rendered meanwhile is
   * the reason that dialogue appeared.
   */
  useEffect(() => {
    if (!passkey) return;
    let cancelled = false;
    void (async () => {
      try {
        const challenge = await client.webauthnStart({ Login: passkey.code });
        const signed = await signWithPasskey(challenge.rcr, challenge.exp);
        const outcome = await client.webauthnFinish(challenge.code, signed);
        if (cancelled) return;
        setPasskey(undefined);
        apply(outcome);
      } catch (cause: unknown) {
        if (cancelled) return;
        setPasskey(undefined);
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [apply, client, passkey]);

  /** The terms themselves, once the provider says they are what is missing. */
  useEffect(() => {
    if (!termsCode) return;
    let cancelled = false;
    void (async () => {
      try {
        const latest = await client.terms();
        if (cancelled) return;
        if (!latest) {
          // 204: a deployment withdrew its terms between refusing this login
          // and being asked for them. Nothing to accept, so try the login again.
          setTermsCode(undefined);
          setError('The terms changed while you were signing in. Try again.');
          return;
        }
        setTerms(latest);
      } catch (cause: unknown) {
        if (cancelled) return;
        setTermsCode(undefined);
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, termsCode]);

  const answerTerms = useCallback(
    async (accept: boolean) => {
      if (!termsCode || !terms) return;
      setBusy(true);
      try {
        const outcome = await client.answerTerms(accept, termsCode, terms.ts);
        setTermsCode(undefined);
        setTerms(undefined);
        apply(outcome);
      } catch (cause: unknown) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [apply, client, terms, termsCode],
  );

  const submit = useCallback(
    async (event: React.FormEvent) => {
      event.preventDefault();
      setError('');
      setNotice('');
      if (!email) return;
      if (needsPassword && !password) {
        setError('A password is required.');
        return;
      }

      setBusy(true);
      try {
        const pow = await solver.take();
        apply(
          await client.authorize(
            loginRequest(params, {
              email,
              password: needsPassword ? password : undefined,
              pow,
            }),
          ),
        );
      } catch (cause: unknown) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [apply, client, email, needsPassword, params, password, solver],
  );

  const registerAccount = useCallback(
    async (event: React.FormEvent) => {
      event.preventDefault();
      setError('');
      setNotice('');
      if (!email || !givenName) {
        setError('An email and a first name are required.');
        return;
      }
      setBusy(true);
      try {
        const pow = await solver.take();
        const outcome = await client.register({
          email,
          given_name: givenName,
          family_name: familyName || undefined,
          pow,
          // No `redirect_uri`: the provider checks it against a list of its
          // own, and this application's callback is not on it — it refuses the
          // whole registration for a field that only decides where somebody
          // lands after setting a password.
        });
        if (outcome.kind === 'sent') {
          // The provider mails a link to set a password; the account is not
          // usable until they follow it, and saying so avoids a second try.
          setNotice('Check your email for a link to finish setting up the account.');
          setSigningUp(false);
        } else if (outcome.kind === 'closed') {
          setError('This deployment does not let people create their own accounts.');
        } else if (outcome.kind === 'rate-limited') {
          setError('Too many attempts. Try again later.');
        } else {
          setError(outcome.message);
        }
      } catch (cause: unknown) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [client, email, familyName, givenName, solver],
  );

  const requestReset = useCallback(async () => {
    setError('');
    setNotice('');
    setBusy(true);
    try {
      const pow = await solver.take();
      const outcome = await client.requestReset({
        email,
        pow,
        redirect_uri: params.redirectUri,
      });
      if (outcome.kind === 'sent') {
        // Said the same way whether or not the account exists — the provider
        // answers identically, and so should the page.
        setNotice('If that account exists, a reset link is on its way.');
        setOfferReset(false);
      } else if (outcome.kind === 'rate-limited') {
        setError('Too many attempts. Try again later.');
      } else {
        setError(outcome.message);
      }
    } catch (cause: unknown) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }, [client, email, params.redirectUri, solver]);

  if (terms) {
    return (
      <IdpTerms
        terms={terms}
        busy={busy}
        onAccept={() => void answerTerms(true)}
        onDecline={() => void answerTerms(false)}
      />
    );
  }

  // The browser's own dialogue is already up; this only says what it is for.
  if (passkey) {
    return (
      <Card className="mx-auto w-full max-w-sm" data-testid="idp-passkey">
        <CardHeader>
          <CardTitle>Use your passkey</CardTitle>
          <CardDescription className="text-balance">
            This account finishes signing in with a passkey. Your browser is asking for it now.
          </CardDescription>
        </CardHeader>
      </Card>
    );
  }

  if (handoff) {
    return (
      <Card className="mx-auto w-full max-w-sm" data-testid="idp-handoff">
        <CardHeader className="space-y-1.5 text-center">
          <CardTitle className="text-2xl">One more step</CardTitle>
          <CardDescription className="text-balance">{HANDOFF_REASON[handoff]}</CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-muted-foreground text-sm">
            {handoff === 'mfa'
              ? 'Add a passkey to your account, then sign in again.'
              : 'Your account has something to settle before this sign-in can finish.'}
          </p>
        </CardContent>
        <CardFooter>
          {/* A whole page load rather than a route change: this is the one
              place where the panel has a provider session and no engine one,
              and the account screen lives outside the guarded shell. */}
          <Button className="w-full" data-testid="idp-handoff-continue" asChild>
            <a href="/account">Go to your account</a>
          </Button>
        </CardFooter>
      </Card>
    );
  }

  const blocked = retryAt !== undefined && retryAt * 1000 > Date.now();

  if (signingUp) {
    return (
      <Card className="mx-auto w-full max-w-sm" data-testid="idp-signup">
        <CardHeader className="space-y-1.5 text-center">
          <CardTitle className="text-2xl">Create an account</CardTitle>
          <CardDescription className="text-balance">
            The provider will email you a link to set a password.
          </CardDescription>
        </CardHeader>
        <form onSubmit={registerAccount} noValidate>
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="idp-signup-email" required>
                Email
              </Label>
              <Input
                id="idp-signup-email"
                type="email"
                autoComplete="email"
                required
                data-testid="idp-signup-email"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="idp-signup-given" required>
                First name
              </Label>
              <Input
                id="idp-signup-given"
                autoComplete="given-name"
                required
                data-testid="idp-signup-given"
                value={givenName}
                onChange={(event) => setGivenName(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="idp-signup-family">Last name</Label>
              <Input
                id="idp-signup-family"
                autoComplete="family-name"
                data-testid="idp-signup-family"
                value={familyName}
                onChange={(event) => setFamilyName(event.target.value)}
              />
            </div>
            {error && (
              <p role="alert" className="font-medium text-destructive text-sm" data-testid="idp-error">
                {error}
              </p>
            )}
            {notice && (
              <p role="status" className="text-muted-foreground text-sm" data-testid="idp-notice">
                {notice}
              </p>
            )}
          </CardContent>
          <CardFooter className="flex-col gap-2">
            <Button type="submit" className="w-full" data-testid="idp-signup-submit" disabled={busy}>
              {busy ? 'Creating…' : 'Create the account'}
            </Button>
            <Button
              type="button"
              variant="ghost"
              className="w-full"
              data-testid="idp-signup-cancel"
              onClick={() => {
                setSigningUp(false);
                setError('');
              }}
            >
              Back to signing in
            </Button>
          </CardFooter>
        </form>
      </Card>
    );
  }

  return (
    <Card className="mx-auto w-full max-w-sm">
      <CardHeader className="space-y-1.5 text-center">
        <CardTitle className="text-2xl">Sign in</CardTitle>
        <CardDescription className="text-balance">
          {needsPassword ? 'Enter the password for this account.' : 'Continue with your account.'}
        </CardDescription>
      </CardHeader>
      <form onSubmit={submit} noValidate>
        <CardContent className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="idp-email" required>
              Email
            </Label>
            <Input
              id="idp-email"
              type="email"
              autoComplete="email"
              required
              aria-required="true"
              data-testid="idp-email"
              value={email}
              readOnly={needsPassword}
              onChange={(event) => setEmail(event.target.value)}
            />
          </div>

          {needsPassword && (
            <div className="space-y-1">
              <Label htmlFor="idp-password" required>
                Password
              </Label>
              <Input
                id="idp-password"
                type="password"
                autoComplete="current-password"
                required
                aria-required="true"
                data-testid="idp-password"
                ref={passwordRef}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            </div>
          )}

          {error && (
            <p role="alert" className="font-medium text-destructive text-sm" data-testid="idp-error">
              {error}
            </p>
          )}
          {notice && (
            <p role="status" className="text-muted-foreground text-sm" data-testid="idp-notice">
              {notice}
            </p>
          )}
        </CardContent>
        <CardFooter className="flex-col gap-2">
          <Button
            type="submit"
            className="w-full"
            data-testid="idp-submit"
            disabled={starting || busy || blocked}
          >
            {busy ? 'Signing in…' : needsPassword ? 'Sign in' : 'Continue'}
          </Button>
          {offerReset && (
            <Button
              type="button"
              variant="ghost"
              className="w-full"
              data-testid="idp-reset"
              disabled={busy}
              onClick={() => {
                void requestReset();
              }}
            >
              Send a password reset link
            </Button>
          )}
          {registration && !needsPassword && (
            <Button
              type="button"
              variant="ghost"
              className="w-full"
              data-testid="idp-signup-open"
              disabled={busy}
              onClick={() => {
                setSigningUp(true);
                setError('');
                setNotice('');
              }}
            >
              Create an account
            </Button>
          )}
        </CardFooter>
      </form>
    </Card>
  );
}

/** The route: reads the authorization request, then renders the form. */
export default function IdpAuthorizePage(): ReactElement {
  const location = useLocation();
  const params = useMemo(() => parseAuthorizeParams(location.search), [location.search]);
  const client = useMemo(() => new IdpClient(IDP_BASE), []);
  const solver = useMemo(() => new PowSolver(() => client.challenge()), [client]);

  return (
    <AuthPage footer="donat">
      {params ? (
        <IdpAuthorizeForm
          params={params}
          client={client}
          solver={solver}
          registration={IDP_REGISTRATION}
        />
      ) : (
        <Card className="mx-auto w-full max-w-sm">
          <CardHeader className="space-y-1.5 text-center">
            <CardTitle className="text-2xl">Nothing to sign in to</CardTitle>
            <CardDescription className="text-balance">
              This page renders an authorization request, and none arrived with it.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <p className="text-muted-foreground text-sm">
              Start at <code>/auth/login</code>, which is where the engine builds the request.
            </p>
          </CardContent>
        </Card>
      )}
    </AuthPage>
  );
}
