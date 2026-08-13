import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
import { rememberStand, resolveStand, STANDS, type Stand } from './index';

/**
 * Which deployment the panel is looking at.
 *
 * Held above the app runtime because everything below it belongs to one stand:
 * the resource registry, the data provider's endpoint, the role, the query
 * cache. Changing it remounts that whole tree rather than re-pointing it.
 */
export interface StandState {
  readonly stand: Stand;
  readonly stands: Stand[];
  select: (id: string) => void;
}

const StandContext = createContext<StandState | null>(null);

export function StandProvider({
  children,
  stands = STANDS,
}: {
  children: ReactNode;
  stands?: Stand[];
}): React.ReactElement {
  const [selected, setSelected] = useState<Stand>(() => resolveStand(stands));

  const select = useCallback(
    (id: string) => {
      const next = stands.find((stand) => stand.id === id);
      if (!next || next.id === selected.id) return;
      rememberStand(next.id);
      setSelected(next);
    },
    [stands, selected.id],
  );

  const value = useMemo<StandState>(
    () => ({ stand: selected, stands, select }),
    [selected, stands, select],
  );
  return <StandContext.Provider value={value}>{children}</StandContext.Provider>;
}

export function useStand(): StandState {
  const ctx = useContext(StandContext);
  if (!ctx) throw new Error('useStand: wrap with <StandProvider>');
  return ctx;
}
