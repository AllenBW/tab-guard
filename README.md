# Tab Guard

A macOS menu-bar app that keeps Firefox tabs from taking over the Mac's memory.

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
  (under 2% CPU), and never one under 400 MB.
- **Pacing:** at most one close per check, at least 60 s apart.

In the window, **Spare** protects a tab process until it exits and **Close** closes one
now. Firefox doesn't tell other apps which website runs in which process; the
**Open about:processes** button asks Firefox to open that page (macOS asks once whether Tab
Guard may control Firefox) and also puts the address on the clipboard as a fallback.

Settings: `~/Library/Application Support/local.tabguard/config.json`
Activity log: `~/Library/Logs/Tab Guard.log`

## Build and install

Needs Rust, Node and pnpm.

```bash
pnpm install
pnpm build
cp -R "src-tauri/target/release/bundle/macos/Tab Guard.app" ~/Applications/
open ~/Applications/"Tab Guard.app"
```

On first launch it opens its window and turns on **Start Tab Guard when I log in**.

To remove it: quit it from the menu bar, then delete `~/Applications/Tab Guard.app`
and `~/Library/LaunchAgents/Tab Guard.plist`.

## Development

- `pnpm test` runs the UI helper tests (node) and the Rust tests.
- `cd src-tauri && cargo test live_sample -- --ignored --nocapture` samples the Firefox that's running
  now; `cargo test --release bench_sample -- --ignored --nocapture` times a check.
- `node scripts/preview-server.mjs` serves the window's UI at http://localhost:5174 with a
  fake backend, for layout work without rebuilding.
- `pnpm icons` redraws the icons from `scripts/make-icons.mjs`.
