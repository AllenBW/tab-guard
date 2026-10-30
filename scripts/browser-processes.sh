#!/usr/bin/env bash
# Prints every process of the browsers Tab Guard watches, with its parent, as the kernel reports
# them: what to run when the window lists browsers that aren't there ("Google Chrome (12)") or
# misses one. The output holds no page titles or addresses, only paths inside the browser apps
# and the flags that say what each helper is, so it can go in a bug report.
#
# A note in brackets at the end of a line is the thing to look at. "kernel can't name it" is a
# process whose executable was deleted under it (an installer can do that to a running browser,
# leaving a newer copy at the same path); the sampler then has to find its path another way.
# "file gone" is a path that no longer exists at all. The kernel check needs python3; without
# it only the file check runs.
set -euo pipefail
rows=$(ps -axo pid=,ppid=,rss=,comm= | awk '
  /\/(Firefox|Zen|LibreWolf|Waterfox|Floorp|Mullvad Browser|Tor Browser|Google Chrome|Microsoft Edge|Brave Browser|Arc|Vivaldi|Opera|Chromium)[^\/]*\.app\/Contents\// {
    path = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9]+ +/, "", path)
    printf "%s\t%s\t%s\t%s\n", $1, $2, int($3 / 1024), path
  }' | sort -t "$(printf '\t')" -k2,2n -k1,1n)
[ -n "$rows" ] || { echo "No browser is running."; exit 0; }

if command -v python3 >/dev/null; then
  printf '%s\n' "$rows" | python3 -I -c '
import ctypes, ctypes.util, os, subprocess, sys
libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
for line in sys.stdin:
    pid, ppid, mb, path = line.rstrip("\n").split("\t")
    args = subprocess.run(["ps", "-o", "args=", "-p", pid], capture_output=True, text=True).stdout.split()
    flags = [a for a in args if a.startswith(("--type=", "--utility-sub-type=", "--app-id=")) or a in ("--extension-process", "-isForBrowser")]
    if args and args[-1] == "tab":
        flags.append("tab")
    notes = []
    if not os.path.exists(path):
        notes.append("file gone")
    buf = ctypes.create_string_buffer(4096)
    if libc.proc_pidpath(int(pid), buf, 4096) <= 0:
        notes.append("kernel can'"'"'t name it")
    elif buf.value.decode() != path:
        notes.append("kernel says " + buf.value.decode())
    print(f"{pid:>6}  parent {ppid:>6}  {mb:>5} MB  {path}  {" ".join(flags)}" + (f"  [{", ".join(notes)}]" if notes else ""))
'
else
  printf '%s\n' "$rows" | while IFS=$'\t' read -r pid ppid mb path; do
    flags=$(ps -o args= -p "$pid" | tr ' ' '\n' | grep -E '^(--extension-process|-isForBrowser|tab)$|^(--type|--utility-sub-type|--app-id)=' | tr '\n' ' ' || true)
    gone=""; [ -e "$path" ] || gone="  [file gone]"
    printf "%6s  parent %6s  %5s MB  %s  %s%s\n" "$pid" "$ppid" "$mb" "$path" "$flags" "$gone"
  done
fi
