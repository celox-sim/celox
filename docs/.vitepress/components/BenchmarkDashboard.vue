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
  key: string;
  benchName: string;
  runtime: "rust" | "rust-dse" | "native-tb" | "ts" | "verilator" | "unknown";
  points: SeriesPoint[];
}

interface ChartCard {
  benchName: string;
  title: string;
  series: Series[];
}

interface TabSection {
  label: string;
  cards: ChartCard[];
}

// --- Tab definitions ---

interface TabDef {
  key: string;
  label: string;
  match: (name: string) => boolean;
  sections: (cards: ChartCard[]) => TabSection[];
}

const props = withDefaults(defineProps<{ mode?: "overview" | "diagnostic" }>(), {
  mode: "overview",
});

const PRIMARY_COUNTER_BENCHES = new Set([
  "simulation_build_top_n1000",
  "simulation_tick_top_n1000_x1000000",
  "testbench_tick_top_n1000_x1000000",
]);

const PRIMARY_STDLIB_BENCHES = new Set([
  "simulation_eval_linear_sec_p6_x1000000",
  "simulation_eval_countones_w64_x1000000",
  "simulation_eval_onehot_w64_x1000000",
  "simulation_tick_std_counter_w32_x1000000",
  "simulation_tick_fifo_w8_d16_x1000000",
]);

function stdlibSections(cards: ChartCard[]): TabSection[] {
  const groups: Record<string, ChartCard[]> = {
    "General Comb: countones": [],
    "Comb + Liveness: onehot": [],
    "Structured Datapath: linear_sec": [],
    "Sequential Runtime: std::counter": [],
    "Sequential Runtime: std::fifo": [],
  };

  for (const card of cards) {
    if (card.benchName.includes("linear_sec")) {
      groups["Structured Datapath: linear_sec"].push(card);
    } else if (card.benchName.includes("countones")) {
      groups["General Comb: countones"].push(card);
    } else if (card.benchName.includes("std_counter")) {
      groups["Sequential Runtime: std::counter"].push(card);
    } else if (card.benchName.includes("fifo")) {
      groups["Sequential Runtime: std::fifo"].push(card);
    } else if (card.benchName.includes("onehot")) {
      groups["Comb + Liveness: onehot"].push(card);
    }
  }

  return Object.entries(groups)
    .filter(([, c]) => c.length > 0)
    .map(([label, c]) => ({ label, cards: c }));
}

function singleSection(cards: ChartCard[]): TabSection[] {
  return cards.length > 0 ? [{ label: "", cards }] : [];
}

const overviewTabs: TabDef[] = [
  {
    key: "counter",
    label: "Counter",
    match: (n) => n.includes("top_n1000"),
    sections: singleSection,
  },
  {
    key: "stdlib",
    label: "Std Library",
    match: (n) => /linear_sec|countones|std_counter|fifo|onehot/.test(n),
    sections: stdlibSections,
  },
];

function apiSections(cards: ChartCard[]): TabSection[] {
  const groups: Record<string, ChartCard[]> = {
    Overhead: [],
    "Time-based Simulation": [],
    "Testbench Helpers": [],
  };

  for (const card of cards) {
    if (/^simulator_tick_|^simulation_step_/.test(card.benchName)) {
      groups["Overhead"].push(card);
    } else if (/^simulation_time_|^runUntil/.test(card.benchName)) {
      groups["Time-based Simulation"].push(card);
    } else {
      groups["Testbench Helpers"].push(card);
    }
  }

  return Object.entries(groups)
    .filter(([, c]) => c.length > 0)
    .map(([label, c]) => ({ label, cards: c }));
}

const diagnosticTabs: TabDef[] = [
  ...overviewTabs,
  {
    key: "api",
    label: "API",
    match: (n) =>
      /^simulator_tick_|^simulation_step_|^simulation_time_|^waitForCycles|^manual_step|^runUntil/.test(n),
    sections: apiSections,
  },
  {
    key: "optimize",
    label: "Optimize",
    match: (n) => n.includes("optimize"),
    sections: singleSection,
  },
];

const tabs = computed(() => (props.mode === "diagnostic" ? diagnosticTabs : overviewTabs));

function classifySeries(benchName: string): string {
  const orderedTabs = tabs.value.length > 2
    ? [tabs.value[2], tabs.value[3], tabs.value[1], tabs.value[0]]
    : [tabs.value[1], tabs.value[0]];

  for (const tab of orderedTabs) {
    if (tab.match(benchName)) return tab.key;
  }
  return "hidden";
}

// --- Color palette ---

const RUNTIME_COLORS: Record<string, string> = {
  rust: "#3b82f6",
  "rust-dse": "#a855f7",
  "native-tb": "#f43f5e",
  ts: "#22c55e",
  verilator: "#f97316",
  unknown: "#9ca3af",
};

const RUNTIME_LABELS: Record<string, string> = {
  rust: "Celox (rust)",
  "rust-dse": "Celox (rust-dse)",
  "native-tb": "Celox (native-tb)",
  ts: "Celox (ts)",
  verilator: "Verilator",
  unknown: "Unknown",
};

// --- State ---

const loading = ref(true);
const error = ref("");
const rawData = ref<BenchData | null>(null);
const activeTab = ref("counter");

// --- Helpers ---

function stripPrefix(name: string): string {
  return name.replace(/^(rust-dse|rust|ts|verilator)\//, "");
}

/** Normalize variant-specific benchmark names so they merge into the same chart cards */
function normalizeBenchName(benchName: string): string {
  if (benchName.startsWith("native_tb_")) {
    return benchName
      .replace(/^native_tb_build_/, "simulation_build_")
      .replace(/^native_tb_raw_tick_/, "simulation_tick_")
      .replace(/_counter_n(\d+)/, "_top_n$1");
  }

  if (benchName.startsWith("dse_")) {
    return benchName
      .replace(/^dse_build_/, "simulation_build_")
      .replace(/^dse_eval_/, "simulation_eval_")
      .replace(/^dse_tick_/, "simulation_tick_")
      .replace(/^dse_testbench_eval_/, "testbench_eval_")
      .replace(/^dse_testbench_tick_/, "testbench_tick_");
  }

  return benchName;
}

function runtime(name: string): "rust" | "rust-dse" | "native-tb" | "ts" | "verilator" | "unknown" {
  if (name.startsWith("rust-dse/")) return "rust-dse";
  if (name.startsWith("rust/") && stripPrefix(name).startsWith("native_tb_")) return "native-tb";
  if (name.startsWith("rust/")) return "rust";
  if (name.startsWith("ts/")) return "ts";
  if (name.startsWith("verilator/")) return "verilator";
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

/** Format chart title from benchName */
function formatChartTitle(benchName: string): string {
  let s = benchName;

  // Strip module identifiers for stdlib
  s = s.replace(/^linear_sec_p6_/, "");
  s = s.replace(/^countones_w64_/, "");
  s = s.replace(/^std_counter_w32_/, "");
  s = s.replace(/^std_gray_counter_w32_/, "");
  s = s.replace(/^fifo_w8_d16_/, "");
  s = s.replace(/^gray_codec_w32_/, "");
  s = s.replace(/^edge_detector_w32_/, "");
  s = s.replace(/^onehot_w64_/, "");
  s = s.replace(/^lfsr_w32_/, "");
  // Strip counter workload identifiers
  s = s.replace(/_top_n1000/, "");
  // Strip optimize prefix patterns
  s = s.replace(/^optimize_/, "");

  // Replace operation names (longer prefixes first to avoid partial matches)
  s = s.replace(/^native_tb_run/, "Native TB run");
  s = s.replace(/^native_tb_exec/, "Native TB exec");
  s = s.replace(/^simulation_time_build/, "Time build");
  s = s.replace(/^simulation_time_tick/, "Time tick");
  s = s.replace(/^simulation_build/, "Build");
  s = s.replace(/^simulation_eval/, "Eval");
  s = s.replace(/^simulation_tick/, "Tick");
  s = s.replace(/^simulation_step/, "Step");
  s = s.replace(/^simulator_tick/, "Simulator tick");
  s = s.replace(/^testbench_array_tick/, "Testbench (array) tick");
  s = s.replace(/^testbench_eval/, "Testbench eval");
  s = s.replace(/^testbench_tick/, "Testbench tick");
  s = s.replace(/^manual_step/, "Manual step");
  s = s.replace(/^waitForCycles/, "waitForCycles");
  s = s.replace(/^runUntil/, "runUntil");

  // Format iteration counts: _x1000000 → ×1M, _x1000 → ×1K, _x1 → ×1
  s = s.replace(/_x(\d+)$/, (_, n) => {
    const num = parseInt(n, 10);
    if (num >= 1_000_000) return ` ×${num / 1_000_000}M`;
    if (num >= 1_000) return ` ×${num / 1_000}K`;
    return ` ×${num}`;
  });

  // Clean up remaining underscores
  s = s.replace(/_/g, " ");

  // Capitalize first letter
  s = s.charAt(0).toUpperCase() + s.slice(1);

  return s;
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
      result.push({
        key: name,
        benchName: normalizeBenchName(stripPrefix(name)),
        runtime: runtime(name),
        points: points.sort((a, b) => a.date - b.date),
      });
    }
  }

  return result;
});

function isPrimaryBench(benchName: string): boolean {
  return PRIMARY_COUNTER_BENCHES.has(benchName) || PRIMARY_STDLIB_BENCHES.has(benchName);
}

// --- Computed: tabs with chart cards ---

const tabData = computed(() => {
  const seriesList =
    props.mode === "diagnostic"
      ? allSeries.value
      : allSeries.value.filter((s) => isPrimaryBench(s.benchName));
  if (seriesList.length === 0) return new Map<string, TabSection[]>();

  // Group all series by tab
  const tabSeries = new Map<string, Series[]>();
  for (const tab of tabs.value) {
    tabSeries.set(tab.key, []);
  }

  for (const s of seriesList) {
    const tabKey = classifySeries(s.benchName);
    if (tabKey === "hidden") continue;
    tabSeries.get(tabKey)?.push(s);
  }

  // Within each tab, group series by benchName into chart cards
  const result = new Map<string, TabSection[]>();

  for (const tab of tabs.value) {
    const tabSeriesList = tabSeries.get(tab.key) ?? [];

    // Group by benchName
    const byBenchName = new Map<string, Series[]>();
    for (const s of tabSeriesList) {
      if (!byBenchName.has(s.benchName)) byBenchName.set(s.benchName, []);
      byBenchName.get(s.benchName)!.push(s);
    }

    // Create chart cards sorted alphabetically
    const cards: ChartCard[] = [...byBenchName.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([benchName, series]) => ({
        benchName,
        title: formatChartTitle(benchName),
        series: series.sort((a, b) => a.runtime.localeCompare(b.runtime)),
      }));

    result.set(tab.key, tab.sections(cards));
  }

  return result;
});

/** Which tabs actually have data */
const availableTabs = computed(() =>
  tabs.value.filter((t) => {
    const sections = tabData.value.get(t.key);
    return sections && sections.some((s) => s.cards.length > 0);
  }),
);

/** Active tab's sections */
const activeSections = computed(() =>
  tabData.value.get(activeTab.value) ?? [],
);

// --- Chart data builder per card ---

function buildChartData(card: ChartCard) {
  const dateSet = new Set<number>();
  for (const s of card.series) {
    for (const p of s.points) dateSet.add(p.date);
  }
  const dates = [...dateSet].sort((a, b) => a - b);
  const labels = dates.map((d) => shortDate(d));

  const datasets = card.series.map((s) => {
    const color = RUNTIME_COLORS[s.runtime];
    const dateToValue = new Map(s.points.map((p) => [p.date, p.value]));
    return {
      label: RUNTIME_LABELS[s.runtime] ?? s.runtime,
      data: dates.map((d) => dateToValue.get(d) ?? null),
      borderColor: color,
      backgroundColor: color + "1a",
      tension: 0.3,
      pointRadius: 2,
    };
  });

  return { labels, datasets };
}

function makeChartOptions() {
  return {
    responsive: true,
    maintainAspectRatio: false,
    interaction: {
      mode: "index" as const,
      intersect: false,
    },
    plugins: {
      legend: {
        labels: { color: "#e5e7eb", boxWidth: 12, padding: 8, font: { size: 11 } },
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
        ticks: { color: "#9ca3af", font: { size: 10 } },
        grid: { color: "rgba(255,255,255,0.06)" },
      },
      y: {
        ticks: {
          color: "#9ca3af",
          font: { size: 10 },
          callback: (v: number) => formatUs(v),
        },
        grid: { color: "rgba(255,255,255,0.06)" },
      },
    },
    spanGaps: true,
  };
}

const chartOptions = makeChartOptions();

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

  // Default to first tab that has data
  if (availableTabs.value.length > 0) {
    activeTab.value = availableTabs.value[0].key;
  }
});
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
      <!-- Tab bar -->
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

      <!-- Tab content: sections with chart card grids -->
      <div
        v-if="activeSections.length > 0"
        :class="['bench-sections', { 'bench-sections-columns': activeTab === 'stdlib' }]"
      >
        <div
          v-for="section in activeSections"
          :key="section.label"
          :class="['bench-section', { 'bench-section-column': activeTab === 'stdlib' }]"
        >
          <h3 v-if="section.label" class="bench-section-title">
            {{ section.label }}
          </h3>
          <div
            :class="[
              activeTab === 'stdlib' ? 'bench-column-cards' : 'bench-grid',
            ]"
          >
            <div
              v-for="card in section.cards"
              :key="card.benchName"
              class="bench-card"
            >
              <div class="bench-card-title">{{ card.title }}</div>
              <div class="bench-card-chart">
                <Line
                  :data="buildChartData(card)"
                  :options="(chartOptions as any)"
                />
              </div>
            </div>
          </div>
        </div>
      </div>
      <div v-else class="bench-status">
        No benchmark data for this category.
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

/* --- Tab bar --- */

.bench-tabs {
  display: flex;
  gap: 0;
  border-bottom: 2px solid var(--vp-c-divider);
  margin-bottom: 1.25rem;
}

.bench-tab {
  padding: 0.5rem 1.25rem;
  border: none;
  background: transparent;
  color: var(--vp-c-text-2);
  cursor: pointer;
  font-size: 0.9rem;
  font-weight: 500;
  border-bottom: 2px solid transparent;
  margin-bottom: -2px;
  transition: color 0.15s, border-color 0.15s;
}

.bench-tab:hover {
  color: var(--vp-c-brand-1);
}

.bench-tab.active {
  color: var(--vp-c-brand-1);
  border-bottom-color: var(--vp-c-brand-1);
}

/* --- Sections --- */

.bench-sections {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}

.bench-sections-columns {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 1rem;
  align-items: start;
}

.bench-section-column {
  min-width: 0;
}

.bench-section-title {
  font-size: 0.95rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin: 0 0 0.75rem 0;
  padding-bottom: 0.35rem;
  border-bottom: 1px solid var(--vp-c-divider);
}

.bench-column-cards {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

/* --- Card grid --- */

.bench-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(380px, 1fr));
  gap: 1rem;
}

.bench-card {
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  padding: 0.75rem;
  background: var(--vp-c-bg-soft);
}

.bench-card-title {
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin-bottom: 0.5rem;
}

.bench-card-chart {
  position: relative;
  height: 220px;
}
</style>
