import { createContext, useContext, useMemo, type ReactNode } from 'react';
import { DONAT_ROLE } from '../env';
import { loadCsrfToken } from './csrf';
import {
  createTransport,
  signIn,
  type AuthTransport,
  type RequestAuth,
  type SessionState,
} from './session';

/**
 * React binding over the {@link AuthTransport}.
 *
 * It holds no token, no user record and no state at all — the session lives
 * in a cookie this code cannot read, by design. What the tree needs from here
 * is the role it runs as, a way to attach that role to a request, and the two
 * navigations that start and end a session.
 */
export interface AuthState {
  /** The default role, for a panel that declares no stand of its own. */
  readonly role: string;
  authorize: (role?: string) => RequestAuth;
  session: () => Promise<SessionState>;
  recover: () => Promise<boolean>;
  signIn: (returnTo?: string) => void;
  signOut: () => void;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({
  children,
  transport,
}: {
  children: ReactNode;
  transport?: AuthTransport;
}): React.ReactElement {
  const value = useMemo<AuthState>(() => {
    const active = transport ?? createTransport(DONAT_ROLE);
    return {
      role: DONAT_ROLE,
      authorize: (role?: string) => active.authorize(role ?? DONAT_ROLE),
      // The provider's CSRF token comes with the session check, because the
      // two answer the same question — is this browser still signed in — and
      // because every write after it needs the token. A deployment whose
      // identity fields carry a key of their own simply never gets one.
      session: async () => {
        const [session] = await Promise.all([active.session(), loadCsrfToken()]);
        return session;
      },
      recover: () => active.recover(),
      signIn,
      signOut: () => active.signOut(),
    };
  }, [transport]);

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthState {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error('useAuth: wrap with <AuthProvider>');
  return ctx;
}
