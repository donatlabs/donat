#!/bin/sh
# Mint an HS256 token for a local example stand.
#
# This engine has no admin role and honours no role header on its own: a role
# reaches it from a verified JWT or an authentication hook. The full example
# (`petshop/`) runs a real identity provider for that. The single-surface
# examples next to it do not — they exist to show one API, not a login — so
# they verify tokens with a shared development key, and this script signs one.
#
#   ./mint-token.sh <role> [user-id]
#
# The key must match DONAT_GRAPHQL_JWT_SECRET in that example's compose file.
# It is a development key checked into a repository: it is worth exactly
# nothing outside a stand on your own machine.
set -eu

ROLE="${1:?usage: mint-token.sh <role> [user-id]}"
USER_ID="${2-}"
KEY="${DONAT_EXAMPLE_JWT_KEY:-donat-example-dev-key-change-me-32bytes}"
LIFETIME="${DONAT_EXAMPLE_TOKEN_LIFETIME:-3600}"

# base64url without padding, the JWT encoding.
b64url() {
    openssl base64 -A | tr '+/' '-_' | tr -d '='
}

header='{"alg":"HS256","typ":"JWT"}'
claims="{\"sub\":\"$ROLE\",\"exp\":$(($(date +%s) + LIFETIME)),\"x-donat-default-role\":\"$ROLE\",\"x-donat-allowed-roles\":[\"$ROLE\"]"
if [ -n "$USER_ID" ]; then
    claims="$claims,\"x-donat-user-id\":\"$USER_ID\""
fi
claims="$claims}"

signing_input="$(printf '%s' "$header" | b64url).$(printf '%s' "$claims" | b64url)"
signature=$(printf '%s' "$signing_input" | openssl dgst -sha256 -hmac "$KEY" -binary | b64url)
printf '%s.%s\n' "$signing_input" "$signature"
