import { defineConfig } from "vitepress";
import typedocSidebar from "../api/typedoc-sidebar.json";

export default defineConfig({
  title: "Celox",
  description: "JIT simulator for Veryl HDL",
  base: "/celox/",

  appearance: "dark",

  locales: {
    root: {
      label: "English",
      lang: "en",
    },
    ja: {
      label: "Japanese",
      lang: "ja",
      themeConfig: {
        nav: [
          { text: "ガイド", link: "/ja/guide/introduction" },
          { text: "API", link: "/api/" },
          { text: "内部構造", link: "/internals/architecture" },
          { text: "ベンチマーク", link: "/ja/benchmarks/" },
          {
            text: "GitHub",
            link: "https://github.com/celox-sim/celox",
          },
        ],
        sidebar: {
          "/ja/guide/": [
            {
              text: "ガイド",
              items: [
                { text: "概要", link: "/ja/guide/introduction" },
                { text: "はじめる", link: "/ja/guide/getting-started" },
                {
                  text: "テストの書き方",
                  link: "/ja/guide/writing-tests",
                },
                {
                  text: "4 値シミュレーション",
                  link: "/ja/guide/four-state",
                },
                {
                  text: "型変換",
                  link: "/ja/guide/type-conversion",
                },
                {
                  text: "Vite プラグイン",
                  link: "/ja/guide/vite-plugin",
                },
              ],
            },
          ],
        },
      },
    },
  },

  themeConfig: {
    nav: [
      { text: "Guide", link: "/guide/introduction" },
      { text: "API", link: "/api/" },
      { text: "Internals", link: "/internals/architecture" },
      { text: "Benchmarks", link: "/benchmarks/" },
      {
        text: "GitHub",
        link: "https://github.com/celox-sim/celox",
      },
    ],

    sidebar: {
      "/guide/": [
        {
          text: "Guide",
          items: [
            { text: "Introduction", link: "/guide/introduction" },
            { text: "Getting Started", link: "/guide/getting-started" },
            { text: "Writing Tests", link: "/guide/writing-tests" },
            { text: "4-State Simulation", link: "/guide/four-state" },
            { text: "Type Conversion", link: "/guide/type-conversion" },
            { text: "Vite Plugin", link: "/guide/vite-plugin" },
            { text: "Benchmarks", link: "/benchmarks/" },
          ],
        },
      ],
      "/api/": [
        {
          text: "API Reference",
          items: typedocSidebar as any,
        },
      ],
      "/internals/": [
        {
          text: "Internals",
          items: [
            { text: "Architecture", link: "/internals/architecture" },
            { text: "IR Reference", link: "/internals/ir-reference" },
            { text: "Optimizations", link: "/internals/optimizations" },
            { text: "4-State Simulation", link: "/internals/four-state" },
            {
              text: "Combinational Analysis",
              link: "/internals/combinational-analysis",
            },
            {
              text: "Cascade Limitations",
              link: "/internals/cascade-limitations",
            },
            { text: "Status", link: "/internals/status" },
          ],
        },
      ],
    },

    socialLinks: [
      { icon: "github", link: "https://github.com/celox-sim/celox" },
    ],
  },

  vite: {
    ssr: {
      noExternal: ["vue-chartjs"],
    },
    optimizeDeps: {
      include: ["chart.js"],
    },
  },
});
