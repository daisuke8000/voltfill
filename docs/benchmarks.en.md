# Live API benchmarks

English · [日本語](benchmarks.md)

Measured with the Jev API on 2026-10-18 using a recreated synthetic PDF with two pages and 20 fields. CLI and HTTP are grouped separately, with Text, Vision, Hybrid, PDFium and Hayro compared within each group. **Ten conditions were measured once each with the same input.**

This PDF has a different hash and source geometry from the earlier benchmark input. Direct performance comparisons with those earlier measurements are not supported. The input PDF and dedicated data are not included in this repository.

## Findings

- **Returning values and matching complete values are different results.** Every condition retained all 20 values and their Evidence: 200 values in total, with zero errors. There were 164/200 full matches and 36/200 partial matches, no extra source fragments and no missing values. Four fields retained an `ambiguous` warning.
- **Vision and Hybrid did not fully match all fields in every condition.** CLI full-match counts were Text 6, Vision PDFium 18 / Hayro 19, Hybrid PDFium 20 / Hayro 20. HTTP counts in the same order were 6 / 18 / 19 / 20 / 18.
- **Hybrid had higher mean Box confidence.** Across 80 paired fields, it was higher in 53, lower in 23 and equal in 4. The means were 0.76075 for Vision and 0.78725 for Hybrid. These are returned scores, not evidence of higher accuracy.
- **Vision used less elapsed time and input usage in these runs.** Within each route/renderer pair, Hybrid took 2.6894–3.8786 times as long and used 5.0876–5.3793 times as many input tokens, including retries.
- **Text used the single-candidate schema.** Each route used 2 POSTs, with 6 full and 14 partial matches. The new Text multiple-selection feature was not measured here.

One input and one run per condition do not establish general accuracy or performance rankings. Assess full matches, omitted fragments, warnings, elapsed time and usage separately.

## Conditions

| Setting | Value |
|---|---|
| Input | One shared recreated PDF, two pages, 20 fields |
| Model | `jev-1.14.0` |
| Rendering | PDFium / Hayro, `1.5 pixels/point`, no automatic fallback |
| Concurrency | At most 2 concurrent POSTs within one extraction; conditions run sequentially |
| Request rate | At most 2 POSTs/second |
| Batching | At most 10 questions; split by page, modality and question dependencies |
| Questions | Text 20 / Vision 40 / Hybrid 60; the three original schemas were used unchanged |
| Retries | At most 3 attempts including the first; POST caps: Text 6 / Vision 12 / Hybrid 18 |
| Input reservations | At most 100,000 tokens/second and 64,000 tokens/attempt |
| Context limits | 64,000 tokens/request, 32,000 tokens/question; image estimate 16,000 tokens/occurrence |
| Cost reservation cap | 50,000,000 nano USD per extraction; modeled reservations do not guarantee a provider billing cap |
| Timeouts | Overall deadline 180 seconds; individual POST timeout 10 seconds |
| Confidence profile | Unspecified; values were not removed based on confidence |
| Output contract | Text returns one candidate; Vision/Hybrid return arrays of source fragments |
| Timing scope | Internal VOLTFILL elapsed time; excludes CLI startup and HTTP upload/response round trips |
| Measurements | One per condition; no repeated-run means, p95 or statistical significance assessed |

A full match requires every reference fragment in order, including exact whitespace. A partial match returns only a subset. Warnings count fields whose `status` is not `extracted`, separately from full matches. Confidence below selects each field's final Box answer in received-attempt order, then averages the 20 fields. It is neither an all-attempt average nor a repeated-run mean. Text Choice confidence is omitted because it uses a different question type. Usage includes retries.

## CLI

| Strategy | Renderer | Full matches | Mean Box confidence | Total seconds | POSTs | Retries | Fields with warnings |
|---|---|---:|---:|---:|---:|---:|---:|
| Text | — | 6/20 | — | 0.397 | 2 | 0 | 1 |
| Vision | PDFium | 18/20 | 0.7570 | 1.303 | 4 | 0 | 0 |
| Vision | Hayro | 19/20 | 0.7550 | 1.257 | 4 | 0 | 0 |
| Hybrid | PDFium | 20/20 | 0.7870 | 3.808 | 6 | 0 | 0 |
| Hybrid | Hayro | 20/20 | 0.7845 | 4.875 | 7 | 1 | 0 |

| Strategy | Renderer | Render seconds | Input tokens | Output tokens |
|---|---|---:|---:|---:|
| Text | — | 0.000 | 57620 | 43634 |
| Vision | PDFium | 0.029 | 14666 | 796 |
| Vision | Hayro | 0.022 | 14676 | 796 |
| Hybrid | PDFium | 0.028 | 74656 | 44430 |
| Hybrid | Hayro | 0.022 | 78946 | 44624 |

### Vision versus Hybrid

| Renderer | Mean confidence difference (Hybrid − Vision) | Fields higher in Hybrid | Lower | Equal |
|---|---:|---:|---:|---:|
| PDFium | +0.0300 | 12/20 | 6 | 2 |
| Hayro | +0.0295 | 17/20 | 3 | 0 |

Only CLI Hybrid/Hayro retried, once. Two attempts for the same page/question set were recorded, both with HTTP 200. The retry cause is not asserted here. Both attempts are included in time, POST counts and usage.

## HTTP

| Strategy | Renderer | Full matches | Mean Box confidence | Total seconds | POSTs | Retries | Fields with warnings |
|---|---|---:|---:|---:|---:|---:|---:|
| Text | — | 6/20 | — | 0.377 | 2 | 0 | 1 |
| Vision | PDFium | 18/20 | 0.7680 | 1.327 | 4 | 0 | 0 |
| Vision | Hayro | 19/20 | 0.7630 | 1.319 | 4 | 0 | 0 |
| Hybrid | PDFium | 20/20 | 0.8025 | 3.570 | 6 | 0 | 1 |
| Hybrid | Hayro | 18/20 | 0.7750 | 3.562 | 6 | 0 | 1 |

| Strategy | Renderer | Render seconds | Input tokens | Output tokens |
|---|---|---:|---:|---:|
| Text | — | 0.000 | 57620 | 43634 |
| Vision | PDFium | 0.029 | 14666 | 796 |
| Vision | Hayro | 0.021 | 14676 | 796 |
| Hybrid | PDFium | 0.029 | 74656 | 44430 |
| Hybrid | Hayro | 0.021 | 74666 | 44430 |

### Vision versus Hybrid

| Renderer | Mean confidence difference (Hybrid − Vision) | Fields higher in Hybrid | Lower | Equal |
|---|---:|---:|---:|---:|
| PDFium | +0.0345 | 13/20 | 6 | 1 |
| Hayro | +0.0120 | 11/20 | 8 | 1 |

No HTTP condition retried. Hybrid had higher mean Box confidence with both renderers, but not for every field, and confidence did not determine full-match counts.

## Differences by field

| Condition | Source fragments absent from returned values |
|---|---|
| Vision/PDFium, CLI and HTTP | First fragment of field 14, final fragment of field 20 |
| Vision/Hayro, CLI and HTTP | Final fragment of field 20 |
| Hybrid/Hayro, HTTP | First fragment of field 14, final fragment of field 20 |

These are omissions in the returned scope; they do not establish that rendering itself omitted the content. All other Vision/Hybrid fields fully matched. Text's single-candidate output contract produced partial matches for fields requiring multiple fragments.

| Condition and field with a warning | Status / reason | Retained content |
|---|---|---|
| Text, CLI and HTTP, field 10 | `ambiguous` / `probability_sum_not_one` | Selected value and Evidence; retained even with CLI exit code 3 |
| Hybrid/PDFium, HTTP, field 17 | `ambiguous` / `probability_sum_not_one` | Fully matching array and Evidence |
| Hybrid/Hayro, HTTP, field 14 | `ambiguous` / `hybrid_source_disagreement` | Array resolved from the Box and Evidence; a partial full-value match |

Probability totals and text/image disagreement did not delete received values or Evidence. All original responses were retained. Values with warnings must not be treated as unconditional successes.

## Renderers and routes

Full-match counts differed by route and renderer. CLI Hybrid matched 20/20 with both renderers; HTTP Hybrid matched 20/20 with PDFium and 18/20 with Hayro. Each was measured once, without scoring visual rendering quality. Equal values do not imply identical images, Boxes or Evidence coordinates.

The input and model were shared by all conditions. Schemas and configured limits were defined per strategy, then held constant across CLI/HTTP and renderers for that strategy. CLI and HTTP share extraction and transport code, with file reading versus upload receipt at the entry point. Byte-for-byte equality of complete sent bodies is unverified. Differences between CLI and HTTP times do not establish server overhead or general strategy performance.

## Records and further measurements

The ten conditions used 45 POSTs, including 1 retry, with 476,848 input and 268,366 output tokens. All 45 attempts returned HTTP 200 with complete original bodies. Internal total times sum to 21.794533751 seconds: this is the sum across ten sequential conditions, not the benchmark's wall-clock duration. No-send tests and saved-response replays are excluded. Charges were not measured.

Original responses, Evidence, details and the HTML dashboard are retained as local artifacts. This Markdown summarizes the same measured data as the current dashboard; it is not a distributable dataset containing inputs and responses. SHA-256 records cover the input, executable, schemas, configurations and native library used. The code commit ID and detailed CPU/OS information were not recorded.

Further measurements should fix and record the input, questions/output contract, configuration, model, binary and environment, keeping CLI and HTTP comparisons separate. The new Text multiple-selection feature and inputs with text/image disagreement require separate conditions. No automatically repeating live-API command is provided here. Input can first be checked without sending through the normal product path:

```sh
voltfill inspect input.pdf
voltfill run input.pdf schema.json --dry-run
```
