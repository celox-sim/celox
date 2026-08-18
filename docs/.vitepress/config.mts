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
          {
            text: "プロジェクト",
            items: [
              { text: "アーキテクチャ", link: "/internals/architecture" },
              { text: "ベンチマーク", link: "/ja/benchmarks/" },
            ],
          },
          {
            text: "Playground",
            link: "https://celox-sim.github.io/celox/playground/",
          },
          {
            text: "GitHub",
            link: "https://github.com/celox-sim/celox",
          },
        ],
        sidebar: {
          "/ja/guide/": [
            {
              text: "はじめに",
              items: [
                { text: "概要", link: "/ja/guide/introduction" },
                { text: "はじめる", link: "/ja/guide/getting-started" },
              ],
            },
            {
              text: "テストベンチ",
              items: [
                {
                  text: "テストの書き方",
                  link: "/ja/guide/writing-tests",
                },
                {
                  text: "celox CLIとcocotb",
                  link: "/ja/guide/cocotb",
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
                  text: "VCD 波形出力",
                  link: "/ja/guide/vcd",
                },
                {
                  text: "型変換",
                  link: "/ja/guide/type-conversion",
                },
              ],
            },
            {
              text: "プロジェクト設定",
              items: [
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
            {
              text: "高度な機能",
              collapsed: true,
              items: [
                {
                  text: "組み合わせループ",
                  link: "/ja/guide/combinational-loops",
                },
                {
                  text: "デッドストア除去",
                  link: "/ja/guide/dead-store-elimination",
                },
                {
                  text: "最適化チューニング",
                  link: "/ja/guide/optimization-tuning",
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
      {
        text: "Project",
        items: [
          { text: "Architecture", link: "/internals/architecture" },
          { text: "Benchmarks", link: "/benchmarks/" },
        ],
      },
      {
        text: "Playground",
        link: "https://celox-sim.github.io/celox/playground/",
      },
      {
        text: "GitHub",
        link: "https://github.com/celox-sim/celox",
      },
    ],

    sidebar: {
      "/guide/": [
        {
          text: "Start Here",
          items: [
            { text: "Introduction", link: "/guide/introduction" },
            { text: "Getting Started", link: "/guide/getting-started" },
          ],
        },
        {
          text: "Testbenches",
          items: [
            { text: "Writing Tests", link: "/guide/writing-tests" },
            { text: "Celox CLI and cocotb", link: "/guide/cocotb" },
            { text: "4-State Simulation", link: "/guide/four-state" },
            { text: "Parameter Overrides", link: "/guide/parameter-overrides" },
            { text: "Child Instance Access", link: "/guide/hierarchy" },
            { text: "VCD Waveform Output", link: "/guide/vcd" },
            { text: "Type Conversion", link: "/guide/type-conversion" },
          ],
        },
        {
          text: "Project Configuration",
          items: [
            { text: "Vite Plugin", link: "/guide/vite-plugin" },
            { text: "celox.toml", link: "/guide/celox-toml" },
          ],
        },
        {
          text: "Advanced",
          collapsed: true,
          items: [
            { text: "Combinational Loops", link: "/guide/combinational-loops" },
            { text: "Dead Store Elimination", link: "/guide/dead-store-elimination" },
            { text: "Optimization Tuning", link: "/guide/optimization-tuning" },
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
            {
              text: "Compiler Components",
              link: "/internals/compiler-crate-architecture",
            },
            { text: "IR Reference", link: "/internals/ir-reference" },
            { text: "Optimizations", link: "/internals/optimizations" },
            { text: "4-State Simulation", link: "/internals/four-state" },
            {
              text: "Combinational Analysis",
              link: "/internals/combinational-analysis",
            },
            {
              text: "Runtime Semantics",
              link: "/internals/cascade-limitations",
            },
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
