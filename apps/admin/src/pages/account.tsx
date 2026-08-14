/**
 * The account, in this panel.
 *
 * The provider ships a page for this and sends people to it from three places:
 * a login that needs a second factor, one that needs the account updated, and
 * its own menus. All three land here instead. Nothing about the protocol
 * changes — these are its endpoints, called with the session cookie of the
 * browser that signed in, so the provider still decides what somebody may do
 * to their own record. What changes is that it looks like the rest of the
 * panel and is reached without leaving it.
 *
 * This screen acts as a *person*. The `Identity` screens act as the
 * deployment, through the engine and its API key. Keeping the two apart is
 * deliberate: nothing here can reach an account other than the caller's own,
 * because the only thing it holds is that person's own session.
 */
import { useCallback, useEffect, useState, type ReactElement } from 'react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
  Input,
  Label,
  Separator,
} from '@refinest/ui-shadcn';

import {
  AccountClient,
  policyFailures,
  type Account,
  type PasswordPolicy,
  type Passkey,
} from '../idp/account';
import { createPasskey, passkeysAvailable, type CredentialCreation } from '../idp/webauthn';

const when = (seconds?: number) =>
  seconds ? new Date(seconds * 1000).toLocaleString() : undefined;

export interface AccountScreenProps {
  client?: AccountClient;
}

export function AccountScreen({ client: given }: AccountScreenProps = {}): ReactElement {
  const [client] = useState(() => given ?? new AccountClient());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [account, setAccount] = useState<Account | undefined>();
  const [policy, setPolicy] = useState<PasswordPolicy | undefined>();
  const [passkeys, setPasskeys] = useState<Passkey[]>([]);

  const [givenName, setGivenName] = useState('');
  const [familyName, setFamilyName] = useState('');
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [keyName, setKeyName] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    const session = await client.session();
    const [record, rules] = await Promise.all([client.account(session.user_id), client.policy()]);
    setAccount(record);
    setGivenName(record.given_name ?? '');
    setFamilyName(record.family_name ?? '');
    // A deployment may serve no passkeys at all; that is not an error here.
    setPasskeys(await client.passkeys(session.user_id).catch(() => []));
    setPolicy(rules);
  }, [client]);

  useEffect(() => {
    let cancelled = false;
    void load()
      .catch((cause: unknown) => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [load]);

  const act = useCallback(
    async (what: () => Promise<string>) => {
      setError('');
      setNotice('');
      setBusy(true);
      try {
        setNotice(await what());
      } catch (cause: unknown) {
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const saveProfile = () =>
    void act(async () => {
      if (!account) throw new Error('Nothing loaded.');
      const updated = await client.update(account.id, {
        email: account.email,
        given_name: givenName,
        family_name: familyName,
      });
      setAccount(updated);
      return 'Saved.';
    });

  const failures = policy ? policyFailures(password, policy) : [];
  const passwordReady = password.length > 0 && password === repeat && failures.length === 0;

  const changePassword = () =>
    void act(async () => {
      if (!account) throw new Error('Nothing loaded.');
      await client.update(account.id, { email: account.email, password });
      setPassword('');
      setRepeat('');
      return 'That password is now in force.';
    });

  const enrol = () =>
    void act(async () => {
      if (!account) throw new Error('Nothing loaded.');
      const name = keyName.trim() || 'passkey';
      const challenge = (await client.passkeyStart(account.id, name)) as CredentialCreation;
      const created = await createPasskey(challenge);
      await client.passkeyFinish(account.id, name, created);
      setPasskeys(await client.passkeys(account.id));
      setKeyName('');
      return `"${name}" is enrolled.`;
    });

  const forget = (name: string) =>
    void act(async () => {
      if (!account) throw new Error('Nothing loaded.');
      await client.passkeyDelete(account.id, name);
      setPasskeys(await client.passkeys(account.id));
      return `"${name}" is gone.`;
    });

  if (loading) {
    return <div className="text-muted-foreground p-6">Loading…</div>;
  }

  if (!account) {
    return (
      <div className="p-6" data-testid="account-error">
        <p className="text-destructive text-sm">{error || 'That account could not be read.'}</p>
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-3xl space-y-6 p-6" data-testid="account">
      <div>
        <h1 className="text-2xl font-semibold">Your account</h1>
        <p className="text-muted-foreground text-sm">{account.email}</p>
        <div className="mt-2 flex flex-wrap gap-1">
          {(account.roles ?? []).map((role) => (
            <Badge key={role} variant="secondary">
              {role}
            </Badge>
          ))}
        </div>
      </div>

      {error && (
        <p className="text-destructive text-sm" data-testid="account-error">
          {error}
        </p>
      )}
      {notice && (
        <p className="text-sm text-emerald-600" data-testid="account-notice">
          {notice}
        </p>
      )}

      <Card>
        <CardHeader>
          <CardTitle>Profile</CardTitle>
          <CardDescription>
            What this deployment shows other people. Your email is changed by the deployment.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <div className="grid gap-2">
            <Label htmlFor="given-name">First name</Label>
            <Input
              id="given-name"
              data-testid="account-given-name"
              value={givenName}
              onChange={(event) => setGivenName(event.target.value)}
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="family-name">Last name</Label>
            <Input
              id="family-name"
              data-testid="account-family-name"
              value={familyName}
              onChange={(event) => setFamilyName(event.target.value)}
            />
          </div>
        </CardContent>
        <CardFooter>
          <Button onClick={saveProfile} disabled={busy} data-testid="account-save-profile">
            Save
          </Button>
        </CardFooter>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Password</CardTitle>
          <CardDescription>
            {policy
              ? `At least ${policy.length_min} characters.`
              : 'The deployment sets what a password has to be.'}
            {account.password_expires
              ? ` This one expires ${when(account.password_expires)}.`
              : ''}
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <div className="grid gap-2">
            <Label htmlFor="new-password">New password</Label>
            <Input
              id="new-password"
              type="password"
              autoComplete="new-password"
              data-testid="account-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="repeat-password">Again</Label>
            <Input
              id="repeat-password"
              type="password"
              autoComplete="new-password"
              data-testid="account-password-repeat"
              value={repeat}
              onChange={(event) => setRepeat(event.target.value)}
            />
          </div>
          {/* Every reason at once: a form that reports one at a time makes
              somebody guess how many are left. */}
          {password && failures.length > 0 && (
            <p className="text-muted-foreground text-sm sm:col-span-2" data-testid="account-policy">
              Needs {failures.join(', ')}.
            </p>
          )}
          {repeat && password !== repeat && (
            <p className="text-destructive text-sm sm:col-span-2" data-testid="account-mismatch">
              Those two do not match.
            </p>
          )}
        </CardContent>
        <CardFooter>
          <Button
            onClick={changePassword}
            disabled={busy || !passwordReady}
            data-testid="account-save-password"
          >
            Change it
          </Button>
        </CardFooter>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Passkeys</CardTitle>
          <CardDescription>
            A key on a device you hold. Some applications require one before they let you in.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {passkeys.length === 0 && (
            <p className="text-muted-foreground text-sm" data-testid="account-no-passkeys">
              None enrolled.
            </p>
          )}
          {passkeys.map((key) => (
            <div key={key.name} data-testid={`account-passkey-${key.name}`}>
              <div className="flex items-center justify-between gap-4">
                <div>
                  <p className="text-sm font-medium">{key.name}</p>
                  <p className="text-muted-foreground text-xs">
                    Added {when(key.registered)}
                    {key.last_used ? ` · last used ${when(key.last_used)}` : ''}
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => forget(key.name)}
                  data-testid={`account-forget-${key.name}`}
                >
                  Remove
                </Button>
              </div>
              <Separator className="mt-3" />
            </div>
          ))}
          {passkeysAvailable() ? (
            <div className="flex items-end gap-2">
              <div className="grid flex-1 gap-2">
                <Label htmlFor="key-name">Name a new key</Label>
                <Input
                  id="key-name"
                  placeholder="This laptop"
                  data-testid="account-key-name"
                  value={keyName}
                  onChange={(event) => setKeyName(event.target.value)}
                />
              </div>
              <Button onClick={enrol} disabled={busy} data-testid="account-enrol">
                Add a passkey
              </Button>
            </div>
          ) : (
            <p className="text-muted-foreground text-sm">
              This browser cannot hold a passkey.
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
