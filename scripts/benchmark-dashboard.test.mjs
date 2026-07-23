import assert from "node:assert/strict";
import test from "node:test";

import { normalizeRuns } from "../site/dev/bench/benchmark-data.mjs";

test("normalizes benchmark-action named suite entries", () => {
    const data = {
        lastUpdate: 200,
        entries: {
            "aioduct benchmarks": [
                {
                    date: 200,
                    commit: { id: "new" },
                    benches: [
                        { name: "get", value: 120, range: 4, unit: "ns/iter" },
                    ],
                },
                {
                    date: 100,
                    commit: { id: "old" },
                    benches: [
                        { name: "get", value: "80" },
                        { name: "invalid", value: "not-a-number" },
                    ],
                },
            ],
        },
    };

    assert.deepEqual(normalizeRuns(data), [
        {
            date: 100,
            commit: { id: "old" },
            benches: [{ name: "get", value: 80, range: 0, unit: "ns/iter" }],
        },
        {
            date: 200,
            commit: { id: "new" },
            benches: [{ name: "get", value: 120, range: 4, unit: "ns/iter" }],
        },
    ]);
});

test("keeps legacy array and runs envelopes", () => {
    const run = {
        commit: { timestamp: "2026-07-23T00:00:00Z" },
        benches: [{ name: "post", value: 42 }],
    };

    assert.deepEqual(normalizeRuns([run]), normalizeRuns({ runs: [run] }));
    assert.equal(normalizeRuns(null).length, 0);
});
