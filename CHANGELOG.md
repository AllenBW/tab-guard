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
