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
3. Wait for **Release Build (Draft)** (about 15 minutes), then download the DMG from the draft
   release and check: it opens without a Gatekeeper warning, the menu-bar icon appears, the
   window opens, and **Which site is which?** works with Firefox (about:processes) and Chrome (it
   comes forward).
4. Publish the draft. Installed copies pick it up within 6 hours (or on **Check for updates**).

## One-time setup

Repository secrets (Settings → Secrets and variables → Actions):

| Secret | What |
|---|---|
| `APPLE_CERTIFICATE` | Developer ID Application certificate + key, as a base64 `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_PASSWORD` | an app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | `B6J5X2WZ98` |
| `TAURI_SIGNING_PRIVATE_KEY` | contents of `~/.tauri/tab-guard.key` (update signing) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | contents of `~/.tauri/tab-guard.key.password` |

Repository variable (not secret: it ships inside the app): `APTABASE_APP_KEY`, the Aptabase
app key (`A-US-…` or `A-EU-…`). Without it, builds never send usage counts and don't offer the
setting.

The update-signing key pair lives in `~/.tauri/`; keep a copy in the password manager. **If it's
lost, installed copies can't update** and everyone has to reinstall by hand. Its public half is
`plugins.updater.pubkey` in `tauri.conf.json`.
