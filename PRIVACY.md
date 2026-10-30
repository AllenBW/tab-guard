# Privacy

Tab Guard runs on your Mac. It reads the memory and CPU use of your browsers' processes
from macOS, and keeps its settings and activity log in your home folder. It doesn't read your
browsing: it never sees which website runs in which process. It does go online to check for
updates (below), and to send usage counts if you turn them on.

## Anonymous usage counts (off unless you turn them on)

Tab Guard asks once, in its welcome card, whether to share anonymous usage counts. You can
change your answer any time under **Limits & settings → General**. While it's off, nothing is
sent.

While it's on, Tab Guard sends these events to [Aptabase](https://aptabase.com), an
open-source analytics service:

| Event | When | Details sent |
|---|---|---|
| `app_started` | Tab Guard starts | the mode (Watch only, Warn, then close, or Close at limit) |
| `tab_closed` | a tab process is closed | which rule closed it: per-tab limit, whole-browser limit, or you |
| `whole_browser_paused` | the whole-browser limit backs off | why: closing didn't help, or the hourly cap |
| `mode_changed` | you switch modes | the new mode |

Every event also carries its time, the Tab Guard and macOS versions, whether it's a development
build, and a random session ID that's new each time Tab Guard starts. Tab Guard sends nothing that
links one session to another.

Never sent: website addresses, tab titles, process names or IDs, memory or CPU figures, your
name, account, computer name, or any lasting identifier. Aptabase processes the IP address and
user agent of each request in memory only and never stores them; to count daily users it derives
a pseudonymous ID from them with a salt it rotates every 24 hours, so days can't be linked. See
[Aptabase's privacy policy](https://aptabase.com/legal/privacy).

The code that sends these is in [`src-tauri/src/usage.rs`](src-tauri/src/usage.rs).

## Updates

With **Install updates automatically** on (the default), Tab Guard checks GitHub for a new
release a minute after it starts and every 6 hours. That request is an ordinary download from
github.com and includes no information about you or your browsers. Each update is
signature-checked before it's installed.

## Controlling your browser

macOS asks once whether Tab Guard may control Firefox (or another Firefox-based browser). That
permission is only used by the **Which site is which?** button, to ask the browser to open its
`about:processes` page. Chrome-family browsers are handed the address of their
`chrome://process-internals` page the ordinary way, which needs no permission.

## Links

The links under **About** open in your browser like any other link. **Take a walk.** goes to the
author's website with `?ref=tab-guard-app` on the address, so that site can count how many walks
start from Tab Guard. The tag names the app, nothing about you; the link on the project page says
`?ref=tab-guard-page` instead.
