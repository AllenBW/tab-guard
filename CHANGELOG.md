# Changelog

## 1.0.0

First public release.

- Watches Firefox's tab processes from the menu bar and, if you choose, closes the ones that
  pass your per-tab or whole-browser memory limits.
- Three modes: Watch only (the default), Warn then close, and Close at limit.
- The whole-browser limit backs off when closing tabs doesn't bring Firefox down, and closes
  at most 3 tabs an hour.
- Spare or close any tab process from the window; an activity log shows what happened and why.
- Signed and notarized, with automatic updates.
- Optional anonymous usage counts, off unless you turn them on (see PRIVACY.md).
- An About tab (also in the menu bar) with your version, a manual update check, and links.
- Works with the keyboard and VoiceOver: arrow keys move between tabs and modes, every
  button says which process it acts on, and focus stays put while the table refreshes.
- The window opens tall enough for its longest tab, up to the height of your screen. Opening
  Tab Guard again from Spotlight or Applications while it's running shows the window.
