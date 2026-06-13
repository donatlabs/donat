# JWT test keys

Keypairs used by JWT-mode conformance suites (mirrors tests-py
`fixtures/jwt.py`, which generates a fresh pair per run):

```sh
openssl genrsa -out rsa_private.pem 2048
openssl rsa -pubout -in rsa_private.pem -out rsa_public.pem

openssl genpkey -algorithm ed25519 -out ed25519_private.pem
openssl pkey -pubout -in ed25519_private.pem -out ed25519_public.pem

openssl ecparam -name prime256v1 -genkey -noout -out es256_sec1.pem
openssl pkcs8 -topk8 -nocrypt -in es256_sec1.pem -out es256_private.pem
rm es256_sec1.pem  # jsonwebtoken needs PKCS#8, not SEC1
openssl ec -in es256_private.pem -pubout -out es256_public.pem
```

The engine is configured with
`DONAT_GRAPHQL_JWT_SECRET={"type":"RS512"|"Ed25519"|"ES256","key":"<*_public.pem>",...}`.
Test-only keys, committed on purpose — never use them anywhere else.

`rsa_jwk.json` is the JWK form of `rsa_public.pem` (kid `test-key-1`),
served by the JWKS stub in `tests/jwk.rs` (jwk_url mode,
`DONAT_GRAPHQL_JWT_SECRET={"jwk_url": ...}`). Precomputed via:

```sh
openssl rsa -pubin -in rsa_public.pem -noout -modulus  # hex -> base64url = n
# e = base64url(0x010001) = AQAB
```
