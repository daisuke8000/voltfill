//! Layout hypotheses over source fragments: lines and label/value pairs.
//!
//! Pairs are candidates, not decisions: a later stage matches them against
//! form labels. Every value is an exact byte range of an existing fragment,
//! so text is never synthesized. Sub-run boxes are estimated from
//! character classes (full width = 1 em, otherwise 0.5 em) and marked
//! [`Precision::Estimated`]; they are never reported as measured.
//!
//! Supported: horizontal text, inline `label：value` / `label: value` in one
//! fragment, value to the right on the same line, value directly below, and
//! value continuation over following lines in the same column.
//! Not supported yet: tables, AcroForm values, vertical text, and splits
//! without a delimiter (for example `発行日 2026年…`).
use crate::geometry::Rect;
use crate::source::{Quality, SourceFragment};
use serde::Serialize;

/// How a span's box was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// The whole fragment box reported by the extractor.
    Run,
    /// A part of a fragment, positioned by character-class width estimates.
    Estimated,
}

/// An exact byte range of one source fragment with its box.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub fragment_id: String,
    pub start: usize,
    pub end: usize,
    pub bbox: Rect,
    pub precision: Precision,
}

/// Usable fragments sharing a baseline band, left to right.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub page: u32,
    /// Indexes into the analyzed fragment slice.
    pub fragments: Vec<usize>,
    pub bbox: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Label and value in one fragment, split at a delimiter.
    Inline,
    /// Value is the next fragment to the right on the same line.
    Right,
    /// Value starts directly below the label.
    Below,
}

/// A label/value hypothesis. `value` holds one span per line, in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pair {
    pub page: u32,
    pub label: Span,
    pub value: Vec<Span>,
    pub placement: Placement,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub pairs: Vec<Pair>,
}

/// Build lines and label/value hypotheses from usable fragments.
/// Work is O(n log n) per page plus a bounded forward scan per pair.
#[must_use]
pub fn analyze(fragments: &[SourceFragment]) -> Layout {
    let lines = group_lines(fragments);
    let mut pairs = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for &f in &line.fragments {
            if let Some(pair) = inline_pair(&fragments[f]) {
                pairs.push(pair);
            }
        }
        for window in line.fragments.windows(2) {
            let (label, value) = (&fragments[window[0]], &fragments[window[1]]);
            if inline_pair(label).is_some() {
                continue;
            }
            let mut values = vec![whole(value)];
            values.extend(continuation(fragments, &lines, i, value));
            pairs.push(Pair {
                page: line.page,
                label: label_span(label),
                value: values,
                placement: Placement::Right,
            });
        }
        if let Some(next) = lines.get(i + 1).filter(|next| next.page == line.page) {
            for &f in &line.fragments {
                if inline_pair(&fragments[f]).is_some() {
                    continue;
                }
                if let Some(value) = below(fragments, &fragments[f], next) {
                    let mut values = vec![whole(value)];
                    values.extend(continuation(fragments, &lines, i + 1, value));
                    pairs.push(Pair {
                        page: line.page,
                        label: label_span(&fragments[f]),
                        value: values,
                        placement: Placement::Below,
                    });
                }
            }
        }
    }
    Layout { lines, pairs }
}

fn usable(f: &SourceFragment) -> bool {
    f.quality == Quality::Usable && !f.raw_text.trim().is_empty() && f.bbox.validate().is_ok()
}

fn group_lines(fragments: &[SourceFragment]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..fragments.len())
        .filter(|&i| usable(&fragments[i]))
        .collect();
    order.sort_by(|&a, &b| {
        let (fa, fb) = (&fragments[a], &fragments[b]);
        fa.page
            .cmp(&fb.page)
            .then(fa.bbox.top.total_cmp(&fb.bbox.top))
            .then(fa.bbox.left.total_cmp(&fb.bbox.left))
    });
    let mut lines: Vec<Line> = Vec::new();
    for i in order {
        let f = &fragments[i];
        match lines.last_mut() {
            Some(line) if line.page == f.page && same_band(line.bbox, f.bbox) => {
                line.fragments.push(i);
                line.bbox = union(line.bbox, f.bbox);
            }
            _ => lines.push(Line {
                page: f.page,
                fragments: vec![i],
                bbox: f.bbox,
            }),
        }
    }
    for line in &mut lines {
        line.fragments
            .sort_by(|&a, &b| fragments[a].bbox.left.total_cmp(&fragments[b].bbox.left));
    }
    lines
}

/// Vertical overlap of at least half the smaller height.
fn same_band(a: Rect, b: Rect) -> bool {
    let overlap = a.bottom.min(b.bottom) - a.top.max(b.top);
    overlap >= 0.5 * a.height().min(b.height())
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn whole(f: &SourceFragment) -> Span {
    let start = f.raw_text.len() - f.raw_text.trim_start().len();
    let end = f.raw_text.trim_end().len();
    span(f, start, end)
}

/// A label fragment without a trailing delimiter, e.g. `請求番号：` → `請求番号`.
fn label_span(f: &SourceFragment) -> Span {
    let full = whole(f);
    let text = &f.raw_text[full.start..full.end];
    let trimmed = text
        .trim_end_matches(DELIMITERS)
        .trim_end_matches(char::is_whitespace);
    if trimmed.is_empty() || trimmed.len() == text.len() {
        return full;
    }
    span(f, full.start, full.start + trimmed.len())
}

/// A span over `start..end` (byte offsets on char boundaries) of `f`.
fn span(f: &SourceFragment, start: usize, end: usize) -> Span {
    let text = &f.raw_text;
    if start == 0 && end == text.len() {
        return Span {
            fragment_id: f.id.clone(),
            start,
            end,
            bbox: f.bbox,
            precision: Precision::Run,
        };
    }
    let weight = |s: &str| s.chars().map(char_em).sum::<f64>();
    let total = weight(text);
    let (left, right) = if total > 0.0 {
        let width = f.bbox.width();
        (
            f.bbox.left + width * weight(&text[..start]) / total,
            f.bbox.left + width * weight(&text[..end]) / total,
        )
    } else {
        (f.bbox.left, f.bbox.right)
    };
    Span {
        fragment_id: f.id.clone(),
        start,
        end,
        bbox: Rect {
            left,
            top: f.bbox.top,
            right: right.max(left + f64::EPSILON),
            bottom: f.bbox.bottom,
        },
        precision: Precision::Estimated,
    }
}

const DELIMITERS: &[char] = &['：', ':'];

/// Split `label：value` inside one fragment. Skips times (`9:00`) and URL schemes.
fn inline_pair(f: &SourceFragment) -> Option<Pair> {
    let text = &f.raw_text;
    for (at, delimiter) in text.char_indices().filter(|(_, c)| DELIMITERS.contains(c)) {
        let after = at + delimiter.len_utf8();
        let before_char = text[..at].chars().next_back();
        let after_char = text[after..].chars().next();
        if delimiter == ':'
            && (before_char.is_some_and(|c| c.is_ascii_digit())
                && after_char.is_some_and(|c| c.is_ascii_digit())
                || text[after..].starts_with("//"))
        {
            continue;
        }
        let label_text = text[..at].trim();
        let value_text = text[after..].trim();
        if label_text.is_empty() || value_text.is_empty() {
            continue;
        }
        let label_start = text.len() - text.trim_start().len();
        let label_end = text[..at].trim_end().len();
        let value_start = after + (text[after..].len() - text[after..].trim_start().len());
        let value_end = text.trim_end().len();
        return Some(Pair {
            page: f.page,
            label: span(f, label_start, label_end),
            value: vec![span(f, value_start, value_end)],
            placement: Placement::Inline,
        });
    }
    None
}

/// The first fragment on `next` that starts under `label` and is close below it.
fn below<'a>(
    fragments: &'a [SourceFragment],
    label: &SourceFragment,
    next: &Line,
) -> Option<&'a SourceFragment> {
    next.fragments.iter().map(|&i| &fragments[i]).find(|v| {
        let height = label.bbox.height().max(v.bbox.height());
        inline_pair(v).is_none()
            && (v.bbox.left - label.bbox.left).abs() <= height
            && v.bbox.top - label.bbox.bottom <= height
            && v.bbox.top >= label.bbox.bottom - 0.5 * height
    })
}

/// Follow-on lines of a value in the same column. A line is merged only when it
/// has a fragment aligned with the value and nothing in the label column to
/// its left, so stacked label rows (小計 / 合計) stay separate.
fn continuation(
    fragments: &[SourceFragment],
    lines: &[Line],
    from: usize,
    value: &SourceFragment,
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut previous = value.bbox;
    for line in lines.iter().skip(from + 1) {
        if line.page != value.page {
            break;
        }
        let height = previous.height();
        let Some(&first) = line.fragments.first() else {
            break;
        };
        let next = &fragments[first];
        if line.bbox.top - previous.bottom > height
            || (next.bbox.left - value.bbox.left).abs() > 0.5 * height
            || line.fragments.len() != 1
            || inline_pair(next).is_some()
        {
            break;
        }
        spans.push(whole(next));
        previous = next.bbox;
    }
    spans
}

/// Estimated advance in em: wide East Asian characters 1, others 0.5.
fn char_em(c: char) -> f64 {
    let wide = matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD);
    if wide { 1.0 } else { 0.5 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(id: &str, text: &str, [l, t, r, b]: [f64; 4]) -> SourceFragment {
        SourceFragment {
            id: id.into(),
            page: 1,
            item_index: 0,
            raw_text: text.into(),
            bbox: Rect {
                left: l,
                top: t,
                right: r,
                bottom: b,
            },
            quality: Quality::Usable,
        }
    }

    fn texts(fragments: &[SourceFragment], spans: &[Span]) -> Vec<String> {
        spans
            .iter()
            .map(|s| {
                let f = fragments.iter().find(|f| f.id == s.fragment_id).unwrap();
                f.raw_text[s.start..s.end].to_owned()
            })
            .collect()
    }

    fn find<'a>(layout: &'a Layout, fragments: &[SourceFragment], label: &str) -> Vec<&'a Pair> {
        layout
            .pairs
            .iter()
            .filter(|p| texts(fragments, std::slice::from_ref(&p.label))[0] == label)
            .collect()
    }

    #[test]
    fn inline_delimiter_splits_one_fragment_on_char_boundaries() {
        // Coordinates observed from an embedded-TrueType fixture.
        let fragments = [
            frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
            frag("b", "請求番号： INV-0012", [60.0, 139.0, 163.4, 150.0]),
            frag("c", "Invoice No: AB-778", [60.0, 159.0, 154.8, 170.0]),
        ];
        let layout = analyze(&fragments);
        let inline: Vec<_> = layout
            .pairs
            .iter()
            .filter(|p| p.placement == Placement::Inline)
            .collect();
        assert_eq!(inline.len(), 3);
        for pair in &inline {
            assert_eq!(pair.label.precision, Precision::Estimated);
            assert_eq!(pair.value[0].precision, Precision::Estimated);
        }
        assert_eq!(texts(&fragments, &[inline[0].label.clone()]), ["請求番号"]);
        assert_eq!(texts(&fragments, &inline[0].value), ["INV-0012"]);
        assert_eq!(texts(&fragments, &inline[1].value), ["INV-0012"]);
        assert_eq!(
            texts(&fragments, &[inline[2].label.clone()]),
            ["Invoice No"]
        );
        assert_eq!(texts(&fragments, &inline[2].value), ["AB-778"]);
        // Inline pairs are complete; they are never read as labels or values of other pairs.
        assert_eq!(layout.pairs.len(), 3);
        // True value start is 115.0 for this font; the estimate stays within 0.2 em.
        let value = &inline[0].value[0].bbox;
        assert!((value.left - 115.0).abs() < 0.2 * 11.0, "{}", value.left);
        assert!((value.right - 161.4).abs() < 1e-9);
    }

    #[test]
    fn times_urls_and_empty_sides_are_not_split() {
        for text in ["9:00 〜 18:00", "https://example.invalid", "備考：", "：値"] {
            assert!(
                inline_pair(&frag("x", text, [0.0, 0.0, 100.0, 10.0])).is_none(),
                "{text}"
            );
        }
        let pair = inline_pair(&frag("x", "時間：9:00", [0.0, 0.0, 100.0, 10.0])).unwrap();
        assert_eq!(pair.value[0].start, "時間：".len());
    }

    #[test]
    fn right_values_and_multiline_continuation() {
        let fragments = [
            frag("l1", "住所", [60.0, 119.0, 82.0, 130.0]),
            frag(
                "v1",
                "東京都架空区サンプル町1-2-3",
                [200.0, 119.0, 346.7, 130.0],
            ),
            frag("v2", "テストビル5階", [200.0, 135.0, 272.1, 146.0]),
            frag("l2", "備考", [60.0, 165.0, 82.0, 176.0]),
            frag("v3", "午前中の配達を希望。", [200.0, 165.0, 310.0, 176.0]),
            frag(
                "v4",
                "不在時は宅配ボックスへ。",
                [200.0, 181.0, 332.0, 192.0],
            ),
        ];
        let layout = analyze(&fragments);
        assert_eq!(layout.lines.len(), 4);
        let address = find(&layout, &fragments, "住所");
        assert_eq!(address.len(), 1);
        assert_eq!(address[0].placement, Placement::Right);
        assert_eq!(
            texts(&fragments, &address[0].value),
            ["東京都架空区サンプル町1-2-3", "テストビル5階"]
        );
        assert!(
            address[0]
                .value
                .iter()
                .all(|s| s.precision == Precision::Run)
        );
        let note = find(&layout, &fragments, "備考");
        assert_eq!(
            texts(&fragments, &note[0].value),
            ["午前中の配達を希望。", "不在時は宅配ボックスへ。"]
        );
    }

    #[test]
    fn stacked_label_rows_do_not_merge() {
        let fragments = [
            frag("a", "小計", [330.0, 119.0, 352.0, 130.0]),
            frag("b", "¥11,000", [480.2, 119.0, 520.0, 130.0]),
            frag("c", "消費税(10%)", [330.0, 139.0, 392.3, 150.0]),
            frag("d", "¥1,100", [486.4, 139.0, 520.0, 150.0]),
            frag("e", "合計", [330.0, 159.0, 352.0, 170.0]),
            frag("f", "¥12,100", [480.2, 159.0, 520.0, 170.0]),
        ];
        let layout = analyze(&fragments);
        for (label, value) in [
            ("小計", "¥11,000"),
            ("消費税(10%)", "¥1,100"),
            ("合計", "¥12,100"),
        ] {
            let right: Vec<_> = find(&layout, &fragments, label)
                .into_iter()
                .filter(|p| p.placement == Placement::Right)
                .collect();
            assert_eq!(right.len(), 1, "{label}");
            assert_eq!(texts(&fragments, &right[0].value), [value]);
        }
    }

    #[test]
    fn value_below_label() {
        let fragments = [
            frag("a", "お届け先氏名", [60.0, 121.0, 114.0, 130.0]),
            frag("b", "お届け予定日", [300.0, 121.0, 354.0, 130.0]),
            frag("c", "佐藤 花子", [60.0, 135.0, 115.6, 148.0]),
            frag("d", "2026/12/01", [300.0, 135.0, 365.0, 148.0]),
        ];
        let layout = analyze(&fragments);
        for (label, value) in [
            ("お届け先氏名", "佐藤 花子"),
            ("お届け予定日", "2026/12/01"),
        ] {
            let below: Vec<_> = find(&layout, &fragments, label)
                .into_iter()
                .filter(|p| p.placement == Placement::Below)
                .collect();
            assert_eq!(below.len(), 1, "{label}");
            assert_eq!(texts(&fragments, &below[0].value), [value]);
        }
        // Separate rows with a full line gap are not read as label-above-value.
        let rows = [
            frag("a", "会社名", [60.0, 119.0, 93.0, 130.0]),
            frag("b", "担当者", [60.0, 143.0, 93.0, 154.0]),
        ];
        assert!(analyze(&rows).pairs.is_empty());
    }

    #[test]
    fn unusable_fragments_are_ignored() {
        let mut fragment = frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]);
        fragment.quality = Quality::Suspect;
        assert_eq!(analyze(&[fragment]), Layout::default());
    }
}
