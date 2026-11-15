import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

// THREAT_MODEL UI-01 / Inv-23: nothing may turn engine data into markup or code.
const htmlSinks = ["innerHTML", "outerHTML", "insertAdjacentHTML", "srcdoc"];

export default tseslint.config(
  { ignores: ["dist", "src-tauri"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  reactHooks.configs.flat["recommended-latest"],
  reactRefresh.configs.vite,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: { ecmaVersion: 2023, globals: globals.browser },
    rules: {
      "no-eval": "error",
      "no-implied-eval": "error",
      "no-new-func": "error",
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: "dangerouslySetInnerHTML is banned (UI-01).",
        },
        {
          selector: `MemberExpression[property.name=/^(${htmlSinks.join("|")})$/]`,
          message: "HTML injection sinks are banned (UI-01).",
        },
        {
          selector: "MemberExpression[object.name='document'][property.name='write']",
          message: "document.write is banned (UI-01).",
        },
      ],
    },
  },
);
