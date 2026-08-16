import { describe, expect, it } from 'vitest';

import { inlines, markdown } from './markdown';

describe('inlines', () => {
  it('reads the four runs terms actually use', () => {
    expect(inlines('a **b** and _c_ and `d`')).toEqual([
      { kind: 'text', text: 'a ' },
      { kind: 'strong', text: 'b' },
      { kind: 'text', text: ' and ' },
      { kind: 'emphasis', text: 'c' },
      { kind: 'text', text: ' and ' },
      { kind: 'code', text: 'd' },
    ]);
  });

  it('keeps a link the author can be trusted with', () => {
    expect(inlines('see [the policy](https://example.test/policy)')).toEqual([
      { kind: 'text', text: 'see ' },
      { kind: 'link', text: 'the policy', href: 'https://example.test/policy' },
    ]);
  });

  it.each(['javascript:alert(1)', 'data:text/html,<script>', 'vbscript:x'])(
    'refuses to make %s a link, and shows it instead',
    (href) => {
      expect(inlines(`[click](${href})`)).toEqual([{ kind: 'text', text: `click (${href})` }]);
    },
  );

  it('leaves unmatched syntax as the characters they are', () => {
    expect(inlines('2 * 3 * 4')).toEqual([{ kind: 'text', text: '2 * 3 * 4' }]);
  });
});

describe('markdown', () => {
  it('folds wrapped lines into one paragraph and breaks on a blank line', () => {
    expect(markdown('one\ntwo\n\nthree')).toEqual([
      { kind: 'paragraph', content: [{ kind: 'text', text: 'one two' }] },
      { kind: 'paragraph', content: [{ kind: 'text', text: 'three' }] },
    ]);
  });

  it('reads headings up to three deep', () => {
    expect(markdown('# One\n### Three')).toEqual([
      { kind: 'heading', level: 1, content: [{ kind: 'text', text: 'One' }] },
      { kind: 'heading', level: 3, content: [{ kind: 'text', text: 'Three' }] },
    ]);
  });

  it('gathers a bullet list, and starts a new one when the marker changes', () => {
    expect(markdown('- a\n- b\n1. c')).toEqual([
      {
        kind: 'list',
        ordered: false,
        items: [[{ kind: 'text', text: 'a' }], [{ kind: 'text', text: 'b' }]],
      },
      { kind: 'list', ordered: true, items: [[{ kind: 'text', text: 'c' }]] },
    ]);
  });

  it('ends a list when prose follows it', () => {
    expect(markdown('- a\nprose')).toEqual([
      { kind: 'list', ordered: false, items: [[{ kind: 'text', text: 'a' }]] },
      { kind: 'paragraph', content: [{ kind: 'text', text: 'prose' }] },
    ]);
  });

  it('is total: any text is some text', () => {
    for (const source of ['', '   ', '#', '- ', '****', '[](', '\r\n\r\n']) {
      expect(() => markdown(source)).not.toThrow();
    }
  });
});
