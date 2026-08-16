/**
 * `toBeDisabled`, `toHaveAttribute` and the rest of the matchers that talk
 * about the DOM rather than about objects.
 */
import '@testing-library/jest-dom/vitest';

/**
 * jsdom gaps the shadcn layout primitives depend on. All three exist in every
 * browser and none in jsdom, so a component that reads them throws on mount
 * rather than failing an assertion — stubbing them here keeps a render test
 * about the panel instead of about jsdom.
 */
if (typeof window !== 'undefined') {
  if (!window.matchMedia) {
    window.matchMedia = (query: string) =>
      ({
        matches: false,
        media: query,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }) as MediaQueryList;
  }

  if (!('ResizeObserver' in globalThis)) {
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
  }

  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
}
