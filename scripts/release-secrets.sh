#!/usr/bin/env bash
# Sets the GitHub Actions secrets the release workflow needs, from 1Password,
# in one go. Two items feed it:
#   - the Apple item, shared by every app signed under the same team. The
#     Developer ID certificate is attached as Certificates.p12 and becomes
#     APPLE_CERTIFICATE (base64); its fields hold APPLE_CERTIFICATE_PASSWORD,
#     APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID.
#   - this app's own item, with its update-signing key pair.
# Every field of either item whose label is an UPPER_CASE name becomes a
# secret of that name, so another app can copy this script and point
# PROJECT_SECRETS_ITEM at its own item (or set it empty to skip). See
# RELEASING.md.
#
# Needs the op, gh and jq CLIs, and 1Password's CLI integration turned on
# (1Password > Settings > Developer). Expect one approval per item plus one
# for the certificate file.
set -euo pipefail

VAULT="${SECRETS_VAULT:-Private}"
APPLE_ITEM="${APPLE_SECRETS_ITEM:-Apple developer secrets}"
PROJECT_ITEM="${PROJECT_SECRETS_ITEM-Tab Guard update signing}"

for tool in op gh jq; do
  command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done
cd "$(dirname "$0")/.."

# Sets a secret for every UPPER_CASE text or concealed field of the item.
set_fields() {
  local item="$1" count=0 name value
  while IFS= read -r -d '' name && IFS= read -r -d '' value; do
    printf %s "$value" | gh secret set "$name"
    count=$((count + 1))
  done < <(op item get "$item" --vault "$VAULT" --format json --reveal | jq -j '
    .fields[]
    | select(.purpose != "NOTES" and (.type == "STRING" or .type == "CONCEALED"))
    | select((.label | test("^[A-Z][A-Z0-9_]*$")) and .value != null)
    | "\(.label)\u0000\(.value)\u0000"')
  [ "$count" -gt 0 ] || { echo "\"$item\" has no UPPER_CASE fields to set" >&2; exit 1; }
}

echo "Reading \"$APPLE_ITEM\" from the $VAULT vault. 1Password may ask you to approve."
set_fields "$APPLE_ITEM"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
op read --force --out-file "$tmp/Certificates.p12" "op://$VAULT/$APPLE_ITEM/Certificates.p12" >/dev/null
base64 -i "$tmp/Certificates.p12" | tr -d '\n' | gh secret set APPLE_CERTIFICATE

if [ -n "$PROJECT_ITEM" ]; then
  echo "Reading \"$PROJECT_ITEM\"."
  set_fields "$PROJECT_ITEM"
fi

echo
gh secret list
