/**
 * New terms, on the way in.
 *
 * The provider answers HTTP 206 to a login when the credentials were right and
 * the account has not accepted the terms in force. This is that screen, and
 * the login resumes from it: accepting (or declining, while that is still
 * allowed) returns the same outcome the password step would have.
 *
 * The text is a deployment's own, in Markdown or HTML, and the two are
 * rendered differently on purpose. Markdown becomes this panel's components —
 * so terms read like the rest of the panel, and no provider text ever becomes
 * markup. HTML is somebody's document and stays one: it goes into a sandboxed
 * frame with no scripts and no access to this origin. The provider's own page
 * injects both directly into itself; this is stricter than what it does.
 */
import { useMemo, type ReactElement } from 'react';
import {
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@refinest/ui-shadcn';

import { markdown, type Block, type Inline } from '../idp/markdown';
import type { Terms } from '../idp/types';

export interface IdpTermsProps {
  terms: Terms;
  /** True while the answer is in flight. */
  busy?: boolean;
  onAccept: () => void;
  onDecline: () => void;
}

function renderInline(run: Inline, key: number): ReactElement | string {
  switch (run.kind) {
    case 'strong':
      return <strong key={key}>{run.text}</strong>;
    case 'emphasis':
      return <em key={key}>{run.text}</em>;
    case 'code':
      return (
        <code key={key} className="bg-muted rounded px-1 py-0.5 text-[0.9em]">
          {run.text}
        </code>
      );
    case 'link':
      return (
        <a
          key={key}
          href={run.href}
          target="_blank"
          rel="noreferrer noopener"
          className="underline underline-offset-4"
        >
          {run.text}
        </a>
      );
    default:
      return run.text;
  }
}

function renderBlock(block: Block, key: number): ReactElement {
  const content = block.kind === 'list' ? null : block.content.map(renderInline);
  switch (block.kind) {
    case 'heading': {
      const size = ['text-lg font-semibold', 'text-base font-semibold', 'text-sm font-semibold'][
        block.level - 1
      ];
      return (
        <p key={key} className={`${size} mt-4 first:mt-0`}>
          {content}
        </p>
      );
    }
    case 'list': {
      const items = block.items.map((item, index) => (
        <li key={index}>{item.map(renderInline)}</li>
      ));
      return block.ordered ? (
        <ol key={key} className="ml-5 list-decimal space-y-1">
          {items}
        </ol>
      ) : (
        <ul key={key} className="ml-5 list-disc space-y-1">
          {items}
        </ul>
      );
    }
    default:
      return (
        <p key={key} className="leading-relaxed">
          {content}
        </p>
      );
  }
}

/** True while declining is still one of the answers. */
export function declinable(terms: Terms, now = Date.now()): boolean {
  // Three seconds of slack, as the provider's own page allows: a deadline that
  // passed while the page was rendering should not offer a button that fails.
  return terms.opt_until !== undefined && terms.opt_until > now / 1000 + 3;
}

export function IdpTerms({ terms, busy, onAccept, onDecline }: IdpTermsProps): ReactElement {
  const blocks = useMemo(
    () => (terms.is_html ? [] : markdown(terms.content)),
    [terms.content, terms.is_html],
  );
  const mayDecline = declinable(terms);

  return (
    <Card className="mx-auto w-full max-w-2xl" data-testid="idp-terms">
      <CardHeader>
        <CardTitle>Before you continue</CardTitle>
        <CardDescription className="text-balance">
          {mayDecline
            ? 'These terms are new. You can accept them now or carry on without.'
            : 'These terms have to be accepted before signing in.'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div
          className="max-h-[50vh] space-y-3 overflow-y-auto rounded-md border p-4 text-sm"
          data-testid="idp-terms-body"
        >
          {terms.is_html ? (
            <iframe
              // Not our markup, and never allowed to become ours: no scripts,
              // no forms, no same-origin — a document, shown.
              sandbox=""
              srcDoc={terms.content}
              title="Terms"
              className="h-[45vh] w-full border-0"
              data-testid="idp-terms-frame"
            />
          ) : (
            blocks.map(renderBlock)
          )}
        </div>
      </CardContent>
      <CardFooter className="gap-2">
        <Button onClick={onAccept} disabled={busy} data-testid="idp-terms-accept">
          Accept and continue
        </Button>
        {mayDecline && (
          <Button
            variant="outline"
            onClick={onDecline}
            disabled={busy}
            data-testid="idp-terms-decline"
          >
            Not now
          </Button>
        )}
      </CardFooter>
    </Card>
  );
}
