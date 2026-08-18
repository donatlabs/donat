import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Link, useLocation, useNavigate, useSearchParams } from 'react-router';
import { QueryClient } from '@tanstack/react-query';
import {
  createDataSession,
  createDispatcher,
  type AppRuntime,
  type DataSessionController,
  type RouteDescriptor,
} from '@refinest/core';
import { defineContextSlices, createResolver, type Resolver } from '@refinest/context';
import { createI18n, createI18nextAdapter, type I18nFacade } from '@refinest/i18n-core';
import {
  RefinestProvider,
  RelationAggregatorProvider,
  RouterAdapterProvider,
  createDataRuntime,
  createDataRuntimeTransitionParticipant,
  useDataRuntime,
  en as frameworkEn,
  type DataRuntimeLease,
} from '@refinest/react';
import {
  DispatcherProvider,
  Toaster,
  createNotificationProvider,
  shadcnFieldImplementations,
  type LinkComponent,
  LinkComponentProvider,
} from '@refinest/ui-shadcn';
import { AuthProvider, useAuth } from '../auth/auth-context';
import { createDonatDataProvider } from '../data/donat-data-provider';
import { createAdminApp } from '../refinest-app';
import { StandProvider, useStand } from '../stands/stand-context';
import { standMappings, type Stand } from '../stands';

/**
 * The application mount: router adapter, auth, app runtime, data runtime,
 * i18n, and the action dispatcher.
 *
 * Ported from the Solar UI's `components/providers.tsx` and trimmed
 * of what it needed and this does not: the current-event scope provider, the
 * file-storage plugin, and the token-refresh plumbing (donat issues no
 * tokens, so there is nothing to refresh here — see `auth/session.ts`).
 */

/** React Router's Link behind the framework's router-agnostic contract. */
const RouterLinkAdapter: LinkComponent = ({ href, children, ...rest }) => (
  <Link to={href} {...rest}>
    {children}
  </Link>
);

/**
 * Binds `@refinest/react`'s URL-state hooks to React Router. Navigations are
 * coalesced through a microtask so several state writes in one render settle
 * into a single history entry.
 */
function ReactRouterAdapter({ children }: { children: ReactNode }): React.ReactElement {
  const navigate = useNavigate();
  const location = useLocation();
  const [search] = useSearchParams();
  const pendingNavigation = useRef<{ url: string; mode: 'push' | 'replace' } | null>(null);
  const navigationScheduled = useRef(false);

  const scheduleNavigation = useCallback(
    (url: string, mode: 'push' | 'replace'): void => {
      const prior = pendingNavigation.current;
      pendingNavigation.current = {
        url,
        mode: prior?.mode === 'push' || mode === 'push' ? 'push' : 'replace',
      };
      if (navigationScheduled.current) return;
      navigationScheduled.current = true;
      queueMicrotask(() => {
        const pending = pendingNavigation.current;
        pendingNavigation.current = null;
        navigationScheduled.current = false;
        if (pending) navigate(pending.url, { replace: pending.mode === 'replace' });
      });
    },
    [navigate],
  );

  const adapter = useMemo(
    () => ({
      pathname: location.pathname,
      searchParams: new URLSearchParams(search.toString()),
      replace: (url: string) => scheduleNavigation(url, 'replace'),
      push: (url: string) => scheduleNavigation(url, 'push'),
    }),
    [location.pathname, search, scheduleNavigation],
  );
  return <RouterAdapterProvider adapter={adapter}>{children}</RouterAdapterProvider>;
}

/** Resolve an action's `redirect` descriptor to a URL path. */
function resolveRedirect(to: RouteDescriptor): string {
  if (typeof to === 'string') return to;
  const base = `/${to.resource}`;
  const action = to.action ?? 'list';
  const params = to.params ?? {};
  const id = params.id;
  let path: string;
  if (action === 'list') path = base;
  else if (action === 'create') path = `${base}/create`;
  else if (action === 'edit') path = id != null ? `${base}/${id}/edit` : `${base}/edit`;
  else path = id != null ? `${base}/${id}` : base;
  const qs = Object.entries(params)
    .filter(([k, v]) => k !== 'id' && v != null)
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
    .join('&');
  return qs ? `${path}?${qs}` : path;
}

/**
 * The action dispatcher, mounted inside the data runtime so `refetch`
 * operates on framework descriptor keys: `notify` → a toast, `refetch` →
 * query invalidation (all, or the named resources), `redirect` → navigation.
 */
function NativeDispatcher({
  notificationProvider,
  children,
}: {
  notificationProvider: ReturnType<typeof createNotificationProvider>;
  children: ReactNode;
}): React.ReactElement {
  const navigate = useNavigate();
  const runtime = useDataRuntime();
  const dispatcher = useMemo(
    () =>
      createDispatcher({
        notify: (n) =>
          notificationProvider.open(
            typeof n === 'string'
              ? { type: 'success', message: n }
              : { type: n.type === 'info' ? 'progress' : n.type, message: n.message },
          ),
        refetch: async (resources) => {
          if (resources === false) return;
          if (resources === true) {
            await runtime.queryClient.invalidateQueries({
              predicate: (query) =>
                query.queryKey[0] === 'refinest-data' && query.queryKey[1] === 1,
            });
            return;
          }
          const requested = new Set(resources);
          await runtime.queryClient.invalidateQueries({
            predicate: (query) => {
              const descriptor = query.queryKey[2];
              return (
                query.queryKey[0] === 'refinest-data' &&
                query.queryKey[1] === 1 &&
                descriptor !== null &&
                typeof descriptor === 'object' &&
                !Array.isArray(descriptor) &&
                requested.has((descriptor as { readonly resource?: unknown }).resource as string)
              );
            },
          });
        },
        redirect: (to) => {
          navigate(resolveRedirect(to));
        },
      }),
    [notificationProvider, navigate, runtime.queryClient],
  );
  return <DispatcherProvider dispatcher={dispatcher}>{children}</DispatcherProvider>;
}

/** No context slices: one fixed role, no scope axis. */
function buildAdminSlices() {
  return defineContextSlices(() => ({}));
}
type AdminSlices = ReturnType<typeof buildAdminSlices>;

async function createAdminDataRuntime(
  app: AppRuntime,
): Promise<{ dataRuntime: DataRuntimeLease; dataSession: DataSessionController }> {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const dataSession = createDataSession({ scopeKey: {} });
  const participant = createDataRuntimeTransitionParticipant({ app, queryClient });
  dataSession.registerParticipant(participant);
  const dataRuntime = await createDataRuntime({
    app,
    queryClient,
    dataSession,
    transitionParticipant: participant,
  });
  return { dataRuntime, dataSession };
}

function RefinestRoot({ children }: { children: ReactNode }): React.ReactElement | null {
  const auth = useAuth();
  const { stand } = useStand();
  const rootNavigate = useNavigate();
  const rootPathname = useLocation().pathname;
  const [resolver, setResolver] = useState<Resolver<AdminSlices> | null>(null);
  const [i18n, setI18n] = useState<I18nFacade | null>(null);
  const [dataRuntime, setDataRuntime] = useState<DataRuntimeLease | null>(null);
  // The app is built once; auth is read through a ref so a sign-in or a
  // dropped session never re-creates the registry underneath the runtime.
  const authRef = useRef(auth);
  authRef.current = auth;

  // Rebuilt per stand: the registry, the mappings and the endpoint all belong
  // to it. `<RefinestRoot>` is keyed by stand id so the runtimes below are
  // rebuilt with it rather than re-pointed underneath their caches.
  const app = useMemo(
    () =>
      createAdminApp(stand, {
        defaultProviderId: 'default',
        dataProviderFactories: {
          default: () =>
            createDonatDataProvider({
              endpoint: stand.graphqlUrl,
              authorize: () => authRef.current.authorize(stand.role),
              recover: () => authRef.current.recover(),
              resources: standMappings(stand),
            }),
        },
      }),
    [stand],
  );

  useEffect(() => {
    let cancelled = false;
    (async () => {
      await app.ready;
      const r = createResolver(buildAdminSlices());
      await r.boot();
      const adapter = await createI18nextAdapter({
        defaultLocale: 'en',
        locales: ['en'],
        resources: { en: { common: { ...frameworkEn } } },
      });
      const facade = createI18n({ adapter, warnOnMissing: import.meta.env.DEV });
      if (!cancelled) {
        setResolver(r);
        setI18n(facade);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [app]);

  useEffect(() => {
    let cancelled = false;
    let owned: DataRuntimeLease | undefined;
    void createAdminDataRuntime(app).then(({ dataRuntime: lease }) => {
      owned = lease;
      if (!cancelled) setDataRuntime(lease);
      else void lease.dispose();
    });
    return () => {
      cancelled = true;
      setDataRuntime(null);
      if (owned) void owned.dispose();
    };
  }, [app]);

  const notificationProvider = useMemo(() => createNotificationProvider(), []);

  if (!resolver || !i18n || !dataRuntime) return null;

  return (
    <RefinestProvider
      app={app}
      dataRuntime={dataRuntime}
      resolver={resolver}
      i18n={i18n}
      fieldImplementations={shadcnFieldImplementations}
      components={{}}
    >
      <LinkComponentProvider
        linkComponent={RouterLinkAdapter}
        navigate={(href) => rootNavigate(href)}
        currentPath={rootPathname}
      >
        <RelationAggregatorProvider>
          <NativeDispatcher notificationProvider={notificationProvider}>
            {children}
          </NativeDispatcher>
        </RelationAggregatorProvider>
      </LinkComponentProvider>
    </RefinestProvider>
  );
}

function StandScopedRoot({ children }: { children: ReactNode }): React.ReactElement {
  const { stand } = useStand();
  // The key is the whole point: switching stands switches backends, so the
  // registry, the query cache and every in-flight request go with it.
  return <RefinestRoot key={stand.id}>{children}</RefinestRoot>;
}

export function Providers({
  children,
  stands,
}: {
  children: ReactNode;
  /** Overrides the configured stands; the worked example and the tests use it. */
  stands?: Stand[];
}): React.ReactElement {
  return (
    <ReactRouterAdapter>
      <AuthProvider>
        <StandProvider stands={stands}>
          <StandScopedRoot>
            {children}
            <Toaster position="bottom-right" />
          </StandScopedRoot>
        </StandProvider>
      </AuthProvider>
    </ReactRouterAdapter>
  );
}
