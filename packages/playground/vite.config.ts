import { defineConfig, type Plugin } from "vite";
import monacoEditorPlugin from "vite-plugin-monaco-editor";

const monacoPlugin = (monacoEditorPlugin as any).default || monacoEditorPlugin;

// Vite's server.headers only applies to the main HTML.
// We need COOP/COEP on ALL responses for SharedArrayBuffer to work.
function crossOriginIsolation(): Plugin {
  return {
    name: "cross-origin-isolation",
    configureServer(server) {
      server.middlewares.use((_, res, next) => {
        res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
        res.setHeader("Cross-Origin-Embedder-Policy", "credentialless");
        next();
      });
    },
  };
}

export default defineConfig({
  base: process.env.PLAYGROUND_BASE ?? "/",
  plugins: [
    crossOriginIsolation(),
    monacoPlugin({
      languageWorkers: ["editorWorkerService", "typescript"],
    }),
  ],
  build: {
    target: "esnext",
    outDir: "dist",
  },
});
