// ESLint for web/src (H15-36): typescript-eslint's recommended rules, React hook rules and
// complexity limits agreed on H15-36. `pnpm lint` runs it; so do `just check` and the pre-commit hook.
import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/**", "src/gen/**", "src/vendor/**", "node_modules/**"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}", "scripts/**/*.mjs", "*.config.{js,ts}"],
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // Complexity limits (H15-36, the "stricter" choice).
      complexity: ["error", 10],
      "max-depth": ["error", 4],
      "max-lines-per-function": ["error", { max: 150, skipBlankLines: true, skipComments: true }],
      "max-params": ["error", 5],
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
    },
  },
  {
    // Tests are long tables of cases, not logic.
    files: ["src/**/*.test.{ts,tsx}"],
    rules: { "max-lines-per-function": "off" },
  },
  {
    files: ["scripts/**/*.mjs"],
    languageOptions: { globals: { console: "readonly", process: "readonly", URL: "readonly" } },
  },
);
