// Builds the React islands and a self-hosted React runtime as ES modules into dist/static/.
// crates/streamdockd embeds dist/ and serves it under /static; the page's import map points
// the bare specifiers "react", "react/jsx-runtime" and "react-dom/client" at the vendor
// entries, so proa's bootstrap and every island share one React instance.
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "dist/static",
    emptyOutDir: true,
    target: "es2022",
    modulePreload: false,
    rollupOptions: {
      preserveEntrySignatures: "strict",
      input: {
        "vendor/react": "src/vendor/react.js",
        "vendor/react-jsx-runtime": "src/vendor/react-jsx-runtime.js",
        "vendor/react-dom-client": "src/vendor/react-dom-client.js",
        "islands/BuildVersion": "src/islands/BuildVersion.tsx",
        "islands/Changelog": "src/islands/Changelog.tsx",
        "islands/Dashboard": "src/islands/Dashboard.tsx",
        "islands/Settings": "src/islands/Settings.tsx",
        "islands/KeysEditor": "src/islands/KeysEditor.tsx",
        "islands/DesignSystem": "src/islands/DesignSystem.tsx",
      },
      output: {
        format: "es",
        entryFileNames: "[name].js",
        chunkFileNames: "chunks/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]",
      },
    },
  },
});
