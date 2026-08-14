/**
 * Where the reset link in an email lands.
 *
 * The provider mails a link to its own page. The engine turns that link into
 * this route, and this page then makes the very request the link named — which
 * is what sets the binding cookie the change is checked against — and reads
 * the values it needs out of the answer. The protocol is unchanged; only the
 * markup is ours.
 *
 * A link is spent the moment it is used, so the honest failure is loud: this
 * says the link is gone rather than showing a form that cannot succeed.
 */
import { useCallback, useEffect, useState, type ReactElement } from 'react';
import { useParams } from 'react-router';
import {
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

import { policyFailures } from '../idp/account';
import { ResetClient, type ResetInfo } from '../idp/reset';

export interface IdpResetFormProps {
  userId: string;
  resetId: string;
  client?: ResetClient;
}

export function IdpResetForm({ userId, resetId, client: given }: IdpResetFormProps): ReactElement {
  const [client] = useState(() => given ?? new ResetClient());
  const [info, setInfo] = useState<ResetInfo | undefined>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [mfaCode, setMfaCode] = useState('');

  useEffect(() => {
    let cancelled = false;
    void client
      .open(userId, resetId)
      .then((opened) => {
        if (!cancelled) setInfo(opened);
      })
      .catch((cause: unknown) => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client, resetId, userId]);

  const failures = info?.policy ? policyFailures(password, info.policy) : [];
  const ready =
    !!info && password.length > 0 && password === repeat && failures.length === 0 && !busy;

  const submit = useCallback(
    async (event: React.FormEvent) => {
      event.preventDefault();
      if (!info) return;
      setError('');
      setBusy(true);
      try {
        const next = await client.change(info, password, mfaCode || undefined);
        window.location.replace(next);
      } catch (cause: unknown) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [client, info, mfaCode, password],
  );

  if (loading) {
    return <p className="text-muted-foreground p-6 text-sm">Checking that link…</p>;
  }

  if (!info) {
    return (
      <Card className="mx-auto w-full max-w-sm" data-testid="reset-spent">
        <CardHeader>
          <CardTitle>That link no longer works</CardTitle>
          <CardDescription className="text-balance">{error}</CardDescription>
        </CardHeader>
        <CardFooter>
          <Button className="w-full" asChild>
            <a href="/login">Back to signing in</a>
          </Button>
        </CardFooter>
      </Card>
    );
  }

  return (
    <Card className="mx-auto w-full max-w-sm" data-testid="reset">
      <CardHeader>
        <CardTitle>Set a new password</CardTitle>
        <CardDescription className="text-balance">
          {info.policy
            ? `At least ${info.policy.length_min} characters.`
            : 'Choose one this deployment will accept.'}
        </CardDescription>
      </CardHeader>
      <form onSubmit={(event) => void submit(event)}>
        <CardContent className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="reset-password">New password</Label>
            <Input
              id="reset-password"
              type="password"
              autoComplete="new-password"
              data-testid="reset-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="reset-repeat">Again</Label>
            <Input
              id="reset-repeat"
              type="password"
              autoComplete="new-password"
              data-testid="reset-repeat"
              value={repeat}
              onChange={(event) => setRepeat(event.target.value)}
            />
          </div>
          {info.needsMfa && (
            <div className="grid gap-2">
              <Label htmlFor="reset-mfa">Code from your second factor</Label>
              <Input
                id="reset-mfa"
                data-testid="reset-mfa"
                value={mfaCode}
                onChange={(event) => setMfaCode(event.target.value)}
              />
            </div>
          )}
          {password && failures.length > 0 && (
            <p className="text-muted-foreground text-sm" data-testid="reset-policy">
              Needs {failures.join(', ')}.
            </p>
          )}
          {repeat && password !== repeat && (
            <p className="text-destructive text-sm" data-testid="reset-mismatch">
              Those two do not match.
            </p>
          )}
          {error && (
            <p className="text-destructive text-sm" data-testid="reset-error">
              {error}
            </p>
          )}
        </CardContent>
        <CardFooter>
          <Button type="submit" className="w-full" disabled={!ready} data-testid="reset-submit">
            Set it
          </Button>
        </CardFooter>
      </form>
    </Card>
  );
}

/** The route: the two ids come from the path the engine redirected to. */
export default function IdpResetPage(): ReactElement {
  const { userId, resetId } = useParams();
  if (!userId || !resetId) {
    return <p className="text-destructive p-6 text-sm">That link is incomplete.</p>;
  }
  return (
    <div className="flex min-h-svh items-center justify-center p-6">
      <IdpResetForm userId={userId} resetId={resetId} />
    </div>
  );
}
