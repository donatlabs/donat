import { DONAT_ROLE, GRAPHQL_URL } from '../env';
import { standsFromEnv } from './config';
import type { Stand } from './types';

export type { Stand, StandResource, StandUsers } from './types';
export { usersResource } from './types';
export { standFromConfig, standsFromEnv, type StandConfig } from './config';

/**
 * The stands this panel serves — one panel, several deployments.
 *
 * A stand is a deployment seen through one role, and both are configuration:
 * `VITE_DONAT_STANDS` (a JSON array) or, when it is absent, a single stand at
 * `VITE_DONAT_GRAPHQL_URL` as `VITE_DONAT_ROLE`. The role name is whatever the
 * deployment calls its operator — `admin`, `support`, `operator` — and naming
 * it here grants nothing: this engine has no admin role, so the deployment's
 * own metadata is what gives that role its permissions.
 */
export function loadStands(): Stand[] {
  return standsFromEnv(import.meta.env.VITE_DONAT_STANDS, {
    graphqlUrl: GRAPHQL_URL,
    role: DONAT_ROLE,
  });
}

export const STANDS: Stand[] = loadStands();

const SELECTED_KEY = 'donat-admin-stand';

/** The stand to show, honouring what the operator last picked. */
export function resolveStand(stands: Stand[] = STANDS, stored?: string | null): Stand {
  const wanted =
    stored ?? (typeof window === 'undefined' ? null : window.localStorage.getItem(SELECTED_KEY));
  return stands.find((stand) => stand.id === wanted) ?? stands[0];
}

export function rememberStand(id: string): void {
  if (typeof window !== 'undefined') {
    window.localStorage.setItem(SELECTED_KEY, id);
  }
}

/** The mappings the data provider needs for one stand's resources. */
export function standMappings(stand: Stand) {
  return Object.fromEntries(stand.resources.map((entry) => [entry.name, entry.mapping]));
}
