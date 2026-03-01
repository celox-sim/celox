<script setup lang="ts">
import { ref, computed, onMounted, watch } from "vue";
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
  date: number;
  tool: string;
  benches: {
    name: string;
    value: number;
    unit: string;
    range?: string;
    extra?: string;
  }[];
}

interface BenchData {
  entries: Record<string, BenchEntry[]>;
}

interface SeriesPoint {
  date: number;
  commit: string;
  commitUrl: string;
  value: number;
}

interface Series {
  /** Unique key: "rust/simulation_tick_..." or "ts/simulation_tick_..." */
  key: string;
  benchName: string;
  runtime: "rust" | "ts" | "unknown";
  category: string;
  points: SeriesPoint[];
}

// --- Category definitions ---

const categories = [
  {
    key: "build",
    label: "Build",
    match: (n: string) => n.includes("simulation_build"),
  },
  {
    key: "tick",
    label: "Tick",
    match: (n: string) => /^simulation_tick_/.test(n),
  },
  {
    key: "testbench",
    label: "Testbench",
    match: (n: string) => /^testbench_(array_)?tick_/.test(n),
  },
  {
    key: "overhead",
    label: "Overhead",
    match: (n: string) => /^simulator_tick_|^simulation_step_/.test(n),
  },
  { key: "other", label: "Other", match: () => true },
] as const;

// --- Color palette ---

const RUST_COLORS = [
  "#3b82f6",
  "#6366f1",
  "#8b5cf6",
  "#0ea5e9",
  "#06b6d4",
  "#2563eb",
];
const TS_COLORS = [
  "#22c55e",
  "#10b981",
  "#14b8a6",
  "#84cc16",
  "#a3e635",
  "#34d399",
];
const DASH_PATTERNS = [[], [6, 3], [2, 2], [8, 4, 2, 4], [4, 4]];

// --- State ---

const loading = ref(true);
const error = ref("");
const rawData = ref<BenchData | null>(null);
const selected = ref(new Set<string>());
const selectorOpen = ref(true);

// --- Helpers ---

function stripPrefix(name: string): string {
  return name.replace(/^(rust|ts)\//, "");
}

function runtime(name: string): "rust" | "ts" | "unknown" {
  if (name.startsWith("rust/")) return "rust";
  if (name.startsWith("ts/")) return "ts";
  return "unknown";
}

function toMicroseconds(value: number, unit: string): number {
  const u = unit.toLowerCase().trim();
  if (u === "ns/iter" || u === "ns") return value / 1000;
  if (u === "ms") return value * 1000;
  return value;
}

function formatUs(us: number): string {
  if (us < 1) return `${(us * 1000).toFixed(1)} ns`;
  if (us < 1000) return `${us.toFixed(2)} µs`;
  if (us < 1_000_000) return `${(us / 1000).toFixed(2)} ms`;
  return `${(us / 1_000_000).toFixed(2)} s`;
}

function shortDate(epoch: number): string {
  const d = new Date(epoch);
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

/** Human-friendly display label */
function displayLabel(s: Series): string {
  const prefix = s.runtime === "rust" ? "Rust" : s.runtime === "ts" ? "TS" : "";
  return prefix ? `${prefix} / ${s.benchName}` : s.benchName;
}

// --- Computed: all series ---

const allSeries = computed<Series[]>(() => {
  if (!rawData.value) return [];

  const result: Series[] = [];

  for (const [, entries] of Object.entries(rawData.value.entries)) {
    const map = new Map<string, SeriesPoint[]>();

    for (const entry of entries) {
      for (const b of entry.benches) {
        if (!map.has(b.name)) map.set(b.name, []);
        map.get(b.name)!.push({
          date: entry.date,
          commit: entry.commit.id.slice(0, 7),
          commitUrl: entry.commit.url,
          value: toMicroseconds(b.value, b.unit),
        });
      }
    }

    for (const [name, points] of map) {
      const stripped = stripPrefix(name);
      const rt = runtime(name);
      let cat = "other";
      for (const c of categories) {
        if (c.key !== "other" && c.match(stripped)) {
          cat = c.key;
          break;
        }
      }

      result.push({
        key: name,
        benchName: stripped,
        runtime: rt,
        category: cat,
        points: points.sort((a, b) => a.date - b.date),
      });
    }
  }

  // Sort: by category order, then by name, then rust before ts
  const catOrder = Object.fromEntries(categories.map((c, i) => [c.key, i]));
  result.sort((a, b) => {
    const co = (catOrder[a.category] ?? 99) - (catOrder[b.category] ?? 99);
    if (co !== 0) return co;
    const nc = a.benchName.localeCompare(b.benchName);
    if (nc !== 0) return nc;
    return a.runtime.localeCompare(b.runtime);
  });

  return result;
});

/** Series grouped by category for the selector UI */
const seriesByCategory = computed(() => {
  const groups: { key: string; label: string; items: Series[] }[] = [];
  for (const cat of categories) {
    const items = allSeries.value.filter((s) => s.category === cat.key);
    if (items.length > 0) {
      groups.push({
        key: cat.key,
        label: cat.label,
        items,
      });
    }
  }
  return groups;
});

// --- Selection helpers ---

function toggle(key: string) {
  const s = new Set(selected.value);
  if (s.has(key)) s.delete(key);
  else s.add(key);
  selected.value = s;
}

function toggleCategory(catKey: string) {
  const group = seriesByCategory.value.find((g) => g.key === catKey);
  if (!group) return;
  const keys = group.items.map((s) => s.key);
  const allSelected = keys.every((k) => selected.value.has(k));
  const s = new Set(selected.value);
  if (allSelected) {
    keys.forEach((k) => s.delete(k));
  } else {
    keys.forEach((k) => s.add(k));
  }
  selected.value = s;
}

function isCategoryAllSelected(catKey: string): boolean {
  const group = seriesByCategory.value.find((g) => g.key === catKey);
  if (!group || group.items.length === 0) return false;
  return group.items.every((s) => selected.value.has(s.key));
}

function isCategoryPartial(catKey: string): boolean {
  const group = seriesByCategory.value.find((g) => g.key === catKey);
  if (!group || group.items.length === 0) return false;
  const count = group.items.filter((s) => selected.value.has(s.key)).length;
  return count > 0 && count < group.items.length;
}

function selectOnly(catKey: string) {
  const group = seriesByCategory.value.find((g) => g.key === catKey);
  if (!group) return;
  selected.value = new Set(group.items.map((s) => s.key));
}

function selectAll() {
  selected.value = new Set(allSeries.value.map((s) => s.key));
}

function selectNone() {
  selected.value = new Set();
}

// --- Computed: chart data from selected series ---

const selectedSeries = computed(() =>
  allSeries.value.filter((s) => selected.value.has(s.key)),
);

const chartData = computed(() => {
  const series = selectedSeries.value;
  if (series.length === 0) return null;

  // Collect all unique dates
  const dateSet = new Set<number>();
  for (const s of series) {
    for (const p of s.points) dateSet.add(p.date);
  }
  const dates = [...dateSet].sort((a, b) => a - b);
  const labels = dates.map((d) => shortDate(d));

  // Assign colors: index per runtime
  const rustIdx = { n: 0 };
  const tsIdx = { n: 0 };

  const datasets = series.map((s) => {
    let color: string;
    let idx: number;
    if (s.runtime === "rust") {
      idx = rustIdx.n++;
      color = RUST_COLORS[idx % RUST_COLORS.length];
    } else {
      idx = tsIdx.n++;
      color = TS_COLORS[idx % TS_COLORS.length];
    }

    const dateToValue = new Map(s.points.map((p) => [p.date, p.value]));
    return {
      label: displayLabel(s),
      data: dates.map((d) => dateToValue.get(d) ?? null),
      borderColor: color,
      backgroundColor: color + "1a",
      borderDash: DASH_PATTERNS[idx % DASH_PATTERNS.length],
      tension: 0.3,
      pointRadius: 2,
    };
  });

  return { labels, datasets };
});

const chartOptions = computed(() => ({
  responsive: true,
  maintainAspectRatio: false,
  interaction: {
    mode: "index" as const,
    intersect: false,
  },
  plugins: {
    legend: {
      labels: { color: "#e5e7eb" },
    },
    tooltip: {
      callbacks: {
        label: (ctx: any) =>
          `${ctx.dataset.label}: ${formatUs(ctx.parsed.y)}`,
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
}));

// --- Fetch data ---

onMounted(async () => {
  try {
    const res = await fetch("/celox/dev/bench/data.js");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const text = await res.text();
    const jsonStr = text
      .replace(/^window\.BENCHMARK_DATA\s*=\s*/, "")
      .replace(/;\s*$/, "");
    rawData.value = JSON.parse(jsonStr);
  } catch (e: any) {
    error.value = e.message || "Failed to load benchmark data";
  } finally {
    loading.value = false;
  }
});

// Default selection: first category
watch(
  seriesByCategory,
  (groups) => {
    if (groups.length > 0 && selected.value.size === 0) {
      selected.value = new Set(groups[0].items.map((s) => s.key));
    }
  },
  { immediate: true },
);
</script>

<template>
  <div class="bench-dashboard">
    <!-- Loading / Error -->
    <div v-if="loading" class="bench-status">Loading benchmark data...</div>
    <div v-else-if="error" class="bench-status bench-error">
      <p>Could not load benchmark data: {{ error }}</p>
      <p>
        Data is published by CI to
        <a href="https://celox-sim.github.io/celox/dev/bench/"
          >the external dashboard</a
        >. It may not be available in local dev mode.
      </p>
    </div>

    <template v-else>
      <!-- Preset buttons -->
      <div class="bench-presets">
        <button
          v-for="group in seriesByCategory"
          :key="group.key"
          :class="[
            'bench-preset',
            { active: isCategoryAllSelected(group.key) },
          ]"
          @click="selectOnly(group.key)"
          :title="`Show only ${group.label}`"
        >
          {{ group.label }}
        </button>
        <button class="bench-preset" @click="selectAll()">All</button>
        <button class="bench-preset" @click="selectNone()">None</button>
      </div>

      <!-- Series selector -->
      <details class="bench-selector" :open="selectorOpen || undefined">
        <summary @click.prevent="selectorOpen = !selectorOpen">
          Series ({{ selected.size }} / {{ allSeries.length }})
        </summary>
        <div class="bench-selector-body">
          <div
            v-for="group in seriesByCategory"
            :key="group.key"
            class="bench-cat-group"
          >
            <label class="bench-cat-header" @click.prevent="toggleCategory(group.key)">
              <input
                type="checkbox"
                :checked="isCategoryAllSelected(group.key)"
                :indeterminate="isCategoryPartial(group.key)"
                @click.prevent
              />
              {{ group.label }}
            </label>
            <div class="bench-cat-items">
              <label
                v-for="s in group.items"
                :key="s.key"
                class="bench-series-label"
              >
                <input
                  type="checkbox"
                  :checked="selected.has(s.key)"
                  @change="toggle(s.key)"
                />
                <span
                  class="bench-runtime-badge"
                  :class="s.runtime"
                >{{ s.runtime === "rust" ? "R" : s.runtime === "ts" ? "T" : "?" }}</span>
                {{ s.benchName }}
              </label>
            </div>
          </div>
        </div>
      </details>

      <!-- Chart -->
      <div v-if="chartData && chartData.datasets.length > 0" class="bench-chart-wrapper">
        <Line :data="chartData" :options="chartOptions as any" />
      </div>
      <div v-else class="bench-status">
        Select one or more series to display.
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

/* --- Presets --- */

.bench-presets {
  display: flex;
  gap: 0.4rem;
  flex-wrap: wrap;
  margin-bottom: 0.75rem;
}

.bench-preset {
  padding: 0.3rem 0.75rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  background: transparent;
  color: var(--vp-c-text-2);
  cursor: pointer;
  font-size: 0.85rem;
  transition: all 0.15s;
}

.bench-preset:hover {
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-brand-1);
}

.bench-preset.active {
  background: var(--vp-c-brand-1);
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-white);
}

/* --- Series selector --- */

.bench-selector {
  margin-bottom: 1rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  overflow: hidden;
}

.bench-selector summary {
  padding: 0.5rem 0.75rem;
  cursor: pointer;
  font-weight: 600;
  font-size: 0.9rem;
  color: var(--vp-c-text-1);
  user-select: none;
}

.bench-selector-body {
  padding: 0.5rem 0.75rem 0.75rem;
  display: flex;
  flex-wrap: wrap;
  gap: 1rem;
}

.bench-cat-group {
  min-width: 200px;
  flex: 1;
}

.bench-cat-header {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  font-weight: 600;
  font-size: 0.85rem;
  color: var(--vp-c-text-1);
  cursor: pointer;
  margin-bottom: 0.25rem;
}

.bench-cat-items {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  padding-left: 0.25rem;
}

.bench-series-label {
  display: flex;
  align-items: center;
  gap: 0.35rem;
  font-size: 0.8rem;
  color: var(--vp-c-text-2);
  cursor: pointer;
  line-height: 1.6;
}

.bench-series-label:hover {
  color: var(--vp-c-text-1);
}

.bench-runtime-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.1rem;
  height: 1.1rem;
  border-radius: 3px;
  font-size: 0.65rem;
  font-weight: 700;
  flex-shrink: 0;
}

.bench-runtime-badge.rust {
  background: rgba(59, 130, 246, 0.2);
  color: #60a5fa;
}

.bench-runtime-badge.ts {
  background: rgba(34, 197, 94, 0.2);
  color: #4ade80;
}

/* --- Chart --- */

.bench-chart-wrapper {
  position: relative;
  height: 400px;
  padding: 1rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
}
</style>
