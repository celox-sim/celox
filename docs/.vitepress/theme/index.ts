import DefaultTheme from "vitepress/theme";
import type { Theme } from "vitepress";
import BenchmarkDashboard from "../components/BenchmarkDashboard.vue";

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component("BenchmarkDashboard", BenchmarkDashboard);
  },
} satisfies Theme;
