# Extraction and results

English · [日本語](jev.md) · [README](../README.md)

<a id="models"></a>

## Model selection

Default: `jev-1.14.0`. Override with CLI `--model ID`, Rust `ExtractOptions.model`, or multipart `options.model`.

Text-first accepts versioned IDs and moving aliases `jev-latest` / `jev-preview`. Vision/Hybrid require a versioned ID ≥ 1.14.0; older IDs and aliases fail before sending. Valid syntax does not guarantee provider availability.

<a id="output"></a>

## Output

| Field | Contents |
|---|---|
| `fields` | Per-field values, statuses and Evidence IDs |
| `evidence` | Original strings, PDF pages and coordinates, and value provenance |
| `usage` | Attempt counts and known token usage |
| `jev_responses` | Original body, status, question IDs, retry delay and transport error for every HTTP attempt |

A non-null value is not unconditional success: check `status` and `reason_codes`. Ambiguity, validation failures or another page’s failure can retain partial values and Evidence. Hybrid arrays retain resolved Box runs on text/image disagreement or ties; unresolvable answers do not invent values.

`jev_responses[].body` retains strings/bytes from every attempt, including retries/errors; `body_complete: false` means incomplete receipt. Report `model` records the requested ID; original responses retain the actual model ID. Reports have `evaluation_only: true`; confidence is not a calibrated correctness probability.

<a id="evaluation"></a>

## Confidence evaluation

Without `evaluation_profile`, resolved values have no confidence cutoff; source, geometry, type and validator checks remain. `--evaluation-profile synthetic` is for local synthetic tests; normally omit it from real API commands.

Without a profile, Noul ≥ 0.5 becomes true and < 0.5 false. Vision/Hybrid still ask the Box question when presence is false; only an explicit profile enables presence-based skipping. Explicit profiles annotate resolved source values without deleting them.

Choice probabilities stay as received. Probability totals are diagnostic; ties and nonmaximum selections annotate `ambiguous` without replacing Jev’s selection or discarding resolved values/Evidence.

<a id="multiple-source-runs"></a>

## Multiple source runs

Use `"output_type": "string_array"` with `text_in_box`, `selected_candidate` or `selected_candidates` to return separate source strings.

`text_in_box` returns every usable source run with positive-area Box overlap, ordered by PDF `item_index`. It does not infer visual order, impose an overlap percentage, crop/join text or fill omissions. Whole runs can contain text outside the Box; Evidence retains each original string/geometry. `selected_candidate` returns one selected string as a one-element array. The existing `string` contract is unchanged.

For Text-first multiple selection, use [text-multi-form.json](../examples/text-multi-form.json). `selected_candidates` requires `string_array`, exactly one independent Text Choice question with dynamic `text_spans`, and explicit `max_selections` of 1–254 **per page**. It is unsupported in Vision/Hybrid. Ask for the complete value or relevant part in the question.

The unchanged question repeats with selected IDs excluded until `none` or all candidates are selected. Results follow source order; identical strings at different positions remain separate. Rounds share execution limits. Reaching the cap with candidates left gives `ambiguous` / `selection_limit_reached`; errors/budget exhaustion retain obtained values and Evidence.

Array validators run after collecting all pages. Normalization runs per element without changing Evidence. `non_empty` rejects empty arrays and empty/whitespace-only elements; `max_chars` counts Unicode scalars per element, not total array length. Invalid elements fail validation and are never silently removed/truncated.

Source-text fields, including `string`, can set `"unify_cjk_radicals": true` (default false). Before normalization, Kangxi/CJK Radicals Supplement characters with compatibility decompositions map to unified ideographs; other compatibility characters stay unchanged. Evidence keeps original text.
