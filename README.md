# Tab Guard

A free macOS menu-bar app that stops runaway browser tabs from taking over your Mac.

**[Download the latest release](https://github.com/AllenBW/tab-guard/releases/latest)**: open the
`.dmg` and drag Tab Guard to Applications. macOS 13 or later, Apple silicon and Intel; signed,
notarized, and it updates itself.

## Why

A badly built or badly behaved website shouldn't be able to run amok with your computer's
resources. One tab with a leak can grow to several gigabytes, push the Mac into swap, and slow
everything else down, and you usually find out only when the fans spin up. The fix is tedious:
open Activity Monitor, guess which process is the culprit, find the tab, close it, and repeat
next time.

Tab Guard does that for you. macOS can't cap how much memory one app uses, so Tab Guard watches
instead: every 15 seconds it checks each browser process, flags the tabs that pass your limits,
and, if you let it, closes them one at a time. The browser shows a closed tab as crashed, with a
button that reloads it.

## Supported browsers

- **Firefox** and browsers built on it: Firefox Beta, Developer Edition and Nightly, Zen,
  LibreWolf, Waterfox, Floorp, Mullvad Browser and Tor Browser.
- **Chrome** and browsers built on Chromium: Google Chrome (including Beta, Dev and Canary),
  Microsoft Edge, Brave, Arc, Vivaldi, Opera and Chromium.

Several can run at once; each gets its own limits. Chrome-family browsers start a spare process
ahead of your next tab, so their tab-process count can be one or two higher than the tabs you
have open; those spares are small and never reach a limit. Safari isn't supported: its page processes are
shared with Mail and other apps, so Tab Guard can't tell which are Safari's tabs. Apps built on
the same engines that aren't browsers (Thunderbird, or Electron apps like Slack and VS Code) are
left alone.

## Limitations: read before turning on closing

- **Closing a tab is abrupt.** There's no warning in the browser: the tab just shows as crashed.
  Anything not saved in it is gone: a half-filled form, a draft in a web app, a video call, your
  place in a video. **Don't let Tab Guard close tabs while you're doing critical work.** Keep it in
  **Watch only**, or pause it from the menu bar, during calls, presentations, exams or long forms.
- **One process can hold several tabs.** Browsers sometimes put tabs from the same site in one
  process, so closing it crashes all of them. In Chrome-family browsers a page's embedded frames
  can run in their own process; closing one breaks that part of the page.
- **It judges by memory only.** A tab that burns CPU or battery without growing isn't closed.
- **It can't tell which website a process is.** Browsers don't tell other apps that. It acts on
  how much a process uses, never on which site it is; Firefox's about:processes page and Chrome's
  Task Manager show the site behind each process ID.
- **Not all of a browser's memory is in its tabs.** Closing tabs can't bring that part down,
  which is why the whole-browser limit backs off when closes don't help.
- **It starts in Watch only.** Nothing is closed until you choose a closing mode, so you can see
  what it would do first.

## Using it

The shield in the menu bar shows your browsers' total memory (⚠︎ when a tab is over a limit). Its
menu turns Tab Guard on or off, switches modes, and opens the window.

| Mode | What happens |
|---|---|
| **Watch only** (default) | Nothing is closed. Tabs over a limit are marked "Would close", with the reason. |
| **Warn, then close** | Tabs past the warning level are flagged; tabs over a hard limit are closed. |
| **Close at limit** | Tabs over a hard limit are closed. No early warnings. |

Limits (all editable in **Limits & settings**, separately for each browser):

- **Per tab:** warn at 1 GB, close past 2 GB.
- **Whole browser:** past 8 GB in total, close the biggest tab, preferring an idle one
  (under 2% CPU), and never one under 400 MB. If a close doesn't bring the total down, this limit
  pauses for 15 minutes, and it closes at most 3 tabs an hour. Per-tab limits keep working meanwhile.
- **Pacing:** at most one close per check, at least 60 s apart.

In the window, **Spare** protects a tab process until it exits and **Close** closes one now. With
more than one browser running, the process list can be filtered to one of them. **Which site is
which?** opens Firefox's about:processes page (macOS asks once whether Tab Guard may control
Firefox) or brings a Chrome-family browser forward and says where its Task Manager is.

Settings: `~/Library/Application Support/local.tabguard/config.json`
Activity log: `~/Library/Logs/Tab Guard.log`

On first launch it opens its window with a short welcome, and turns on **Start Tab Guard when I
log in**. To remove it: quit it from the menu bar, then delete Tab Guard from Applications and
`~/Library/LaunchAgents/Tab Guard.plist`.

## Privacy and updates

Everything runs on your Mac. Anonymous usage counts are off unless you turn them on;
[PRIVACY.md](PRIVACY.md) lists exactly what's sent. Updates are checked on GitHub every 6 hours
and installed automatically (you can turn that off in **Limits & settings → Updates**).

## Build from source

Needs Rust, Node and pnpm.

```bash
pnpm install
pnpm build
cp -R "src-tauri/target/release/bundle/macos/Tab Guard.app" ~/Applications/
open ~/Applications/"Tab Guard.app"
```

Builds from source can't update themselves (that needs the release signing key) and never send
usage counts.

## Development

- `pnpm test` runs the UI helper tests (node) and the Rust tests. `pnpm test:ui` drives the window
  in WebKit against the fake backend below (run `pnpm exec playwright install webkit` once first).
- `cd src-tauri && cargo test live_sample -- --ignored --nocapture` samples the browsers running
  now; `cargo test --release bench_sample -- --ignored --nocapture` times a check.
- `node scripts/preview-server.mjs` serves the window's UI at http://localhost:5174 with a
  fake backend, for layout work without rebuilding.
- `pnpm icons` redraws the icons from `scripts/make-icons.mjs`.
- [RELEASING.md](RELEASING.md) covers signed releases; `docs/` is the project page.

## License

MIT. See [LICENSE](LICENSE).
