import type { ReactElement } from 'react';
import { useMemo } from 'react';
import { BrowserRouter, Navigate, Outlet, Route, Routes } from 'react-router';
import { generateRouteDescriptors, useApp, type RouteDescriptor } from '@refinest/react';
import { Providers } from './components/providers';
import AppShell from './layout/app-shell';
import { AccountScreen } from './pages/account';
import IdpAuthorizePage from './pages/idp-authorize';
import IdpResetPage from './pages/idp-reset';
import LoginPage from './pages/login';
import { ResourceCreateBody } from './pages/resources/create';
import { ResourceEditBody } from './pages/resources/edit';
import { ResourceListBody } from './pages/resources/list';
import { ResourceShowBody } from './pages/resources/show';

/**
 * Routing. There is one hand-written route — the login page — and everything
 * else is generated from the resource registry: `generateRouteDescriptors`
 * yields a `list` / `show` / `edit` / `create` descriptor per registered
 * resource, and `pages/resources/` supplies the four connected bodies. Adding
 * a resource to `refinest-app.ts` adds its routes and its sidebar entry; there
 * is nothing to register here.
 */

// `Extract<RouteDescriptor, { action: 'list' | 'edit' }>` would resolve to
// `never`: `ResourceRouteDescriptor.action` is `'list' | 'show' | 'edit' |
// 'create'`, which is not assignable to `'list' | 'edit'`, so Extract's
// per-member assignability check rejects the whole variant. Narrow on the
// `resource` field instead (absent on `CustomPageRouteDescriptor`, which
// carries `page`), then filter by action at runtime.
type ResourceRoute = Extract<RouteDescriptor, { resource: string }>;

function isResourceRoute(descriptor: RouteDescriptor): descriptor is ResourceRoute {
  return 'resource' in descriptor;
}

/** Auth-guarded shell wrapper — `<AppShell>` owns the redirect-to-login check. */
function AdminFrame(): ReactElement {
  return (
    <AppShell>
      <Outlet />
    </AppShell>
  );
}

function NotFoundView(): ReactElement {
  return <div className="p-6 text-muted-foreground">Not found.</div>;
}

export function AppRoutes(): ReactElement {
  const app = useApp();
  const descriptors = useMemo(() => generateRouteDescriptors(app), [app]);
  const resourceRoutes = useMemo(
    () =>
      descriptors
        .filter(isResourceRoute)
        .filter(
          (d) =>
            d.action === 'list' ||
            d.action === 'show' ||
            d.action === 'edit' ||
            d.action === 'create',
        ),
    [descriptors],
  );
  const firstList = resourceRoutes.find((d) => d.action === 'list');

  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      {/*
        Where the engine's authorization redirect lands when a deployment
        points `DONAT_OIDC.authorization_endpoint` here. Outside the guarded
        shell, like `/login`: nobody is signed in yet.
      */}
      <Route path="/idp/authorize" element={<IdpAuthorizePage />} />
      {/*
        The account, replacing the provider's own page. Outside the guarded
        shell on purpose: the provider sends people here mid-login — when an
        application wants a second factor, or the account has to be updated
        before it may sign in — and at that moment there is a provider session
        but no engine one. The screen needs only the first, and answers "that
        session is no longer signed in" when there is not even that.
      */}
      <Route path="/account" element={<AccountScreen />} />
      {/*
        Where the reset link in an email lands, once the engine has turned that
        link into this route. Outside the shell for the plainest of reasons:
        somebody who needs this cannot sign in.
      */}
      <Route path="/idp/reset/:userId/:resetId" element={<IdpResetPage />} />
      <Route element={<AdminFrame />}>
        <Route
          index
          element={
            firstList ? (
              <Navigate to={`/${firstList.resource}`} replace />
            ) : (
              <div className="p-6 text-muted-foreground">No resources yet.</div>
            )
          }
        />
        {resourceRoutes.map((descriptor) => (
          <Route
            key={`${descriptor.resource}:${descriptor.action}`}
            path={descriptor.path}
            element={
              descriptor.action === 'list' ? (
                <ResourceListBody resource={descriptor.resource} />
              ) : descriptor.action === 'show' ? (
                <ResourceShowBody resource={descriptor.resource} />
              ) : descriptor.action === 'create' ? (
                <ResourceCreateBody resource={descriptor.resource} />
              ) : (
                <ResourceEditBody resource={descriptor.resource} />
              )
            }
          />
        ))}
        <Route path="*" element={<NotFoundView />} />
      </Route>
    </Routes>
  );
}

export function App(): ReactElement {
  return (
    <BrowserRouter>
      <Providers>
        <AppRoutes />
      </Providers>
    </BrowserRouter>
  );
}
