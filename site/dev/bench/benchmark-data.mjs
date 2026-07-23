function extractRuns(data) {
    if (Array.isArray(data)) {
        return data;
    }
    if (Array.isArray(data?.entries)) {
        return data.entries;
    }
    if (data?.entries && typeof data.entries === "object") {
        return Object.values(data.entries)
            .filter(Array.isArray)
            .flat();
    }
    if (Array.isArray(data?.runs)) {
        return data.runs;
    }
    return data ? [data] : [];
}

export function normalizeRuns(data) {
    return extractRuns(data)
        .filter((run) => run && Array.isArray(run.benches))
        .map((run) => ({
            date: Number(run.date || Date.parse(run.commit?.timestamp) || 0),
            commit: run.commit || {},
            benches: run.benches
                .filter((bench) => bench && typeof bench.name === "string")
                .map((bench) => ({
                    name: bench.name,
                    value: Number(bench.value),
                    range: Number(bench.range || 0),
                    unit: bench.unit || "ns/iter",
                }))
                .filter((bench) => Number.isFinite(bench.value)),
        }))
        .filter((run) => run.benches.length > 0)
        .sort((a, b) => a.date - b.date);
}
