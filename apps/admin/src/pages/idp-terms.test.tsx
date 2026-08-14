import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';

import { declinable, IdpTerms } from './idp-terms';
import type { Terms } from '../idp/types';

const terms = (over: Partial<Terms> = {}): Terms => ({
  content: '# Terms\n\nBe reasonable.',
  is_html: false,
  ts: 1_700_000_000,
  ...over,
});

describe('declinable', () => {
  const now = 1_800_000_000_000;

  it('is false when a deployment gave no deadline: these must be accepted', () => {
    expect(declinable(terms(), now)).toBe(false);
  });

  it('is true while the deadline is ahead', () => {
    expect(declinable(terms({ opt_until: now / 1000 + 3600 }), now)).toBe(true);
  });

  it('is false once it has passed', () => {
    expect(declinable(terms({ opt_until: now / 1000 - 1 }), now)).toBe(false);
  });

  it('is false inside the last seconds, rather than offering a button that fails', () => {
    expect(declinable(terms({ opt_until: now / 1000 + 1 }), now)).toBe(false);
  });
});

describe('IdpTerms', () => {
  const render_ = (over: Partial<Terms> = {}) => {
    const onAccept = vi.fn();
    const onDecline = vi.fn();
    render(<IdpTerms terms={terms(over)} onAccept={onAccept} onDecline={onDecline} />);
    return { onAccept, onDecline };
  };

  it('renders Markdown as this panel, not as markup', () => {
    render_({ content: '## Heading\n\n- one\n- two' });

    const body = screen.getByTestId('idp-terms-body');
    expect(body.querySelector('ul')?.querySelectorAll('li')).toHaveLength(2);
    // No frame: nothing here was ever a document.
    expect(screen.queryByTestId('idp-terms-frame')).toBeNull();
  });

  it('puts a deployment\'s HTML in a frame that can do nothing', () => {
    render_({ is_html: true, content: '<p>Terms</p><script>alert(1)</script>' });

    const frame = screen.getByTestId('idp-terms-frame');
    // Empty sandbox: no scripts, no forms, no same-origin. The provider's own
    // page injects this straight into itself.
    expect(frame.getAttribute('sandbox')).toBe('');
    expect(frame.getAttribute('srcdoc')).toContain('<script>');
    expect(document.querySelector('script')).toBeNull();
  });

  it('offers only accepting when the terms are not optional', () => {
    const { onAccept } = render_();

    expect(screen.queryByTestId('idp-terms-decline')).toBeNull();
    fireEvent.click(screen.getByTestId('idp-terms-accept'));
    expect(onAccept).toHaveBeenCalled();
  });

  it('offers declining while a deadline is still ahead', () => {
    const { onDecline } = render_({ opt_until: Date.now() / 1000 + 3600 });

    fireEvent.click(screen.getByTestId('idp-terms-decline'));
    expect(onDecline).toHaveBeenCalled();
  });
});
