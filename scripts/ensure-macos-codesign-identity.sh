#!/usr/bin/env bash
set -euo pipefail

IDENTITY="${CUA_CODESIGN_IDENTITY_NAME:-cua Local Developer}"
KEYCHAIN="${CUA_CODESIGN_KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}"
P12_PASSWORD="${CUA_CODESIGN_P12_PASSWORD:-cua-local-codesign}"

existing_identity_hash() {
  security find-identity -v -p codesigning "$KEYCHAIN" 2>/dev/null |
    awk -v name="$IDENTITY" 'index($0, "\"" name "\"") { print $2; exit }'
}

if HASH="$(existing_identity_hash)" && [[ -n "$HASH" ]]; then
  printf '%s\n' "$HASH"
  exit 0
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cua-codesign.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

cat > "$WORK_DIR/openssl.cnf" <<CONF
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = codesign

[ dn ]
CN = $IDENTITY

[ codesign ]
basicConstraints = critical,CA:true
keyUsage = critical,digitalSignature,keyCertSign
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always
CONF

openssl req \
  -new \
  -newkey rsa:2048 \
  -x509 \
  -days 3650 \
  -nodes \
  -keyout "$WORK_DIR/identity.key" \
  -out "$WORK_DIR/identity.crt" \
  -config "$WORK_DIR/openssl.cnf" \
  >/dev/null 2>&1

openssl pkcs12 \
  -export \
  -inkey "$WORK_DIR/identity.key" \
  -in "$WORK_DIR/identity.crt" \
  -out "$WORK_DIR/identity.p12" \
  -passout "pass:$P12_PASSWORD" \
  -legacy \
  >/dev/null 2>&1

security import "$WORK_DIR/identity.p12" \
  -k "$KEYCHAIN" \
  -P "$P12_PASSWORD" \
  -T /usr/bin/codesign \
  -T /usr/bin/security \
  >/dev/null

security add-trusted-cert \
  -r trustRoot \
  -p codeSign \
  -k "$KEYCHAIN" \
  "$WORK_DIR/identity.crt" \
  >/dev/null

HASH="$(existing_identity_hash)"
if [[ -z "$HASH" ]]; then
  printf 'failed to create usable code-signing identity: %s\n' "$IDENTITY" >&2
  exit 1
fi

printf '%s\n' "$HASH"
