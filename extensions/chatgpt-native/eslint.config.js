// Lint config for the chatgpt-native extension sources.
// Scope: no-undef only — this exists to stop undefined identifiers reaching
// runtime (yz-2mf: `selectionFailure(state, ...)` with no `state` in scope).
// No preset is extended, so no other rule is active; `no-undef` is the only
// rule enabled.
import globals from "globals";

export default [
  {
    ignores: ["tests/**", "node_modules/**", "icons/**"],
  },
  {
    files: ["src/**/*.js"],
    linterOptions: {
      reportUnusedDisableDirectives: "off",
    },
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: {
        // MV3 service-worker + content-script runtime.
        chrome: "readonly",
        // Browser DOM globals available in the content-script context
        // (referenced via globalThis in the service worker, bare in
        // content scripts).
        ...globals.browser,
        // ES builtins (globalThis, console, structuredClone, ...).
        ...globals.es2025,
        // chunks.js probes `typeof Buffer` for the Node-side native host
        // path before falling back to atob/btoa — a real runtime global.
        Buffer: "readonly",
      },
    },
    rules: {
      // The deliverable. Everything else stays off.
      "no-undef": "error",
    },
  },
];
