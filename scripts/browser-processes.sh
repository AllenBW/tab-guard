#!/usr/bin/env bash
# Prints every process of the browsers Tab Guard watches, with its parent, as the kernel reports
# them: what to run when the window lists browsers that aren't there ("Google Chrome (12)") or
# misses one. The output holds no page titles or addresses, only paths inside the browser apps
# and the flags that say what each helper is, so it can go in a bug report. A line ending
# "[file gone]" is a process whose executable has been deleted under it, which an installer can
# do to a running browser; the sampler then has to find its path another way.
set -euo pipefail
ps -axo pid=,ppid=,rss=,comm= | awk '
  /\/(Firefox|Zen|LibreWolf|Waterfox|Floorp|Mullvad Browser|Tor Browser|Google Chrome|Microsoft Edge|Brave Browser|Arc|Vivaldi|Opera|Chromium)[^\/]*\.app\/Contents\// {
    path = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9]+ +/, "", path)
    printf "%s\t%s\t%s\t%s\n", $1, $2, int($3 / 1024), path
  }' | sort -t "$(printf '\t')" -k2,2n -k1,1n | while IFS=$'\t' read -r pid ppid mb path; do
  flags=$(ps -o args= -p "$pid" | tr ' ' '\n' | grep -E '^(--type=|--extension-process|--utility-sub-type=|-isForBrowser|--app-id=|tab)$|^(--type|--utility-sub-type|--app-id)=' | tr '\n' ' ' || true)
  gone=""; [ -e "$path" ] || gone=" [file gone]"
  printf "%6s  parent %6s  %5s MB  %s  %s%s\n" "$pid" "$ppid" "$mb" "$path" "$flags" "$gone"
done
