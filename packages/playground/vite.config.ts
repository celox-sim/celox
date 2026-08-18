import {
  defineConfig,
  type Plugin,
  type PreviewServer,
  type ViteDevServer,
} from "vite";
import monacoEditorPlugin from "vite-plugin-monaco-editor";

const monacoPlugin = (monacoEditorPlugin as any).default || monacoEditorPlugin;

// Vite's server.headers only applies to the main HTML.
// We need COOP/COEP on ALL responses for SharedArrayBuffer to work.
function crossOriginIsolation(): Plugin {
  const configureHeaders = (server: PreviewServer | ViteDevServer) => {
    server.middlewares.use((_, res, next) => {
      res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
      res.setHeader("Cross-Origin-Embedder-Policy", "credentialless");
      next();
    });
  };

  return {
    name: "cross-origin-isolation",
    configureServer: configureHeaders,
    configurePreviewServer: configureHeaders,
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
  worker: {
    format: "es",
  },
});
