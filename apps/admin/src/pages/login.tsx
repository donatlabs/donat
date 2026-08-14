import { useLocation } from 'react-router';
import { AuthPage, Button } from '@refinest/ui-shadcn';
import { useAuth } from '../auth/auth-context';
import { DONAT_ROLE } from '../env';

/**
 * Sign-in.
 *
 * One control, and deliberately so. donat owns no identity — it stores no
 * users, holds no passwords and issues no tokens
 * (`knowledgebase/api-surfaces/decisions/010-donat-does-not-own-identity.md`) —
 * so the credentials belong to the provider's own page, and everything
 * interactive lives there with them: a second factor, a passkey, a password
 * reset, its own rate limiting and proof-of-work.
 *
 * A deployment can put this panel's own markup in front of that provider
 * instead — `pages/idp-authorize.tsx` renders the sign-in screen here and
 * speaks the provider's unchanged protocol underneath — in which case
 * `/auth/login` still starts the flow and simply lands there. This page is what
 * the default shape looks like, and `scripts/idp-theme.mjs` sends the panel's
 * palette to the provider's page so even that one is not a different interface.
 */
export default function LoginPage(): React.ReactElement {
  const auth = useAuth();
  const location = useLocation();
  const from =
    (location.state as { from?: string } | null)?.from ??
    new URLSearchParams(location.search).get('redirect') ??
    '/';

  return (
    <AuthPage footer="donat admin">
      <div className="space-y-4">
        <div className="space-y-1">
          <h1 className="font-semibold text-xl">donat admin</h1>
          <p className="text-muted-foreground text-sm">
            The panel runs as the <code>{DONAT_ROLE}</code> role and sees exactly what that role's
            permissions grant.
          </p>
        </div>
        <Button className="w-full" onClick={() => auth.signIn(from)}>
          Sign in
        </Button>
        <p className="text-muted-foreground text-xs">
          You will be sent to your identity provider. donat stores no users and issues no tokens.
        </p>
      </div>
    </AuthPage>
  );
}
