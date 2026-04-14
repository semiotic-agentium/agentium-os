import js from "@eslint/js";
import tseslint from "typescript-eslint";
import pluginVue from "eslint-plugin-vue";

export default tseslint.config(
  { ignores: ["dist/**"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs["flat/recommended"],
  {
    languageOptions: {
      globals: {
        window: "readonly",
        document: "readonly",
        fetch: "readonly",
        setTimeout: "readonly",
        clearTimeout: "readonly",
        setInterval: "readonly",
        clearInterval: "readonly",
        confirm: "readonly",
        console: "readonly",
        HTMLElement: "readonly",
        HTMLTextAreaElement: "readonly",
        HTMLSelectElement: "readonly",
        HTMLInputElement: "readonly",
        KeyboardEvent: "readonly",
        Event: "readonly",
        BeforeUnloadEvent: "readonly",
        EventSource: "readonly",
        AbortController: "readonly",
        Blob: "readonly",
        URL: "readonly",
        Response: "readonly",
        RequestInit: "readonly",
        Headers: "readonly",
        MouseEvent: "readonly",
        MutationObserver: "readonly",
        IntersectionObserver: "readonly",
      },
    },
  },
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
      },
    },
  },
  {
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "warn",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "vue/multi-word-component-names": "off",
      "vue/max-attributes-per-line": "off",
      "vue/singleline-html-element-content-newline": "off",
      "vue/html-self-closing": [
        "warn",
        { html: { void: "always", normal: "never" } },
      ],
    },
  },
);
