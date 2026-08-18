/**
 * Enough Markdown to render a deployment's terms, and not one feature more.
 *
 * The provider stores its terms as either Markdown or HTML and says which.
 * Its own page injects both straight into the document; we do neither. HTML
 * goes into a sandboxed frame, where it cannot reach this origin, and Markdown
 * comes through here — parsed into blocks that the page renders as its own
 * components, so terms look like the rest of the panel instead of like a
 * document dropped into it.
 *
 * Parsing rather than converting to an HTML string is the whole point: there
 * is no stage at which provider text becomes markup, so there is nothing for a
 * sanitiser to get wrong. A link is the only place an author picks a URL, and
 * only `http:`, `https:` and `mailto:` survive.
 *
 * A dependency would cover more syntax. It would also end the panel's claim to
 * be a plain npm project with nothing to audit — the same trade the proof of
 * work already makes.
 */

export type Inline =
  | { kind: 'text'; text: string }
  | { kind: 'strong'; text: string }
  | { kind: 'emphasis'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'link'; text: string; href: string };

export type Block =
  | { kind: 'heading'; level: 1 | 2 | 3; content: Inline[] }
  | { kind: 'paragraph'; content: Inline[] }
  | { kind: 'list'; ordered: boolean; items: Inline[][] };

/** Only schemes that cannot execute. Anything else renders as plain text. */
function safeHref(href: string): string | undefined {
  const trimmed = href.trim();
  return /^(https?:|mailto:)/i.test(trimmed) || trimmed.startsWith('/') ? trimmed : undefined;
}

/**
 * Emphasis may not open or close on a space, which is what keeps arithmetic
 * arithmetic: `2 * 3 * 4` is a sentence about multiplication, not an italic 3.
 */
const RUN = String.raw`(\S(?:[^*_\n]*\S)?)`;
const INLINE = new RegExp(
  String.raw`\[([^\]]+)\]\(([^)\s]+)\)` +
    String.raw`|\*\*${RUN}\*\*|__${RUN}__|\*${RUN}\*|_${RUN}_` +
    '|`([^`]+)`',
);

/** Split one line into its runs. Unmatched syntax stays literal, as it reads. */
export function inlines(line: string): Inline[] {
  const out: Inline[] = [];
  const push = (run: Inline) => {
    const last = out[out.length - 1];
    // Text that was only ever split by syntax that turned out not to be
    // syntax has to read as one run again — a reader sees the sentence, not
    // the parser's second thoughts.
    if (run.kind === 'text' && last?.kind === 'text') last.text += run.text;
    else out.push(run);
  };
  let rest = line;
  for (;;) {
    const match = INLINE.exec(rest);
    if (!match || match.index === undefined) break;
    if (match.index > 0) push({ kind: 'text', text: rest.slice(0, match.index) });
    const [whole, linkText, href, strongStar, strongUnderscore, emStar, emUnderscore, code] = match;
    if (linkText !== undefined && href !== undefined) {
      const safe = safeHref(href);
      push(
        safe
          ? { kind: 'link', text: linkText, href: safe }
          : { kind: 'text', text: `${linkText} (${href})` },
      );
    } else if (strongStar ?? strongUnderscore) {
      push({ kind: 'strong', text: (strongStar ?? strongUnderscore)! });
    } else if (emStar ?? emUnderscore) {
      push({ kind: 'emphasis', text: (emStar ?? emUnderscore)! });
    } else if (code !== undefined) {
      push({ kind: 'code', text: code });
    }
    rest = rest.slice(match.index + whole.length);
  }
  if (rest) push({ kind: 'text', text: rest });
  return out;
}

const HEADING = /^(#{1,3})\s+(.*)$/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;

/** Parse a terms document into blocks. Never throws: any text is some text. */
export function markdown(source: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: Inline[][] } | undefined;

  const endParagraph = () => {
    if (paragraph.length) blocks.push({ kind: 'paragraph', content: inlines(paragraph.join(' ')) });
    paragraph = [];
  };
  const endList = () => {
    if (list) blocks.push({ kind: 'list', ...list });
    list = undefined;
  };

  for (const line of source.replace(/\r\n?/g, '\n').split('\n')) {
    if (!line.trim()) {
      endParagraph();
      endList();
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      endParagraph();
      endList();
      blocks.push({
        kind: 'heading',
        level: heading[1].length as 1 | 2 | 3,
        content: inlines(heading[2]),
      });
      continue;
    }

    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    if (bullet ?? numbered) {
      endParagraph();
      const ordered = !bullet;
      if (list && list.ordered !== ordered) endList();
      list ??= { ordered, items: [] };
      list.items.push(inlines((bullet ?? numbered)![1]));
      continue;
    }

    endList();
    paragraph.push(line.trim());
  }

  endParagraph();
  endList();
  return blocks;
}
