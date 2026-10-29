# Changelog

## 1.0.0

First public release.

- Watches browsers' tab processes from the menu bar and, if you choose, closes the ones that
  pass your per-tab or whole-browser memory limits. Supports Firefox and browsers built on it
  (Zen, LibreWolf, Waterfox, Floorp, Mullvad Browser, Tor Browser) and Chrome and browsers built
  on Chromium (Edge, Brave, Arc, Vivaldi, Opera).
- Three modes: Watch only (the default), Warn, then close, and Close at limit.
- The menu-bar shield shows the state by color (green guarding, orange worth a look, red
  closing) and is struck through while paused. Pause for an hour or until 6 in the
  morning and Tab Guard turns itself back on.
- The whole-browser limit pauses when closing a tab doesn't lower the browser's total, and closes
  at most 3 tabs an hour per browser.
- Only processes that hold web pages are closed: never Firefox's extensions process, its own
  about: pages or its service workers, nor Chrome's extensions.
- Works with several browsers at once (or one browser with a second profile): each row says which
  one it belongs to, the process list can be filtered to one browser, and each has its own
  per-tab and whole-browser limits.
- Spare or close any tab process from the window; an activity log shows what happened and why.
- Signed and notarized, with automatic updates.
- Optional anonymous usage counts, off unless you turn them on (see PRIVACY.md).
- An About tab with your version, a manual update check, and links.
- Works with the keyboard and VoiceOver: arrow keys move between tabs and modes, every
  button says which process it acts on, and focus stays put while the table refreshes.
- Opening Tab Guard again from Spotlight or Applications while it's running shows its window.
