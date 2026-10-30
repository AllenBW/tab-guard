// The window, driven like a person would: clicks, typing and the keyboard.
import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

/** The arguments of every `cmd` the page has sent so far. */
const sent = (page, cmd) => page.evaluate((c) => window.__calls.filter(([n]) => n === c).map(([, a]) => a), cmd);

/** Open the window (`?browsers=2` for Firefox and Nightly side by side) past the welcome card. */
async function open(page, query = "") {
  await page.goto(`/${query}`);
  await expect(page.locator("#headline")).toContainText("using");
  await page.getByRole("button", { name: "Got it" }).click();
  await expect(page.locator("#welcome")).toBeHidden();
}

const tab = (page, name) => page.getByRole("tab", { name });
const rows = (page) => page.locator("#rows tr");

test("tabs switch by clicking and with the arrow keys", async ({ page }) => {
  await open(page);
  await tab(page, "Limits & settings").click();
  await expect(page.locator("#settings")).toBeVisible();
  await expect(page.locator("#processes")).toBeHidden();
  await expect(tab(page, "Limits & settings")).toHaveAttribute("aria-selected", "true");

  await page.keyboard.press("ArrowRight");
  await expect(tab(page, /Activity/)).toBeFocused();
  await expect(page.locator("#activity")).toBeVisible();
  await page.keyboard.press("End");
  await expect(page.locator("#about")).toBeVisible();
  await page.keyboard.press("Home");
  await expect(page.locator("#processes")).toBeVisible();
  await expect(tab(page, "Processes")).toHaveAttribute("tabindex", "0");
  await expect(tab(page, "About")).toHaveAttribute("tabindex", "-1");
});

test("the mode is chosen from a radio group", async ({ page }) => {
  await open(page);
  await page.getByRole("radio", { name: "Close at limit" }).click();
  expect((await sent(page, "save_config")).at(-1).config.mode).toBe("close");
  await expect(page.getByRole("radio", { name: "Close at limit" })).toHaveAttribute("aria-checked", "true");
});

test("with one Firefox, nothing about copies shows", async ({ page }) => {
  await open(page);
  await expect(page.locator("#proc-filter")).toBeHidden();
  await expect(page.locator(".kind.browser")).toHaveCount(0);
  await expect(page.locator("#subline")).toHaveText("1 over a limit · 1 past the warning level");
  await expect(page.getByRole("button", { name: "Which site is which? Open about:processes" })).toBeVisible();
  await tab(page, "Limits & settings").click();
  await expect(page.locator("#limit-picker")).toBeHidden();
  await expect(page.locator("#settings legend").first()).toHaveText("Per-tab limits");
});

test("the process list can be filtered to one Firefox", async ({ page }) => {
  await open(page, "?browsers=2");
  await expect(page.locator("#subline")).toContainText("Firefox 7.7 GB · Nightly 1.1 GB");
  await expect(rows(page)).toHaveCount(8);
  await page.getByRole("radio", { name: "Nightly" }).click();
  await expect(rows(page)).toHaveCount(2);
  for (const row of await rows(page).all()) await expect(row).toContainText("Nightly");
  // The arrow keys move along the filter too.
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByRole("radio", { name: "Firefox", exact: true })).toBeFocused();
  await expect(rows(page)).toHaveCount(6);
  await page.getByRole("radio", { name: "All" }).click();
  await expect(rows(page)).toHaveCount(8);
});

test("Chrome alone: named in the headline, with its Task Manager", async ({ page }) => {
  await open(page, "?browsers=chrome");
  await expect(page.locator("#headline")).toHaveText("Google Chrome is using 8.7 GB across 24 tab processes");
  await expect(page.locator("#proc-filter")).toBeHidden();
  await page.getByRole("button", { name: "Which site is which? Open its Task Manager" }).click();
  expect(await sent(page, "open_about_processes")).toEqual([{ browser: 48857 }]);
  await expect(rows(page).filter({ hasText: "48857" })).toContainText("Google Chrome");
});

test("Firefox and Chrome together each get their own way to see sites", async ({ page }) => {
  await open(page, "?browsers=mixed");
  await expect(page.locator("#headline")).toHaveText("Your browsers are using 8.7 GB across 24 tab processes");
  await expect(page.locator("#subline")).toContainText("Firefox 7.7 GB · Google Chrome 1.1 GB");
  await page.getByRole("button", { name: "Show the Task Manager in Google Chrome" }).click();
  await expect(page.getByRole("button", { name: "Open about:processes in Firefox" })).toBeVisible();
  expect(await sent(page, "open_about_processes")).toEqual([{ browser: 90001 }]);
});

test("about:processes opens in the Firefox you pick", async ({ page }) => {
  await open(page, "?browsers=2");
  await page.getByRole("button", { name: "Open about:processes in Firefox Nightly" }).click();
  await expect(page.locator("#toast")).toBeVisible();
  expect(await sent(page, "open_about_processes")).toEqual([{ browser: 90001 }]);
});

test("each Firefox keeps its own limits", async ({ page }) => {
  await open(page, "?browsers=2");
  await tab(page, "Limits & settings").click();
  await expect(page.locator("#limit-picker")).toBeVisible();
  await page.getByRole("radio", { name: "Nightly" }).click();
  await expect(page.locator("#settings legend").first()).toHaveText("Per-tab limits · Nightly");
  await expect(page.locator("#total-label")).toHaveText("When all of Firefox Nightly passes");
  await page.locator("[name=total_gb]").fill("4");
  // Switch straight away: what was typed is saved for Nightly first.
  await page.getByRole("radio", { name: "Firefox", exact: true }).click();
  await expect(page.locator("[name=total_gb]")).toHaveValue("8");
  await page.getByRole("radio", { name: "Nightly" }).click();
  await expect(page.locator("[name=total_gb]")).toHaveValue("4");

  const saved = (await sent(page, "save_config")).at(-1).config;
  expect(saved.browser_limits["/Applications/Firefox Nightly.app"].total_mb).toBe(4096);
  expect(saved.total_mb).toBe(8192);
  expect(saved.browser_limits["/Applications/Firefox.app"]).toBeUndefined();
});

test("settings are saved as you type", async ({ page }) => {
  await open(page);
  await tab(page, "Limits & settings").click();
  await page.locator("[name=kill_gb]").fill("3");
  await expect.poll(async () => (await sent(page, "save_config")).at(-1).config.kill_mb).toBe(3072);
  await expect(page.locator("#saved")).toContainText("Saved");
});

test("Close asks once more before closing", async ({ page }) => {
  await open(page);
  const close = page.getByRole("button", { name: "Close tab process 48991 (2.2 GB)" });
  await close.click();
  const confirm = page.getByRole("button", { name: "Confirm closing tab process 48991 (2.2 GB)" });
  await expect(confirm).toHaveText("Confirm");
  expect(await sent(page, "close_tab")).toEqual([]);
  await confirm.click();
  expect(await sent(page, "close_tab")).toEqual([{ pid: 48991 }]);
});

test("keyboard focus stays put when the table refreshes", async ({ page }) => {
  await open(page);
  const spare = page.getByRole("button", { name: "Spare tab process 50214 (796 MB)" });
  await spare.focus();
  // Mark the row, then wait for a refresh to replace it.
  await page.evaluate(() => document.querySelector("#rows tr").setAttribute("data-old", ""));
  await expect(page.locator("#rows tr[data-old]")).toHaveCount(0, { timeout: 5000 });
  await expect(spare).toBeFocused();
});

test("an update check answers under its button", async ({ page }) => {
  await open(page);
  await tab(page, "About").click();
  await page.getByRole("button", { name: "Check for updates" }).click();
  await expect(page.locator("#update-status")).toHaveText("Tab Guard 1.0.0 is the latest version (checked 09:16)");
  await expect(page.locator("#toast")).toBeHidden();
});

test("the About tab's links go through the app", async ({ page }) => {
  await open(page);
  await tab(page, "About").click();
  await page.getByRole("button", { name: "Take a walk." }).click();
  expect(await sent(page, "open_link")).toEqual([{ name: "walk" }]);
});

test("long lists scroll inside their box, not the window", async ({ page }) => {
  await page.setViewportSize({ width: 980, height: 560 });
  await open(page);
  const box = page.locator("#processes .scroll-box");
  const fits = (el) => el.evaluate((e) => [e.scrollHeight <= e.clientHeight + 1, e.scrollWidth <= e.clientWidth + 1]);
  expect(await fits(page.locator("body"))).toEqual([true, true]);
  expect((await fits(box))[0]).toBe(false);
  // The column names stay in view while the list scrolls.
  await box.evaluate((e) => (e.scrollTop = e.scrollHeight));
  await expect(page.getByRole("columnheader", { name: "Memory" })).toBeInViewport();
});

for (const width of [720, 980]) {
  test(`nothing runs off the side at ${width} px with two Firefoxes`, async ({ page }) => {
    await page.setViewportSize({ width, height: 760 });
    await open(page, "?browsers=2");
    for (const name of ["Processes", "Limits & settings", /Activity/, "About"]) {
      await tab(page, name).click();
      const wide = await page.evaluate(() =>
        [...document.querySelectorAll("body, .scroll-box, .panel:not([hidden]) *:not(.visually-hidden)")]
          .filter((e) => e.offsetParent && e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).overflowX !== "visible")
          .map((e) => e.id || e.className),
      );
      expect(wide, `${name}`).toEqual([]);
    }
  });
}

for (const scheme of ["dark", "light"]) {
  test(`no accessibility problems on any tab (${scheme})`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await open(page, "?browsers=2");
    for (const name of ["Processes", "Limits & settings", /Activity/, "About"]) {
      await tab(page, name).click();
      const { violations } = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
      expect(violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`), `${name}`).toEqual([]);
    }
  });
}
