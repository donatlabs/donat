import { describe, expect, it } from 'vitest';

import {
  assertion,
  attestation,
  decodeCreation,
  decodeRequest,
  fromBase64Url,
  toBase64Url,
  WebauthnError,
} from './webauthn';

const bytes = (...values: number[]) => new Uint8Array(values).buffer;

/**
 * These are the tests the type system cannot be: every value below is an
 * `ArrayBuffer` or a `string` either way, so a mistake here is not a compile
 * error but a signature over the wrong bytes, refused by the provider as a
 * bad key.
 */
describe('base64url', () => {
  it('decodes the alphabet the provider sends, not the standard one', () => {
    // 0xfb 0xff encodes as "+/8" in standard base64 and "-_8" in base64url.
    expect(new Uint8Array(fromBase64Url('-_8'))).toEqual(new Uint8Array([0xfb, 0xff]));
  });

  it('accepts the dot the provider uses for padding', () => {
    expect(new Uint8Array(fromBase64Url('AQ..'))).toEqual(new Uint8Array([0x01]));
  });

  it('encodes without padding, which is what the provider reads', () => {
    expect(toBase64Url(bytes(0xfb, 0xff))).toBe('-_8');
    expect(toBase64Url(bytes(0x01))).toBe('AQ');
  });

  it('round-trips every byte value', () => {
    const all = new Uint8Array(256).map((_, index) => index);
    expect(new Uint8Array(fromBase64Url(toBase64Url(all.buffer)))).toEqual(all);
  });
});

describe('decodeRequest', () => {
  it('decodes the challenge and every allowed credential', () => {
    const options = decodeRequest({
      publicKey: {
        challenge: '-_8',
        allowCredentials: [{ id: 'AQ', type: 'public-key' }],
      },
    });
    const key = options.publicKey!;
    expect(new Uint8Array(key.challenge as ArrayBuffer)).toEqual(new Uint8Array([0xfb, 0xff]));
    expect(new Uint8Array(key.allowCredentials![0].id as ArrayBuffer)).toEqual(
      new Uint8Array([0x01]),
    );
  });

  it('keeps whatever else the provider put there', () => {
    const options = decodeRequest({
      publicKey: { challenge: 'AQ', rpId: 'example.test', userVerification: 'required' },
    });
    expect(options.publicKey).toMatchObject({
      rpId: 'example.test',
      userVerification: 'required',
    });
  });

  it('refuses a challenge that is not one', () => {
    expect(() => decodeRequest({})).toThrow(WebauthnError);
  });
});

describe('decodeCreation', () => {
  it('decodes the account id as well as the challenge', () => {
    const options = decodeCreation({
      publicKey: {
        challenge: 'AQ',
        user: { id: '-_8', name: 'a@b.test', displayName: 'A' },
        excludeCredentials: [{ id: 'AQ', type: 'public-key' }],
      },
    });
    const key = options.publicKey!;
    expect(new Uint8Array(key.user.id as ArrayBuffer)).toEqual(new Uint8Array([0xfb, 0xff]));
    expect(new Uint8Array(key.excludeCredentials![0].id as ArrayBuffer)).toEqual(
      new Uint8Array([0x01]),
    );
  });
});

const credential = (response: Record<string, ArrayBuffer>) =>
  ({
    id: 'the-key',
    rawId: bytes(0x01),
    type: 'public-key',
    response,
    getClientExtensionResults: () => ({ credProps: { rk: true } }),
  }) as unknown as PublicKeyCredential;

describe('assertion', () => {
  it('encodes exactly the three buffers the provider verifies', () => {
    expect(
      assertion(
        credential({
          authenticatorData: bytes(0xfb),
          clientDataJSON: bytes(0xff),
          signature: bytes(0x01),
        }),
      ),
    ).toEqual({
      id: 'the-key',
      rawId: 'AQ',
      response: { authenticatorData: '-w', clientDataJSON: '_w', signature: 'AQ' },
      extensions: { credProps: { rk: true } },
      type: 'public-key',
    });
  });
});

describe('attestation', () => {
  it('sends the attestation object rather than a signature', () => {
    expect(
      attestation(credential({ attestationObject: bytes(0xfb), clientDataJSON: bytes(0xff) })),
    ).toEqual({
      id: 'the-key',
      rawId: 'AQ',
      response: { attestationObject: '-w', clientDataJSON: '_w' },
      extensions: { credProps: { rk: true } },
      type: 'public-key',
    });
  });
});
