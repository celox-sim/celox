import { defineConfig } from "vitepress";

export default defineConfig({
  title: "Celox",
  description: "JIT simulator for Veryl HDL",
  base: "/celox/",

  appearance: "dark",

  themeConfig: {
    nav: [
      { text: "Guide", link: "/guide/introduction" },
      { text: "Internals", link: "/internals/architecture" },
      {
        text: "GitHub",
        link: "https://github.com/tignear/celox",
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
          ],
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
      { icon: "github", link: "https://github.com/tignear/celox" },
    ],
  },
});
