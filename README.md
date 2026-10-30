# Tab Guard

A free macOS menu-bar app that keeps Firefox tabs from taking over the Mac's memory.

**[Download the latest release](https://github.com/AllenBW/tab-guard/releases/latest)**: open the
`.dmg` and drag Tab Guard to Applications. macOS 13 or later, Apple silicon and Intel; signed,
notarized, and it updates itself.

macOS has no way to cap one app's memory, so Tab Guard watches instead. It checks
every Firefox process every 15 seconds, flags the tab processes that pass your
limits, and, if you let it, closes them one at a time. Firefox shows a closed tab
as crashed, with a **Restore** button that reloads it.

## Using it

The shield in the menu bar shows Firefox's total memory (⚠︎ when a tab is over a
limit). Its menu turns Tab Guard on or off, switches modes, and opens the window.

| Mode | What happens |
|---|---|
| **Watch only** (default) | Nothing is closed. Tabs over a limit are marked "Would close", with the reason. |
| **Warn, then close** | Tabs past the warning level are flagged; tabs over a hard limit are closed. |
| **Close at limit** | Tabs over a hard limit are closed. No early warnings. |

Limits (all editable in **Limits & settings**):

- **Per tab:** warn at 1 GB, close past 2 GB.
- **Whole browser:** past 8 GB in total, close the biggest tab, preferring an idle one
  (under 2% CPU), and never one under 400 MB. Some of Firefox's memory isn't in any tab,
  so this limit backs off: if a close doesn't bring the total down, it pauses for 15
  minutes, and it closes at most 3 tabs an hour. Per-tab limits keep working meanwhile.
- **Pacing:** at most one close per check, at least 60 s apart.

In the window, **Spare** protects a tab process until it exits and **Close** closes one
now. Firefox doesn't tell other apps which website runs in which process; the
**Open about:processes** button asks Firefox to open that page (macOS asks once whether Tab
Guard may control Firefox) and also puts the address on the clipboard as a fallback.

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

- `pnpm test` runs the UI helper tests (node) and the Rust tests.
- `cd src-tauri && cargo test live_sample -- --ignored --nocapture` samples the Firefox that's running
  now; `cargo test --release bench_sample -- --ignored --nocapture` times a check.
- `node scripts/preview-server.mjs` serves the window's UI at http://localhost:5174 with a
  fake backend, for layout work without rebuilding.
- `pnpm icons` redraws the icons from `scripts/make-icons.mjs`.
- [RELEASING.md](RELEASING.md) covers signed releases; `docs/` is the project page.

## License

MIT. See [LICENSE](LICENSE).
