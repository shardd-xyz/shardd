import { defineConfig, devices } from "@playwright/test";

const mockPort = Number(process.env.PLAYWRIGHT_MOCK_PORT || 4183);
const mockUrl = `http://127.0.0.1:${mockPort}`;

export default defineConfig({
  testDir: "./tests",
  testMatch: /mock-.*\.spec\.ts/,
  timeout: 45_000,
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: [
    ["html", { outputFolder: "playwright-report", open: "never" }],
    ["junit", { outputFile: "test-results/e2e-mock.xml" }],
    ["list"],
  ],
  use: {
    baseURL: process.env.PLAYWRIGHT_BASE_URL || mockUrl,
    screenshot: "only-on-failure",
    trace: "on-first-retry",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: {
    command: "node support/mock-server.mjs",
    url: mockUrl,
    reuseExistingServer: false,
    timeout: 15_000,
  },
});
