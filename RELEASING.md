# Releasing

Releases are built, signed with the Developer ID certificate, notarized, and published as
**drafts** by GitHub Actions (`.github/workflows/release.yml`). Installed copies only update
once a draft is published, so every release gets a quick smoke test first.

## Each release

1. Bump the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json`,
   and add a `## X.Y.Z` section to `CHANGELOG.md` (it becomes the release notes).
2. Commit, then tag and push:
   ```bash
   git tag vX.Y.Z && git push origin main vX.Y.Z
   ```
3. Wait for **Release Build (Draft)** (about 15 minutes), then download Install-Tab-Guard.dmg from the
   draft release and check: it opens without a Gatekeeper warning, the menu-bar icon appears, the
   window opens, and **Which site is which?** opens about:processes in Firefox and
   chrome://process-internals in Chrome.
4. Publish the draft. Installed copies pick it up within 6 hours (or on **Check for updates**).
   The disk image is the only file a person needs, and its name carries no version so the page's
   download link never changes. The other assets (`latest.json`, the `.app.tar.gz` and its `.sig`)
   are the updater's and keep the names tauri-action gives them.

## One-time setup

The workflow needs seven repository secrets (Settings → Secrets and variables → Actions). They come
from two 1Password items in the Private vault:

- **Apple developer secrets**: the five Apple values, shared by every app signed under the same
  team. The Developer ID certificate is attached as `Certificates.p12`.
- **Tab Guard update signing**: this app's update-signing key pair.

One command sets them all:

```bash
scripts/release-secrets.sh
```

It needs the `op`, `gh` and `jq` CLIs and 1Password's CLI integration turned on (1Password →
Settings → Developer). Approve the prompts and it sets every secret. Each `UPPER_CASE` field of
either item becomes a secret of that name, and the attached `.p12` becomes `APPLE_CERTIFICATE`, so
another app can copy the script and point `PROJECT_SECRETS_ITEM` at its own item.

| Secret | What | Item |
|---|---|---|
| `APPLE_CERTIFICATE` | Developer ID Application certificate + key, as a base64 `.p12` | Apple |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password | Apple |
| `APPLE_ID` | Apple ID email used for notarization | Apple |
| `APPLE_PASSWORD` | an app-specific password for that Apple ID | Apple |
| `APPLE_TEAM_ID` | `B6J5X2WZ98` | Apple |
| `TAURI_SIGNING_PRIVATE_KEY` | contents of `~/.tauri/tab-guard.key` (update signing) | Tab Guard |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | contents of `~/.tauri/tab-guard.key.password` | Tab Guard |

The certificate expires; the Apple item's notes say when and how to make a new one. The
update-signing key pair is in its item and in `~/.tauri/` on the Mac that made it. **If it's lost,
installed copies can't update** and everyone has to reinstall by hand. Its public half is
`plugins.updater.pubkey` in `tauri.conf.json`.
