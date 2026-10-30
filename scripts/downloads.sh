#!/usr/bin/env bash
# Prints how many times each release file has been downloaded, from GitHub's own count, and what
# that says about installs. Nothing in the app reports anything; this reads the counts GitHub keeps
# for every public release.
#
#   - The installer's count is downloads, not installs: a second download by the same person counts
#     again, and a download never opened counts once.
#   - Every running copy fetches latest.json about four times a day for its update check (PRIVACY.md),
#     so that file's count since it was uploaded, divided by four a day, is roughly how many Macs ran
#     Tab Guard each day.
#   - An archive on the Updater files release is downloaded once per copy that applied the update.
#   - Replacing a file resets its count, so a count starts again at each release.
set -euo pipefail
for tool in gh jq; do command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }; done
REPO=$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || echo AllenBW/tab-guard)
gh api "repos/$REPO/releases" --paginate | jq -r '
  .[]
  | "\(.name) (\(.tag_name)\(if .prerelease then ", pre-release" else "" end))",
    (.assets[]
      | .download_count as $n
      | ((now - (.created_at | fromdateiso8601)) / 86400) as $days
      | if (.name | endswith(".dmg")) then
          "  \(.name): \($n) downloads"
        elif .name == "latest.json" then
          "  \(.name): \($n) fetches in \($days * 10 | round / 10) days; a running copy fetches it about 4 times a day, so "
          + (if $days < 0.5 then "it is too new to say how many Macs are running it"
             else "about \($n / $days / 4 | round) Macs ran Tab Guard each day" end)
        elif (.name | endswith(".app.tar.gz")) then
          "  \(.name): \($n) updates applied"
        elif (.name | endswith(".sig")) then empty
        else "  \(.name): \($n)" end),
    ""'
