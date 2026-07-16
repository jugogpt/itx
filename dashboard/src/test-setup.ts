import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// `@testing-library/react`'s auto-cleanup-after-each-test only
// self-registers when it detects the test framework's globals on
// `globalThis` -- this project deliberately doesn't enable Vitest's
// `globals: true` (explicit `import { it, expect, ... } from "vitest"`
// in every test file instead), so that detection never fires and DOM
// from one test leaks into the next unless this is done by hand.
afterEach(() => {
  cleanup();
});
