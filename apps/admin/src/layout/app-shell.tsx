import { useEffect, useState, type ReactNode } from 'react';
import { Link, useLocation, useNavigate } from 'react-router';
import { useDataRuntime } from '@refinest/react';
import {
  AppHeader,
  AppLayout,
  AppSidebar,
  Badge,
  Button,
  DropdownMenuItem,
  NavMain,
  NavUser,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from '@refinest/ui-shadcn';
import { Check, Database, RefreshCw } from 'lucide-react';
import { useAuth } from '../auth/auth-context';
import { useStand } from '../stands/stand-context';
import { AppBreadcrumbs } from './app-breadcrumbs';

/**
 * Sidebar brand and stand switcher.
 *
 * The stand is named here rather than buried in a settings page because it is
 * the single most important thing on the screen: which deployment, and as
 * which role. Every row below it is that answer.
 */
function SidebarBrand(): React.ReactElement {
  const { stand, stands, select } = useStand();
  const navigate = useNavigate();
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton size="lg" className="cursor-default hover:bg-transparent">
          <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
            <Database className="size-4" />
          </div>
          <div className="grid flex-1 text-left leading-tight">
            <span className="truncate font-semibold">donat admin</span>
            <span className="truncate text-muted-foreground text-xs">{stand.label}</span>
          </div>
        </SidebarMenuButton>
      </SidebarMenuItem>
      {stands.length > 1 &&
        stands.map((candidate) => (
          <SidebarMenuItem key={candidate.id}>
            <SidebarMenuButton
              size="sm"
              isActive={candidate.id === stand.id}
              aria-current={candidate.id === stand.id ? 'true' : undefined}
              onClick={() => {
                if (candidate.id === stand.id) return;
                select(candidate.id);
                // The route belongs to the stand that was showing: another
                // deployment does not have it, and staying would leave the
                // operator on a blank not-found. `/` resolves to whatever the
                // new stand's registry lists first.
                navigate('/', { replace: true });
              }}
            >
              {candidate.id === stand.id ? (
                <Check className="size-3.5" />
              ) : (
                <span className="size-3.5" />
              )}
              <span className="truncate">{candidate.label}</span>
              <span className="ml-auto text-muted-foreground text-xs">{candidate.role}</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        ))}
    </SidebarMenu>
  );
}

/** Invalidates every live data-runtime query so the current view refetches. */
function RefreshButton(): React.ReactElement {
  const runtime = useDataRuntime();
  const [refreshing, setRefreshing] = useState(false);
  return (
    <Button
      type="button"
      size="icon"
      variant="ghost"
      aria-label="Refresh"
      data-testid="refresh"
      disabled={refreshing}
      onClick={async () => {
        setRefreshing(true);
        try {
          await runtime.queryClient.invalidateQueries({
            predicate: (q) => q.queryKey[0] === 'refinest-data' && q.queryKey[1] === 1,
          });
        } finally {
          setRefreshing(false);
        }
      }}
    >
      <RefreshCw className={refreshing ? 'h-4 w-4 animate-spin' : 'h-4 w-4'} />
    </Button>
  );
}

/**
 * The admin shell: registry-driven sidebar, breadcrumbs, refresh, sign-out,
 * and the auth guard.
 *
 * The header shows the active role because it is the only thing that decides
 * what this panel can do. There is no admin role in this engine — the panel is
 * that role and nothing more — so naming it in the chrome is honest rather
 * than decorative.
 */
export default function AppShell({ children }: { children: ReactNode }): React.ReactElement {
  const auth = useAuth();
  const { stand } = useStand();
  const location = useLocation();
  const [checked, setChecked] = useState(false);
  // What the engine says this session is acting as, for the places that
  // display it. A stand that names no role learns it only from the session.
  const [sessionRole, setSessionRole] = useState('');
  // Set when the caller is signed in but their token does not grant the role
  // this stand runs as. Not an error: an answer.
  const [wrongRole, setWrongRole] = useState<{ wanted: string; granted: string[] } | undefined>();

  // Ask the engine who this browser is, before rendering anything that would
  // query it.
  //
  // A refused request is not a reliable signal on its own: a deployment that
  // sets `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` answers an unauthenticated request
  // successfully, as that role, and the operator would be left looking at an
  // empty screen wondering why. The cookie is `HttpOnly`, so this code cannot
  // decide it locally either — only the engine can, and `/auth/session` is it
  // saying so.
  useEffect(() => {
    let cancelled = false;
    void auth.session().then((session) => {
      if (cancelled) return;
      if (!session.authenticated) {
        // `signIn` knows which login this build has — this panel's own form,
        // or the provider's page. Going straight to the provider here is what
        // made a build with its own form still show the provider's markup.
        auth.signIn(location.pathname);
        return;
      }
      // A stand *is* a role, and a token that does not grant it will have
      // every query refused with "Your requested role is not in allowed
      // roles". Saying so here costs one comparison and turns a screen full of
      // failed requests into one sentence. An engine that sends no list at all
      // is not second-guessed.
      // An engine that says nothing is not second-guessed; one that sends a
      // list this stand's role is not in is answered — including the empty
      // list, which is an account the provider gave no roles at all.
      // Two different refusals, and both are worth saying rather than letting
      // every request fail on its own.
      //
      // A stand that names a role can predict a mismatch. One that names none
      // acts as whatever the token says, so there is nothing to disagree with
      // — except an account the provider granted *nothing*, which can
      // authenticate and still act as no one. That one is worth catching
      // whether or not a role was named.
      if (session.roles) {
        const granted = [...session.roles];
        const refused = stand.role ? !granted.includes(stand.role) : granted.length === 0;
        if (refused) setWrongRole({ wanted: stand.role, granted });
      }
      setSessionRole(session.role ?? '');
      setChecked(true);
    });
    return () => {
      cancelled = true;
    };
    // The check belongs to the browser's session, not to the route it is on:
    // re-running it per navigation would ask the engine on every click. The
    // role is in here because the comparison below is about it — switching
    // stands switches roles, and a stand switch has to be re-checked.
  }, [auth, stand.role]);

  if (!checked) {
    return <div className="p-6 text-muted-foreground">Signing in…</div>;
  }

  if (wrongRole) {
    return (
      <div className="mx-auto max-w-md space-y-4 p-10" data-testid="wrong-role">
        <h1 className="font-semibold text-xl">This account cannot use the panel</h1>
        <p className="text-muted-foreground text-sm">
          It is signed in, but{' '}
          {wrongRole.wanted ? (
            <>
              the panel acts as <code>{wrongRole.wanted}</code> and this account holds{' '}
            </>
          ) : (
            <>this account holds{' '}</>
          )}
          {wrongRole.granted.length ? (
            <>
              only{' '}
              {wrongRole.granted.map((role, index) => (
                <span key={role}>
                  {index > 0 ? ', ' : ''}
                  <code>{role}</code>
                </span>
              ))}
            </>
          ) : (
            'no roles'
          )}
          . Someone who administers this deployment can grant it.
        </p>
        <Button
          variant="outline"
          data-testid="wrong-role-sign-out"
          onClick={() => {
            void auth.signOut();
          }}
        >
          Sign out
        </Button>
      </div>
    );
  }

  return (
    <AppLayout
      sidebar={
        <AppSidebar
          switcher={<SidebarBrand />}
          nav={<NavMain topLabel={null} currentPath={location.pathname} />}
          footer={
            <NavUser
              name={stand.role || sessionRole || 'signed in'}
              email="engine session"
              avatarFallback={(stand.role || sessionRole || '?').slice(0, 2).toUpperCase()}
              menu={
                /* The account screen replaces the provider's own page, so the
                   way to it belongs where somebody looks for themselves. */
                <DropdownMenuItem asChild data-testid="nav-account">
                  <Link to="/account">Your account</Link>
                </DropdownMenuItem>
              }
              onLogout={() => auth.signOut()}
            />
          }
        />
      }
      header={
        <AppHeader
          breadcrumbs={<AppBreadcrumbs />}
          actions={
            <>
              <Badge variant="outline">role: {stand.role || sessionRole || '—'}</Badge>
              <RefreshButton />
            </>
          }
        />
      }
    >
      {children}
    </AppLayout>
  );
}
