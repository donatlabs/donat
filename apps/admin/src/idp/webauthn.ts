/**
 * The WebAuthn ceremony, as the provider expects it on the wire.
 *
 * This is the one part of a passkey that cannot be proxied or reimplemented:
 * only the browser can talk to an authenticator, and only for the origin it is
 * on. Everything around it — the challenge, the verification, the decision —
 * stays with the provider. What lives here is the translation between its JSON
 * and the browser's `PublicKeyCredential`, and nothing else.
 *
 * The translation is entirely about **base64url**. The provider sends the
 * challenge and every credential id as base64url text and expects the signed
 * result back the same way, while `navigator.credentials` deals only in
 * `ArrayBuffer`. Getting this wrong does not produce a type error — it
 * produces a signature over the wrong bytes, which the provider rejects as an
 * invalid key. So both directions are tested.
 */

/** Decode base64url text — the challenge, and every credential id. */
export function fromBase64Url(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, '+').replace(/_/g, '/').replace(/\./g, '=');
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes.buffer;
}

/** Encode what the authenticator produced, the way the provider reads it. */
export function toBase64Url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
}

/**
 * What `POST /users/webauthn_start` answers with: the browser's own request
 * options, except that every buffer in them is still base64url text.
 */
export interface CredentialRequest {
  publicKey?: {
    challenge: string | ArrayBuffer;
    allowCredentials?: { id: string | ArrayBuffer; type: string; transports?: string[] }[];
    timeout?: number;
    rpId?: string;
    userVerification?: string;
  };
}

/** The same, for enrolment: `POST /users/{id}/webauthn/register/start`. */
export interface CredentialCreation {
  publicKey?: {
    challenge: string | ArrayBuffer;
    user: { id: string | ArrayBuffer; name: string; displayName: string };
    excludeCredentials?: { id: string | ArrayBuffer; type: string }[];
    timeout?: number;
    [key: string]: unknown;
  };
}

/** Why the ceremony is happening. Signing in carries the code from HTTP 200. */
export type WebauthnPurpose = { Login: string };

export class WebauthnError extends Error {
  constructor(
    message: string,
    /** True when the provider's window closed rather than the key being wrong. */
    readonly expired = false,
  ) {
    super(message);
    this.name = 'WebauthnError';
  }
}

/** True when this browser can do any of this at all. */
export function passkeysAvailable(): boolean {
  return typeof navigator !== 'undefined' && !!navigator.credentials?.get;
}

function decoded(value: string | ArrayBuffer): ArrayBuffer {
  return typeof value === 'string' ? fromBase64Url(value) : value;
}

/**
 * Decode a sign-in challenge in place, the way the provider's own page does.
 *
 * In place, and returning it, because the object is otherwise exactly the
 * argument `navigator.credentials.get` wants — rewriting it field by field
 * would silently drop whatever the provider adds next.
 */
export function decodeRequest(request: CredentialRequest): CredentialRequestOptions {
  if (!request.publicKey) throw new WebauthnError('the provider sent no challenge');
  request.publicKey.challenge = decoded(request.publicKey.challenge);
  for (const credential of request.publicKey.allowCredentials ?? []) {
    credential.id = decoded(credential.id);
  }
  return request as unknown as CredentialRequestOptions;
}

/** The same for an enrolment challenge, which also carries the account id. */
export function decodeCreation(creation: CredentialCreation): CredentialCreationOptions {
  if (!creation.publicKey) throw new WebauthnError('the provider sent no challenge');
  creation.publicKey.challenge = decoded(creation.publicKey.challenge);
  creation.publicKey.user.id = decoded(creation.publicKey.user.id);
  for (const credential of creation.publicKey.excludeCredentials ?? []) {
    credential.id = decoded(credential.id);
  }
  return creation as unknown as CredentialCreationOptions;
}

/** A signed assertion, in the shape `POST /users/webauthn_finish` reads. */
export function assertion(credential: PublicKeyCredential): Record<string, unknown> {
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    id: credential.id,
    rawId: toBase64Url(credential.rawId),
    response: {
      authenticatorData: toBase64Url(response.authenticatorData),
      clientDataJSON: toBase64Url(response.clientDataJSON),
      signature: toBase64Url(response.signature),
    },
    extensions: credential.getClientExtensionResults(),
    type: credential.type,
  };
}

/** A new credential, in the shape `.../webauthn/register/finish` reads. */
export function attestation(credential: PublicKeyCredential): Record<string, unknown> {
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    id: credential.id,
    rawId: toBase64Url(credential.rawId),
    response: {
      attestationObject: toBase64Url(response.attestationObject),
      clientDataJSON: toBase64Url(response.clientDataJSON),
    },
    extensions: credential.getClientExtensionResults(),
    type: credential.type,
  };
}

/**
 * Run a ceremony, and say which of the two failures it was.
 *
 * The browser reports a refused key and a challenge that timed out the same
 * way — an `AbortError`, or a null result — so the clock is what separates
 * them. It matters: "that key is not for this account" is the person's
 * problem, and "this took too long, press it again" is not.
 */
async function ceremony(
  run: Promise<Credential | null>,
  timeout: number,
): Promise<PublicKeyCredential> {
  const deadline = Date.now() + timeout;
  try {
    const credential = await run;
    if (!credential) throw new WebauthnError('that key was refused');
    return credential as PublicKeyCredential;
  } catch (cause) {
    if (cause instanceof WebauthnError) throw cause;
    throw new WebauthnError(
      Date.now() >= deadline ? 'this took too long — try again' : 'that key was refused',
      Date.now() >= deadline,
    );
  }
}

/** Sign in with a passkey: the browser half of it. */
export async function signWithPasskey(
  request: CredentialRequest,
  expiresInSeconds: number,
): Promise<Record<string, unknown>> {
  const options = decodeRequest(request);
  const timeout = Math.max(1, expiresInSeconds - 1) * 1000;
  return assertion(await ceremony(navigator.credentials.get(options), timeout));
}

/** Enrol a passkey: the browser half of it. */
export async function createPasskey(
  creation: CredentialCreation,
): Promise<Record<string, unknown>> {
  const options = decodeCreation(creation);
  const timeout = (creation.publicKey?.timeout ?? 60_000) - 1000;
  return attestation(await ceremony(navigator.credentials.create(options), timeout));
}
