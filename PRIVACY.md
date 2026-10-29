# Privacy

Tab Guard sends nothing about you or your browsing. There are no analytics, no telemetry, no
account and no identifier. It reads the memory and CPU use of your browsers' processes from
macOS, and keeps its settings and activity log in your home folder. It never sees which website
runs in which process.

The one thing it fetches is its own update.

## Updates

With **Install updates automatically** on (the default), Tab Guard checks GitHub for a new
release a minute after it starts and every 6 hours. That request is an ordinary download from
github.com and carries no information about you or your browsers. Each update is
signature-checked before it's installed. Turn it off under **Limits & settings → General**, and
nothing leaves your Mac at all.

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
