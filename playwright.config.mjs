// Browser tests of the window, against the fake backend in
// scripts/preview-server.mjs. WebKit, since the app's window is WebKit too.
import { defineConfig } from "@playwright/test";

const port = 5175;

export default defineConfig({
  testDir: "tests/ui",
  fullyParallel: true,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: `http://localhost:${port}`,
    browserName: "webkit",
    viewport: { width: 980, height: 760 },
    colorScheme: "dark",
  },
  webServer: {
    command: "node scripts/preview-server.mjs",
    env: { PORT: String(port) },
    url: `http://localhost:${port}`,
    reuseExistingServer: !process.env.CI,
  },
});
