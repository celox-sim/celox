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
                  text: "パラメータオーバーライド",
                  link: "/ja/guide/parameter-overrides",
                },
                {
                  text: "子インスタンスへのアクセス",
                  link: "/ja/guide/hierarchy",
                },
                {
                  text: "デッドストア除去",
                  link: "/ja/guide/dead-store-elimination",
                },
                {
                  text: "組み合わせループ",
                  link: "/ja/guide/combinational-loops",
                },
                {
                  text: "VCD 波形出力",
                  link: "/ja/guide/vcd",
                },
                {
                  text: "型変換",
                  link: "/ja/guide/type-conversion",
                },
                {
                  text: "Vite プラグイン",
                  link: "/ja/guide/vite-plugin",
                },
                {
                  text: "celox.toml",
                  link: "/ja/guide/celox-toml",
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
            { text: "Parameter Overrides", link: "/guide/parameter-overrides" },
            { text: "Child Instance Access", link: "/guide/hierarchy" },
            { text: "Dead Store Elimination", link: "/guide/dead-store-elimination" },
            { text: "Combinational Loops", link: "/guide/combinational-loops" },
            { text: "VCD Waveform Output", link: "/guide/vcd" },
            { text: "Type Conversion", link: "/guide/type-conversion" },
            { text: "Vite Plugin", link: "/guide/vite-plugin" },
            { text: "celox.toml", link: "/guide/celox-toml" },
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
