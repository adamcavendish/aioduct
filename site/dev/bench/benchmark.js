import { normalizeRuns } from "./benchmark-data.mjs";

const DATA_URL = "benchmark-data.json";

const els = {
    runCount: document.querySelector("#run-count"),
    benchCount: document.querySelector("#bench-count"),
    latestDate: document.querySelector("#latest-date"),
    empty: document.querySelector("#empty-state"),
    dashboard: document.querySelector("#dashboard"),
    latestPanel: document.querySelector("#latest-panel"),
    select: document.querySelector("#benchmark-select"),
    chart: document.querySelector("#trend-chart"),
    selectedMeta: document.querySelector("#selected-meta"),
    latestCommit: document.querySelector("#latest-commit"),
    table: document.querySelector("#latest-table"),
    filter: document.querySelector("#filter"),
};

let runs = [];
let latestRows = [];

fetch(DATA_URL, { cache: "no-store" })
    .then((response) => {
        if (!response.ok) {
            throw new Error(`Failed to load ${DATA_URL}: ${response.status}`);
        }
        return response.json();
    })
    .then((data) => {
        runs = normalizeRuns(data);
        render();
    })
    .catch((error) => {
        console.error(error);
        runs = [];
        render();
    });

function render() {
    if (runs.length === 0) {
        els.empty.hidden = false;
        els.dashboard.hidden = true;
        els.latestPanel.hidden = true;
        els.runCount.textContent = "0";
        els.benchCount.textContent = "0";
        els.latestDate.textContent = "-";
        return;
    }

    els.empty.hidden = true;
    els.dashboard.hidden = false;
    els.latestPanel.hidden = false;

    const latest = runs.at(-1);
    const names = [...new Set(runs.flatMap((run) => run.benches.map((bench) => bench.name)))].sort();

    els.runCount.textContent = runs.length.toString();
    els.benchCount.textContent = names.length.toString();
    els.latestDate.innerHTML = formatMetricDate(latest.date);
    els.latestCommit.textContent = latest.commit.id
        ? `${shortSha(latest.commit.id)} - ${firstLine(latest.commit.message || "")}`
        : "Most recent benchmark values.";

    els.select.innerHTML = names
        .map((name) => `<option value="${escapeHtml(name)}">${escapeHtml(name)}</option>`)
        .join("");

    latestRows = latest.benches
        .map((bench) => ({
            ...bench,
            delta: deltaFor(bench.name),
        }))
        .sort((a, b) => a.name.localeCompare(b.name));

    els.select.addEventListener("change", () => renderChart(els.select.value));
    els.filter.addEventListener("input", renderLatestTable);

    renderChart(names[0]);
    renderLatestTable();
}

function deltaFor(name) {
    if (runs.length < 2) {
        return null;
    }
    const latest = runs.at(-1).benches.find((bench) => bench.name === name);
    const previousRun = [...runs].slice(0, -1).reverse().find((run) =>
        run.benches.some((bench) => bench.name === name)
    );
    const previous = previousRun?.benches.find((bench) => bench.name === name);
    if (!latest || !previous || previous.value === 0) {
        return null;
    }
    return ((latest.value - previous.value) / previous.value) * 100;
}

function renderLatestTable() {
    const query = els.filter.value.trim().toLowerCase();
    const rows = latestRows.filter((row) => row.name.toLowerCase().includes(query));
    els.table.innerHTML = rows
        .map((row) => `
            <tr>
                <td>${escapeHtml(row.name)}</td>
                <td>${formatValue(row.value, row.unit)}</td>
                <td class="${deltaClass(row.delta)}">${formatDelta(row.delta)}</td>
            </tr>
        `)
        .join("");
}

function renderChart(name) {
    const points = runs
        .map((run) => {
            const bench = run.benches.find((candidate) => candidate.name === name);
            return bench ? { date: run.date, value: bench.value, unit: bench.unit } : null;
        })
        .filter(Boolean);

    els.selectedMeta.textContent = points.length > 0
        ? `${points.length} run${points.length === 1 ? "" : "s"} for ${name}`
        : "No data points for this benchmark.";

    const width = 960;
    const height = 360;
    const margin = { top: 20, right: 28, bottom: 46, left: 84 };
    const innerWidth = width - margin.left - margin.right;
    const innerHeight = height - margin.top - margin.bottom;
    els.chart.setAttribute("viewBox", `0 0 ${width} ${height}`);

    if (points.length === 0) {
        els.chart.innerHTML = `<text x="40" y="60" class="chart-label">No data.</text>`;
        return;
    }

    const values = points.map((point) => point.value);
    const min = Math.min(...values);
    const max = Math.max(...values);
    const yPad = Math.max((max - min) * 0.12, max * 0.04, 1);
    const yMin = Math.max(0, min - yPad);
    const yMax = max + yPad;

    const xFor = (index) => margin.left + (points.length === 1 ? innerWidth / 2 : (index / (points.length - 1)) * innerWidth);
    const yFor = (value) => margin.top + innerHeight - ((value - yMin) / (yMax - yMin)) * innerHeight;

    const path = points
        .map((point, index) => `${index === 0 ? "M" : "L"} ${xFor(index).toFixed(2)} ${yFor(point.value).toFixed(2)}`)
        .join(" ");

    const yTicks = [0, 0.25, 0.5, 0.75, 1].map((ratio) => yMin + (yMax - yMin) * ratio);
    const latest = points.at(-1);

    els.chart.innerHTML = `
        ${yTicks.map((tick) => {
            const y = yFor(tick);
            return `
                <line class="grid-line" x1="${margin.left}" y1="${y}" x2="${width - margin.right}" y2="${y}"></line>
                <text class="chart-label" x="16" y="${y + 4}">${formatCompact(tick)}</text>
            `;
        }).join("")}
        <line class="axis" x1="${margin.left}" y1="${height - margin.bottom}" x2="${width - margin.right}" y2="${height - margin.bottom}"></line>
        <line class="axis" x1="${margin.left}" y1="${margin.top}" x2="${margin.left}" y2="${height - margin.bottom}"></line>
        <path class="line" d="${path}"></path>
        ${points.map((point, index) => `
            <circle class="dot" cx="${xFor(index)}" cy="${yFor(point.value)}" r="4">
                <title>${formatDate(point.date)}: ${formatValue(point.value, point.unit)}</title>
            </circle>
        `).join("")}
        <text class="chart-label" x="${margin.left}" y="${height - 14}">${formatDate(points[0].date)}</text>
        <text class="chart-label" text-anchor="end" x="${width - margin.right}" y="${height - 14}">${formatDate(latest.date)}</text>
        <text class="chart-label" text-anchor="end" x="${width - margin.right}" y="${margin.top + 14}">latest ${formatValue(latest.value, latest.unit)}</text>
    `;
}

function formatDate(value) {
    if (!value) {
        return "-";
    }
    return new Intl.DateTimeFormat(undefined, {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
    }).format(new Date(value));
}

function formatMetricDate(value) {
    if (!value) {
        return "-";
    }
    const date = new Date(value);
    const day = new Intl.DateTimeFormat(undefined, {
        month: "short",
        day: "numeric",
    }).format(date);
    const time = new Intl.DateTimeFormat(undefined, {
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
    }).format(date);
    return `<time datetime="${date.toISOString()}"><span class="date-day">${day}</span><span class="date-time">${time}</span></time>`;
}

function formatValue(value, unit) {
    if (!Number.isFinite(value)) {
        return "-";
    }
    if (unit === "ns/iter" && value >= 1000) {
        return `${formatNumber(value / 1000)} us/iter`;
    }
    return `${formatNumber(value)} ${unit}`;
}

function formatCompact(value) {
    if (value >= 1000000) {
        return `${formatNumber(value / 1000000)} ms`;
    }
    if (value >= 1000) {
        return `${formatNumber(value / 1000)} us`;
    }
    return `${formatNumber(value)} ns`;
}

function formatNumber(value) {
    return new Intl.NumberFormat(undefined, {
        maximumFractionDigits: value >= 100 ? 0 : 1,
    }).format(value);
}

function formatDelta(delta) {
    if (delta === null || !Number.isFinite(delta)) {
        return "baseline";
    }
    const sign = delta > 0 ? "+" : "";
    return `${sign}${delta.toFixed(1)}%`;
}

function deltaClass(delta) {
    if (delta === null || !Number.isFinite(delta) || Math.abs(delta) < 1) {
        return "delta-flat";
    }
    return delta > 0 ? "delta-bad" : "delta-good";
}

function firstLine(value) {
    return value.split("\n")[0] || "latest benchmark run";
}

function shortSha(value) {
    return value.slice(0, 7);
}

function escapeHtml(value) {
    return value
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;");
}
