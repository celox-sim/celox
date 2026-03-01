<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
import { Line } from "vue-chartjs";
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  PointElement,
  LineElement,
  Title,
  Tooltip,
  Legend,
} from "chart.js";

ChartJS.register(
  CategoryScale,
  LinearScale,
  PointElement,
  LineElement,
  Title,
  Tooltip,
  Legend,
);

// --- Types ---

interface BenchEntry {
  commit: { id: string; message: string; timestamp: string; url: string };
  date: number; // epoch ms
  tool: string;
  benches: { name: string; value: number; unit: string; range?: string; extra?: string }[];
}

interface BenchData {
  entries: Record<string, BenchEntry[]>;
}

// --- Category definitions ---

const categories = [
  { key: "build", label: "Build", match: (n: string) => n.includes("simulation_build") },
  { key: "tick", label: "Tick", match: (n: string) => /^simulation_tick_/.test(n) },
  { key: "testbench", label: "Testbench", match: (n: string) => /^testbench_(array_)?tick_/.test(n) },
  { key: "overhead", label: "Overhead", match: (n: string) => /^simulator_tick_|^simulation_step_/.test(n) },
  { key: "ts_extras", label: "TS Extras", match: (_n: string) => true }, // fallback
] as const;

// --- State ---

const loading = ref(true);
const error = ref("");
const activeTab = ref("build");
const rawData = ref<BenchData | null>(null);

// --- Helpers ---

/** Strip rust/ or ts/ prefix */
function stripPrefix(name: string): string {
  return name.replace(/^(rust|ts)\//, "");
}

/** Detect runtime from prefixed name */
function runtime(name: string): "rust" | "ts" | "unknown" {
  if (name.startsWith("rust/")) return "rust";
  if (name.startsWith("ts/")) return "ts";
  return "unknown";
}

/** Normalize a value to µs based on its unit string */
function toMicroseconds(value: number, unit: string): number {
  const u = unit.toLowerCase().trim();
  if (u === "ns/iter" || u === "ns") return value / 1000;
  if (u === "ms") return value * 1000;
  // "us" or "µs" — already in µs
  return value;
}

/** Format µs value to human-readable string with adaptive unit */
function formatUs(us: number): string {
  if (us < 1) return `${(us * 1000).toFixed(1)} ns`;
  if (us < 1000) return `${us.toFixed(2)} µs`;
  if (us < 1_000_000) return `${(us / 1000).toFixed(2)} ms`;
  return `${(us / 1_000_000).toFixed(2)} s`;
}

/** Short date from timestamp */
function shortDate(epoch: number): string {
  const d = new Date(epoch);
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

// --- Computed: parsed series grouped by category ---

interface SeriesPoint {
  date: number;
  commit: string;
  commitUrl: string;
  value: number; // µs
}

interface Series {
  benchName: string; // stripped
  runtime: "rust" | "ts" | "unknown";
  points: SeriesPoint[];
}

const seriesByCategory = computed(() => {
  if (!rawData.value) return {};

  const allSeries: Series[] = [];

  for (const [, entries] of Object.entries(rawData.value.entries)) {
    // Build per-bench-name time series
    const map = new Map<string, SeriesPoint[]>();

    for (const entry of entries) {
      for (const b of entry.benches) {
        const key = b.name; // prefixed name
        if (!map.has(key)) map.set(key, []);
        map.get(key)!.push({
          date: entry.date,
          commit: entry.commit.id.slice(0, 7),
          commitUrl: entry.commit.url,
          value: toMicroseconds(b.value, b.unit),
        });
      }
    }

    for (const [name, points] of map) {
      allSeries.push({
        benchName: stripPrefix(name),
        runtime: runtime(name),
        points: points.sort((a, b) => a.date - b.date),
      });
    }
  }

  // Group into categories
  const result: Record<string, Series[]> = {};

  const assigned = new Set<Series>();

  for (const cat of categories) {
    const matching: Series[] = [];
    for (const s of allSeries) {
      if (assigned.has(s)) continue;
      if (cat.key === "ts_extras") {
        // Fallback: only unassigned TS benchmarks
        if (s.runtime === "ts") {
          matching.push(s);
          assigned.add(s);
        }
      } else if (cat.match(s.benchName)) {
        matching.push(s);
        assigned.add(s);
      }
    }
    if (matching.length > 0) {
      result[cat.key] = matching;
    }
  }

  return result;
});

const availableTabs = computed(() => {
  return categories.filter((c) => seriesByCategory.value[c.key]);
});

const currentSeries = computed(() => {
  return seriesByCategory.value[activeTab.value] ?? [];
});

// --- Group series by stripped bench name for overlay charts ---

interface ChartGroup {
  benchName: string;
  labels: string[];
  datasets: {
    label: string;
    data: (number | null)[];
    borderColor: string;
    backgroundColor: string;
    tension: number;
    pointRadius: number;
  }[];
}

const chartGroups = computed<ChartGroup[]>(() => {
  const grouped = new Map<string, Series[]>();
  for (const s of currentSeries.value) {
    if (!grouped.has(s.benchName)) grouped.set(s.benchName, []);
    grouped.get(s.benchName)!.push(s);
  }

  const result: ChartGroup[] = [];
  for (const [benchName, seriesList] of grouped) {
    // Collect all unique dates across runtimes
    const dateSet = new Set<number>();
    for (const s of seriesList) {
      for (const p of s.points) dateSet.add(p.date);
    }
    const dates = [...dateSet].sort((a, b) => a - b);
    const labels = dates.map((d) => shortDate(d));

    const datasets = seriesList.map((s) => {
      const dateToValue = new Map(s.points.map((p) => [p.date, p.value]));
      return {
        label: s.runtime === "rust" ? "Rust" : s.runtime === "ts" ? "TypeScript" : benchName,
        data: dates.map((d) => dateToValue.get(d) ?? null),
        borderColor: s.runtime === "rust" ? "#3b82f6" : "#22c55e",
        backgroundColor: s.runtime === "rust" ? "rgba(59,130,246,0.1)" : "rgba(34,197,94,0.1)",
        tension: 0.3,
        pointRadius: 2,
      };
    });

    result.push({ benchName, labels, datasets });
  }

  return result;
});

function chartOptions(benchName: string) {
  return {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      title: {
        display: true,
        text: benchName,
        color: "#e5e7eb",
      },
      legend: {
        labels: { color: "#e5e7eb" },
      },
      tooltip: {
        callbacks: {
          label: (ctx: any) => `${ctx.dataset.label}: ${formatUs(ctx.parsed.y)}`,
        },
      },
    },
    scales: {
      x: {
        ticks: { color: "#9ca3af" },
        grid: { color: "rgba(255,255,255,0.06)" },
      },
      y: {
        ticks: {
          color: "#9ca3af",
          callback: (v: number) => formatUs(v),
        },
        grid: { color: "rgba(255,255,255,0.06)" },
      },
    },
    spanGaps: true,
  };
}

// --- Fetch data ---

onMounted(async () => {
  try {
    const res = await fetch("/celox/dev/bench/data.js");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const text = await res.text();
    // data.js assigns: window.BENCHMARK_DATA = { ... }
    const jsonStr = text.replace(/^window\.BENCHMARK_DATA\s*=\s*/, "").replace(/;\s*$/, "");
    rawData.value = JSON.parse(jsonStr);

    // Default to first available tab
    if (availableTabs.value.length > 0) {
      activeTab.value = availableTabs.value[0].key;
    }
  } catch (e: any) {
    error.value = e.message || "Failed to load benchmark data";
  } finally {
    loading.value = false;
  }
});
</script>

<template>
  <div class="bench-dashboard">
    <!-- Loading / Error states -->
    <div v-if="loading" class="bench-status">Loading benchmark data...</div>
    <div v-else-if="error" class="bench-status bench-error">
      <p>Could not load benchmark data: {{ error }}</p>
      <p>
        Data is published by CI to
        <a href="https://celox-sim.github.io/celox/dev/bench/">the external dashboard</a>.
        It may not be available in local dev mode.
      </p>
    </div>

    <!-- Dashboard -->
    <template v-else>
      <!-- Tabs -->
      <div class="bench-tabs">
        <button
          v-for="tab in availableTabs"
          :key="tab.key"
          :class="['bench-tab', { active: activeTab === tab.key }]"
          @click="activeTab = tab.key"
        >
          {{ tab.label }}
        </button>
      </div>

      <!-- Charts -->
      <div v-if="chartGroups.length === 0" class="bench-status">
        No data for this category.
      </div>
      <div v-for="group in chartGroups" :key="group.benchName" class="bench-chart-wrapper">
        <Line
          :data="{ labels: group.labels, datasets: group.datasets }"
          :options="chartOptions(group.benchName) as any"
        />
      </div>
    </template>
  </div>
</template>

<style scoped>
.bench-dashboard {
  margin-top: 1rem;
}

.bench-status {
  padding: 2rem;
  text-align: center;
  color: var(--vp-c-text-2);
}

.bench-error {
  color: var(--vp-c-danger-1);
}

.bench-tabs {
  display: flex;
  gap: 0.5rem;
  flex-wrap: wrap;
  margin-bottom: 1.5rem;
}

.bench-tab {
  padding: 0.4rem 1rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  background: transparent;
  color: var(--vp-c-text-2);
  cursor: pointer;
  font-size: 0.9rem;
  transition: all 0.2s;
}

.bench-tab:hover {
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-brand-1);
}

.bench-tab.active {
  background: var(--vp-c-brand-1);
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-white);
}

.bench-chart-wrapper {
  position: relative;
  height: 350px;
  margin-bottom: 2rem;
  padding: 1rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
}
</style>
