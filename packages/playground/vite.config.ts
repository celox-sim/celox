import { defineConfig } from "vite";
import monacoEditorPlugin from "vite-plugin-monaco-editor";

const monacoPlugin = (monacoEditorPlugin as any).default || monacoEditorPlugin;

export default defineConfig({
  plugins: [
    monacoPlugin({
      languageWorkers: ["editorWorkerService", "typescript"],
    }),
  ],
  server: {
    headers: {
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    },
  },
});
