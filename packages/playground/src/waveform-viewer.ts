/**
 * Canvas-based waveform viewer for the Celox Playground.
 *
 * Renders VCD trace data with:
 * - Time ruler at top
 * - Signal name labels on the left
 * - 1-bit digital waveforms (high/low with transitions)
 * - Multi-bit bus waveforms (filled rectangles with hex values)
 * - Scroll (vertical/horizontal) and zoom (Ctrl+wheel)
 * - Click-to-place cursor with timestamp display
 *
 * @module
 */

// ── Data types ─────────────────────────────────────────────

export interface VcdSignalInfo {
	name: string;
	width: number;
}

export interface VcdSnapshot {
	timestamp: number;
	values: bigint[];
}

export interface VcdTrace {
	signals: VcdSignalInfo[];
	snapshots: VcdSnapshot[];
}

// ── VCD text generation ────────────────────────────────────

function vcdId(n: number): string {
	let id = "";
	let num = n;
	do {
		id = String.fromCharCode((num % 94) + 33) + id;
		num = Math.floor(num / 94) - 1;
	} while (num >= 0);
	return id;
}

function fmtVal(width: number, val: bigint, id: string): string {
	if (width === 1) return `${val}${id}`;
	return `b${val.toString(2)} ${id}`;
}

/** Generate standard VCD text from a trace (for file download). */
export function generateVcdText(trace: VcdTrace): string {
	const L: string[] = [];
	L.push("$date", `  ${new Date().toISOString()}`, "$end");
	L.push("$version celox-playground $end");
	L.push("$timescale 1ns $end");
	L.push("$scope module top $end");
	const ids = trace.signals.map((_, i) => vcdId(i));
	for (let i = 0; i < trace.signals.length; i++) {
		L.push(
			`$var wire ${trace.signals[i].width} ${ids[i]} ${trace.signals[i].name} $end`,
		);
	}
	L.push("$upscope $end");
	L.push("$enddefinitions $end");
	L.push("$dumpvars");
	if (trace.snapshots.length > 0) {
		for (let i = 0; i < trace.signals.length; i++) {
			L.push(
				fmtVal(trace.signals[i].width, trace.snapshots[0].values[i], ids[i]),
			);
		}
	}
	L.push("$end");

	let prev: bigint[] | null = null;
	for (const snap of trace.snapshots) {
		L.push(`#${snap.timestamp}`);
		for (let i = 0; i < trace.signals.length; i++) {
			if (!prev || prev[i] !== snap.values[i]) {
				L.push(fmtVal(trace.signals[i].width, snap.values[i], ids[i]));
			}
		}
		prev = [...snap.values];
	}
	return L.join("\n") + "\n";
}

// ── Theme constants ────────────────────────────────────────

const BG = "#0d1117";
const NAMES_BG = "#161b22";
const TRACE_1BIT = "#3fb950";
const TRACE_BUS = "#58a6ff";
const BUS_FILL = "#0d2847";
const RULER_COLOR = "#8b949e";
const GRID_COLOR = "#21262d";
const TEXT_COLOR = "#c9d1d9";
const TEXT_DIM = "#8b949e";
const CURSOR_COLOR = "#e94560";
const ROW_ALT_BG = "#0f1419";

const ROW_HEIGHT = 30;
const NAME_WIDTH = 150;
const RULER_HEIGHT = 26;
const SIGNAL_PAD = 5;
const TRANSITION_W = 3;

// ── Waveform Viewer ────────────────────────────────────────

export class WaveformViewer {
	private canvas: HTMLCanvasElement;
	private ctx: CanvasRenderingContext2D;
	private trace: VcdTrace | null = null;
	private scrollX = 0;
	private scrollY = 0;
	private pxPerUnit = 4;
	private cursorTime: number | null = null;
	private container: HTMLElement;
	private resizeObs: ResizeObserver;

	constructor(container: HTMLElement) {
		this.container = container;
		this.canvas = document.createElement("canvas");
		this.canvas.style.cssText = "display:block;width:100%;height:100%;";
		container.appendChild(this.canvas);
		this.ctx = this.canvas.getContext("2d")!;

		this.canvas.addEventListener("wheel", this.onWheel.bind(this), {
			passive: false,
		});
		this.canvas.addEventListener("click", this.onClick.bind(this));

		this.resizeObs = new ResizeObserver(() => this.render());
		this.resizeObs.observe(container);
	}

	setTrace(trace: VcdTrace): void {
		this.trace = trace;
		// Auto-fit horizontally
		if (trace.snapshots.length > 1) {
			const maxT = trace.snapshots[trace.snapshots.length - 1].timestamp;
			const avail = this.container.clientWidth - NAME_WIDTH - 60;
			if (maxT > 0 && avail > 0) {
				this.pxPerUnit = Math.max(0.5, Math.min(40, avail / maxT));
			}
		}
		this.scrollX = 0;
		this.scrollY = 0;
		this.cursorTime = null;
		this.render();
	}

	hasTrace(): boolean {
		return this.trace !== null && this.trace.snapshots.length > 0;
	}

	fit(): void {
		if (!this.trace || this.trace.snapshots.length < 2) return;
		const maxT =
			this.trace.snapshots[this.trace.snapshots.length - 1].timestamp;
		const avail = this.container.clientWidth - NAME_WIDTH - 60;
		if (maxT > 0 && avail > 0) {
			this.pxPerUnit = Math.max(0.5, Math.min(40, avail / maxT));
		}
		this.scrollX = 0;
		this.render();
	}

	zoomIn(): void {
		this.pxPerUnit = Math.min(100, this.pxPerUnit * 1.4);
		this.render();
	}

	zoomOut(): void {
		this.pxPerUnit = Math.max(0.1, this.pxPerUnit / 1.4);
		this.render();
	}

	render(): void {
		const dpr = window.devicePixelRatio || 1;
		const w = this.container.clientWidth;
		const h = this.container.clientHeight;
		if (w === 0 || h === 0) return;
		this.canvas.width = w * dpr;
		this.canvas.height = h * dpr;
		this.canvas.style.width = `${w}px`;
		this.canvas.style.height = `${h}px`;
		const ctx = this.ctx;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

		if (!this.trace || this.trace.snapshots.length === 0) {
			ctx.fillStyle = BG;
			ctx.fillRect(0, 0, w, h);
			ctx.fillStyle = TEXT_DIM;
			ctx.font = "13px system-ui, sans-serif";
			ctx.textAlign = "center";
			ctx.fillText(
				"No waveform data. Use sim.dump(timestamp) in your test.",
				w / 2,
				h / 2,
			);
			return;
		}

		const { signals, snapshots } = this.trace;
		const maxT = snapshots[snapshots.length - 1].timestamp;
		const timeToX = (t: number) =>
			NAME_WIDTH + t * this.pxPerUnit - this.scrollX;

		// --- Background ---
		ctx.fillStyle = BG;
		ctx.fillRect(0, 0, w, h);

		// --- Alternating row backgrounds ---
		for (let i = 0; i < signals.length; i++) {
			const y = RULER_HEIGHT + i * ROW_HEIGHT - this.scrollY;
			if (y + ROW_HEIGHT < RULER_HEIGHT || y > h) continue;
			if (i % 2 === 1) {
				ctx.fillStyle = ROW_ALT_BG;
				ctx.fillRect(
					NAME_WIDTH,
					Math.max(RULER_HEIGHT, y),
					w - NAME_WIDTH,
					ROW_HEIGHT,
				);
			}
		}

		// --- Vertical grid lines ---
		const tickIv = this.tickInterval();
		const startTick =
			Math.floor(this.scrollX / this.pxPerUnit / tickIv) * tickIv;
		ctx.strokeStyle = GRID_COLOR;
		ctx.lineWidth = 1;
		for (let t = startTick; t <= maxT + tickIv; t += tickIv) {
			const x = timeToX(t);
			if (x < NAME_WIDTH || x > w) continue;
			ctx.beginPath();
			ctx.moveTo(x, RULER_HEIGHT);
			ctx.lineTo(x, h);
			ctx.stroke();
		}

		// --- Waveform traces (clipped to waveform area) ---
		ctx.save();
		ctx.beginPath();
		ctx.rect(NAME_WIDTH, RULER_HEIGHT, w - NAME_WIDTH, h - RULER_HEIGHT);
		ctx.clip();

		for (let i = 0; i < signals.length; i++) {
			const y = RULER_HEIGHT + i * ROW_HEIGHT - this.scrollY;
			if (y + ROW_HEIGHT < RULER_HEIGHT || y > h) continue;
			this.drawTrace(ctx, i, y, w, timeToX);
		}
		ctx.restore();

		// --- Cursor ---
		if (this.cursorTime !== null) {
			const cx = timeToX(this.cursorTime);
			if (cx >= NAME_WIDTH && cx <= w) {
				ctx.strokeStyle = CURSOR_COLOR;
				ctx.lineWidth = 1;
				ctx.setLineDash([4, 3]);
				ctx.beginPath();
				ctx.moveTo(cx, RULER_HEIGHT);
				ctx.lineTo(cx, h);
				ctx.stroke();
				ctx.setLineDash([]);
			}
		}

		// --- Row separators ---
		ctx.strokeStyle = GRID_COLOR;
		ctx.lineWidth = 1;
		for (let i = 0; i <= signals.length; i++) {
			const y = RULER_HEIGHT + i * ROW_HEIGHT - this.scrollY;
			if (y < RULER_HEIGHT || y > h) continue;
			ctx.beginPath();
			ctx.moveTo(0, y);
			ctx.lineTo(w, y);
			ctx.stroke();
		}

		// --- Names column background (drawn over waveform for clean edge) ---
		ctx.fillStyle = NAMES_BG;
		ctx.fillRect(0, RULER_HEIGHT, NAME_WIDTH, h - RULER_HEIGHT);
		ctx.strokeStyle = GRID_COLOR;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.moveTo(NAME_WIDTH, 0);
		ctx.lineTo(NAME_WIDTH, h);
		ctx.stroke();

		// --- Signal names ---
		for (let i = 0; i < signals.length; i++) {
			const y = RULER_HEIGHT + i * ROW_HEIGHT - this.scrollY;
			if (y + ROW_HEIGHT < RULER_HEIGHT || y > h) continue;

			// Row separator in name column
			ctx.strokeStyle = GRID_COLOR;
			ctx.beginPath();
			ctx.moveTo(0, y + ROW_HEIGHT);
			ctx.lineTo(NAME_WIDTH, y + ROW_HEIGHT);
			ctx.stroke();

			ctx.fillStyle = TEXT_COLOR;
			ctx.font = '11px "Fira Code", monospace';
			ctx.textAlign = "right";
			const label = signals[i].name;
			const labelY = y + ROW_HEIGHT / 2 + 4;
			ctx.fillText(label, NAME_WIDTH - 10, labelY);

			if (signals[i].width > 1) {
				ctx.fillStyle = TEXT_DIM;
				ctx.font = "9px system-ui, sans-serif";
				ctx.fillText(
					`[${signals[i].width - 1}:0]`,
					NAME_WIDTH - 10,
					labelY + 11,
				);
			}
		}

		// --- Cursor value column (overlay on names) ---
		if (this.cursorTime !== null) {
			const snapIdx = this.findSnapshotAt(this.cursorTime);
			if (snapIdx >= 0) {
				for (let i = 0; i < signals.length; i++) {
					const y = RULER_HEIGHT + i * ROW_HEIGHT - this.scrollY;
					if (y + ROW_HEIGHT < RULER_HEIGHT || y > h) continue;
					const val = snapshots[snapIdx].values[i];
					const txt =
						signals[i].width === 1
							? `${val}`
							: `0x${val.toString(16).toUpperCase()}`;
					ctx.fillStyle = CURSOR_COLOR;
					ctx.font = "9px monospace";
					ctx.textAlign = "left";
					ctx.fillText(txt, 6, y + ROW_HEIGHT / 2 + 4);
				}
			}
		}

		// --- Ruler bar (on top of everything) ---
		ctx.fillStyle = NAMES_BG;
		ctx.fillRect(0, 0, w, RULER_HEIGHT);
		ctx.strokeStyle = GRID_COLOR;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.moveTo(0, RULER_HEIGHT);
		ctx.lineTo(w, RULER_HEIGHT);
		ctx.stroke();

		ctx.fillStyle = RULER_COLOR;
		ctx.font = "10px system-ui, sans-serif";
		ctx.textAlign = "center";
		for (let t = startTick; t <= maxT + tickIv; t += tickIv) {
			const x = timeToX(t);
			if (x < NAME_WIDTH - 20 || x > w + 20) continue;
			ctx.strokeStyle = RULER_COLOR;
			ctx.beginPath();
			ctx.moveTo(x, RULER_HEIGHT - 5);
			ctx.lineTo(x, RULER_HEIGHT);
			ctx.stroke();
			ctx.fillText(`${t}`, x, RULER_HEIGHT - 8);
		}

		// Cursor time label in ruler
		if (this.cursorTime !== null) {
			const cx = timeToX(this.cursorTime);
			if (cx >= NAME_WIDTH && cx <= w) {
				ctx.fillStyle = CURSOR_COLOR;
				ctx.font = "bold 10px system-ui, sans-serif";
				ctx.textAlign = "center";
				ctx.fillText(`t=${this.cursorTime}`, cx, 12);
			}
		}

		// Ruler left corner label
		ctx.fillStyle = TEXT_DIM;
		ctx.font = "10px system-ui, sans-serif";
		ctx.textAlign = "center";
		ctx.fillText("ns", NAME_WIDTH / 2, RULER_HEIGHT - 8);
	}

	clear(): void {
		this.trace = null;
		this.render();
	}

	destroy(): void {
		this.resizeObs.disconnect();
		this.canvas.remove();
	}

	// ── Drawing helpers ───────────────────────────────

	private drawTrace(
		ctx: CanvasRenderingContext2D,
		idx: number,
		y: number,
		canvasW: number,
		timeToX: (t: number) => number,
	): void {
		const { signals, snapshots } = this.trace!;
		const sig = signals[idx];
		const top = y + SIGNAL_PAD;
		const bot = y + ROW_HEIGHT - SIGNAL_PAD;
		const mid = (top + bot) / 2;

		if (snapshots.length === 0) return;

		if (sig.width === 1) {
			this.draw1bit(ctx, idx, top, bot, canvasW, timeToX);
		} else {
			this.drawBus(ctx, idx, top, bot, mid, canvasW, timeToX);
		}
	}

	private draw1bit(
		ctx: CanvasRenderingContext2D,
		idx: number,
		top: number,
		bot: number,
		canvasW: number,
		timeToX: (t: number) => number,
	): void {
		const { snapshots } = this.trace!;
		ctx.strokeStyle = TRACE_1BIT;
		ctx.lineWidth = 1.5;
		ctx.beginPath();

		let moved = false;
		for (let t = 0; t < snapshots.length; t++) {
			const val = snapshots[t].values[idx];
			const x = timeToX(snapshots[t].timestamp);
			const yVal = val ? top : bot;
			const nextX =
				t < snapshots.length - 1
					? timeToX(snapshots[t + 1].timestamp)
					: Math.max(x + 30, canvasW);

			// Skip segments entirely off-screen
			if (nextX < NAME_WIDTH && t < snapshots.length - 1) continue;

			if (!moved) {
				ctx.moveTo(Math.max(NAME_WIDTH, x), yVal);
				moved = true;
			} else {
				const prevVal = snapshots[t - 1].values[idx];
				const prevY = prevVal ? top : bot;
				ctx.lineTo(x, prevY);
				if (prevVal !== val) ctx.lineTo(x, yVal);
			}
			ctx.lineTo(Math.min(nextX, canvasW + 10), yVal);
		}
		ctx.stroke();
	}

	private drawBus(
		ctx: CanvasRenderingContext2D,
		idx: number,
		top: number,
		bot: number,
		mid: number,
		canvasW: number,
		timeToX: (t: number) => number,
	): void {
		const { signals, snapshots } = this.trace!;
		const sig = signals[idx];

		for (let t = 0; t < snapshots.length; t++) {
			const val = snapshots[t].values[idx];
			const x1 = timeToX(snapshots[t].timestamp);
			const x2 =
				t < snapshots.length - 1
					? timeToX(snapshots[t + 1].timestamp)
					: Math.max(x1 + 50, canvasW);

			if (x2 < NAME_WIDTH || x1 > canvasW) continue;

			const cx1 = Math.max(NAME_WIDTH, x1);
			const cx2 = Math.min(canvasW + 10, x2);
			const txStart = cx1 + (x1 >= NAME_WIDTH ? TRANSITION_W : 0);

			// Fill
			ctx.fillStyle = BUS_FILL;
			ctx.beginPath();
			ctx.moveTo(txStart, top);
			ctx.lineTo(cx2, top);
			ctx.lineTo(cx2, bot);
			ctx.lineTo(txStart, bot);
			ctx.closePath();
			ctx.fill();

			// Top/bottom lines
			ctx.strokeStyle = TRACE_BUS;
			ctx.lineWidth = 1;
			ctx.beginPath();
			ctx.moveTo(txStart, top);
			ctx.lineTo(cx2, top);
			ctx.stroke();
			ctx.beginPath();
			ctx.moveTo(txStart, bot);
			ctx.lineTo(cx2, bot);
			ctx.stroke();

			// Transition diamond at start
			if (x1 >= NAME_WIDTH) {
				ctx.beginPath();
				ctx.moveTo(x1, mid);
				ctx.lineTo(x1 + TRANSITION_W, top);
				ctx.stroke();
				ctx.beginPath();
				ctx.moveTo(x1, mid);
				ctx.lineTo(x1 + TRANSITION_W, bot);
				ctx.stroke();
			}

			// Hex value text
			const textW = cx2 - txStart - 4;
			if (textW > 18) {
				const hex =
					sig.width <= 8
						? val.toString(10)
						: "0x" + val.toString(16).toUpperCase();
				ctx.fillStyle = TEXT_COLOR;
				ctx.font = '10px "Fira Code", monospace';
				ctx.textAlign = "center";
				ctx.fillText(hex, (txStart + cx2) / 2, mid + 3, textW);
			}
		}
	}

	// ── Tick interval ────────────────────────────────

	private tickInterval(): number {
		const minPx = 60;
		const raw = minPx / this.pxPerUnit;
		const mag = Math.pow(10, Math.floor(Math.log10(raw)));
		const norm = raw / mag;
		const nice = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
		return Math.max(1, nice * mag);
	}

	// ── Find snapshot at cursor time ─────────────────

	private findSnapshotAt(time: number): number {
		const snaps = this.trace!.snapshots;
		if (snaps.length === 0) return -1;
		// Binary search for last snapshot <= time
		let lo = 0;
		let hi = snaps.length - 1;
		while (lo < hi) {
			const mid = (lo + hi + 1) >>> 1;
			if (snaps[mid].timestamp <= time) lo = mid;
			else hi = mid - 1;
		}
		return snaps[lo].timestamp <= time ? lo : -1;
	}

	// ── Event handlers ───────────────────────────────

	private onWheel(e: WheelEvent): void {
		e.preventDefault();
		if (e.ctrlKey || e.metaKey) {
			const rect = this.canvas.getBoundingClientRect();
			const mouseX = e.clientX - rect.left;
			const tAtMouse =
				(mouseX - NAME_WIDTH + this.scrollX) / this.pxPerUnit;
			const factor = e.deltaY > 0 ? 0.8 : 1.25;
			this.pxPerUnit = Math.max(0.1, Math.min(100, this.pxPerUnit * factor));
			this.scrollX = tAtMouse * this.pxPerUnit - (mouseX - NAME_WIDTH);
			this.scrollX = Math.max(0, this.scrollX);
		} else if (e.shiftKey) {
			this.scrollX = Math.max(0, this.scrollX + e.deltaY);
		} else {
			if (this.trace) {
				const maxY = Math.max(
					0,
					this.trace.signals.length * ROW_HEIGHT -
						(this.container.clientHeight - RULER_HEIGHT),
				);
				this.scrollY = Math.max(
					0,
					Math.min(maxY, this.scrollY + e.deltaY),
				);
			}
		}
		this.render();
	}

	private onClick(e: MouseEvent): void {
		const rect = this.canvas.getBoundingClientRect();
		const x = e.clientX - rect.left;
		if (x > NAME_WIDTH) {
			this.cursorTime = Math.max(
				0,
				Math.round((x - NAME_WIDTH + this.scrollX) / this.pxPerUnit),
			);
			this.render();
		}
	}
}
