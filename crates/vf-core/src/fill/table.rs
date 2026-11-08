//! Table fields (line items): rows read under one header row.
//!
//! A table field (`"type": "table"`) lists columns, each with an `id`, a
//! `label`, optional `hints` (other header labels) and a scalar `type`
//! (`text`, `date`, `amount`, `number`). Offline fill commits its rows only
//! when all of these hold ("a wrong value is worse than an empty one"):
//! - Header: one line of the document has, for every column, a whole plain
//!   fragment (no `label：` delimiter) whose label equals the column label
//!   or one of its hints exactly, with the scalar label normalization (NFKC,
//!   no whitespace or trailing delimiter, lowercase, equal bracketed
//!   qualifiers; one leading `ご` `御` `お` ignored). The line may hold
//!   columns the form does not ask for. It must be the only such line in
//!   the document and no column may match two cells of it; otherwise the
//!   field is `ambiguous` and every table read (at most five) is a
//!   candidate.
//! - Rows: [`layout::read_table`] reads the lines below the header as rows
//!   of cells aligned to the header cells (its documentation states the
//!   alignment, row pitch, the rows it treats as irregular and how the
//!   lines below a stop are read). A line with a totals label ends the
//!   rows: closing totals such as `合計`, `請求金額`, `今回御請求額`,
//!   `以下余白`, and subtotals such as `小計`, `消費税`. A line with an item
//!   row left of its totals label (a totals block beside the last row), or
//!   that reads as an item row as a whole, is irregular instead: two or
//!   more cells in the header's first column, a requested text column or a
//!   requested `number`/`date` column (amounts in requested `amount`
//!   columns not counted), one of them in the first or a text column, where
//!   a totals label counts only in a requested non-text column (`合計` in
//!   the margin before `ぶどう 1 ¥900`, `ぶどう 小計 ¥900` with `小計` under
//!   `数量`).
//! - Structure: the rows are regular, with no item row below the line where
//!   they stopped, and each row is an item: a cell in a requested typed
//!   column (when the field has one) and a cell in the header's first column
//!   or a requested text column (otherwise [`Hold::IrregularTable`]); at most
//!   [`layout::MAX_TABLE_ROWS`] rows ([`Hold::TooManyRows`]); and closed:
//!   when a later page exists (usable or not), only a closing total closes
//!   them, and a page that says the document continues (`次頁`, `次ページ`),
//!   or whose next page (with any fragment) says it continues it (`前頁`,
//!   `繰越` anywhere on that page), never closes them
//!   ([`Hold::ContinuesOnNextPage`]). Rows never continue onto another
//!   page.
//! - Cells: every non-empty cell of a `date`, `amount` or `number` column
//!   matches the scalar type's whole-text grammar; amounts and numbers are
//!   not normalized when any page of the document has a unit note
//!   (`（単位：千円）`, `（千円）`, see the parent module) or under a header
//!   cell that states a scale (`金額（千円）`)
//!   ([`Hold::InvalidCell`] otherwise). Text cells are the source text.
//!   Blank cells are `null`, never filled in.
//! - Types: the field has a typed column; with text columns only, nothing
//!   tells item rows from the header and rows of a table below
//!   ([`Hold::UntypedTable`]).
//!
//! A held table is a candidate with the first failed condition, in the
//! order above, and the rows read before it ended (before the first row
//! that is not an item). Status: `filled` (rows, header evidence) with at
//! least one row and no hold; `not_found` without a header line or with a
//! closed header line without rows; `invalid` for [`Hold::InvalidCell`];
//! `ambiguous` otherwise. Table fields are never sent to Jev.
use super::{
    FieldFill, FieldType, FillError, FillEvidence, FillStatus, FormField, Hold, Label,
    MAX_CANDIDATES, TableCandidate, TableCell, TableRow, empty, normalize_label, normalize_value,
    slice, states_scale,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use vf_source::SourceFragment;
use vf_source::budget::{Budget, BudgetError};
use vf_source::layout::{self, ColumnRole, Line, Span, TableEnd, Totals};

/// Most columns of one table field.
pub const MAX_COLUMNS: usize = 16;

/// Normalized labels (stems) of a closing total or end mark: the items end
/// here, also when a later page exists.
const TOTAL: [&str; 32] = [
    "合計",
    "総計",
    "総合計",
    "総額",
    "合計金額",
    "合計額",
    "総合計金額",
    "税込合計",
    "税込合計金額",
    "税込金額",
    "請求金額",
    "請求額",
    "請求合計",
    "今回請求額",
    "今回請求金額",
    "差引請求額",
    "差引請求金額",
    "支払金額",
    "支払額",
    "支払合計",
    "差引支払額",
    "今回支払額",
    "今回支払金額",
    "税込総額",
    "請求総額",
    "税込請求額",
    "税込請求金額",
    "見積金額",
    "見積額",
    "見積総額",
    "以下余白",
    "以上",
];
/// Normalized labels (stems) of a partial sum: the items end here, but may
/// go on (on a later page, without a closing total).
const SUBTOTAL: [&str; 21] = [
    "計",
    "小計",
    "税抜金額",
    "税抜合計",
    "税抜額",
    "消費税",
    "消費税額",
    "消費税等",
    "消費税合計",
    "税額",
    "内消費税",
    "内消費税等",
    "うち消費税",
    "内税",
    "外税",
    "源泉徴収税額",
    "源泉所得税",
    "源泉所得税額",
    "源泉税",
    "値引合計",
    "値引額合計",
];
/// Endings of a normalized label that make it a partial sum (`10%対象`,
/// `8%対象計`, `軽減税率対象`, `ギフト小計`, `10%対象合計`).
const SUBTOTAL_ENDINGS: [&str; 7] = [
    "小計",
    "合計",
    "総計",
    "%対象",
    "%対象計",
    "%対象額",
    "税率対象",
];
/// Text that says the document continues on another page.
const CONTINUATION: [&str; 4] = ["次頁", "次ページ", "次の頁", "次のページ"];
/// Text that says a page continues the page before it (`（前頁より続く）`,
/// `前頁より繰越`).
const CONTINUED_FROM: [&str; 6] = ["前頁", "前ページ", "前の頁", "前のページ", "繰越", "繰り越"];

/// The exact-match key of a label: its stem and qualifiers. Two labels
/// match exactly (score 1 without a hold) when their keys are equal.
pub(super) type Key = (String, Vec<String>);

pub(super) fn key(label: &Label) -> Key {
    (label.stem.clone(), label.qualifiers.clone())
}

/// Whether a fragment is a totals label, and of which kind: its normalized
/// label (bracketed qualifiers ignored: `合計（税込）`, `消費税（10%）`) without
/// a trailing tax rate (`消費税 10%`) and without an honorific before a
/// kanji anywhere in it (`今回御請求額`, `今回ご請求金額`) is one of [`TOTAL`]
/// (a closing total), one of [`SUBTOTAL`] or ends with one of
/// [`SUBTOTAL_ENDINGS`]. A fragment with a delimiter is read by its label
/// side (`合計：¥2,970`).
fn totals(text: &str) -> Option<Totals> {
    let kind = |text: &str| {
        let label = normalize_label(text);
        let plain = without_honorifics(&label.stem);
        let stem = without_rate(&plain);
        if TOTAL.contains(&stem) {
            Some(Totals::Total)
        } else if SUBTOTAL.contains(&stem)
            || SUBTOTAL_ENDINGS.iter().any(|ending| stem.ends_with(ending))
        {
            Some(Totals::Subtotal)
        } else {
            None
        }
    };
    kind(text).or_else(|| {
        let (label, _) = text.split_once([':', '：'])?;
        kind(label)
    })
}

/// A normalized label without `御` and without `ご` `お` before a kanji
/// (`今回御請求額` → `今回請求額`); `ご` `お` elsewhere stay (`りんご`).
fn without_honorifics(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut chars = stem.chars().peekable();
    while let Some(c) = chars.next() {
        let honorific = match c {
            '御' => true,
            'ご' | 'お' => chars.peek().is_some_and(|&next| is_kanji(next)),
            _ => false,
        };
        if !honorific {
            out.push(c);
        }
    }
    out
}

/// A CJK unified ideograph (the common and extension A blocks).
fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// A normalized label without a trailing rate (`消費税10%` → `消費税`).
fn without_rate(stem: &str) -> &str {
    let Some(rest) = stem.strip_suffix('%') else {
        return stem;
    };
    let label = rest.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if label.len() == rest.len() || label.is_empty() {
        stem
    } else {
        label
    }
}

/// The document's lines indexed for header matching, built once per document.
pub(super) struct Tables<'a> {
    fragments: &'a [SourceFragment],
    lines: &'a [Line],
    texts: &'a HashMap<&'a str, &'a SourceFragment>,
    /// The line of each fragment (by index); `None` when it is not usable.
    line_of: Vec<Option<usize>>,
    /// Fragments on lines by the exact-match key of their whole text.
    labels: HashMap<Key, Vec<usize>>,
    /// Whether any page has a unit note (`（単位：千円）`, `（千円）`).
    scaled_document: bool,
    /// Pages that say the document continues ([`CONTINUATION`]), and the
    /// page before each page that says it continues that page
    /// ([`CONTINUED_FROM`]).
    continued_pages: BTreeSet<u32>,
    /// The last page with any fragment, usable or not.
    last_page: u32,
}

impl<'a> Tables<'a> {
    /// Costs one unit per line and per fragment, five per byte of the text
    /// of each fragment on a line (checked for a delimiter, normalized,
    /// copied into its key and scanned twice for continuation text) and one
    /// per page that says it continues the page before it. Only plain
    /// fragments (no `label：` delimiter) are header cells.
    pub(super) fn new(
        fragments: &'a [SourceFragment],
        lines: &'a [Line],
        texts: &'a HashMap<&'a str, &'a SourceFragment>,
        scaled_document: bool,
        budget: &mut Budget,
    ) -> Result<Self, BudgetError> {
        budget.spend(fragments.len() as u64)?;
        let mut line_of = vec![None; fragments.len()];
        let mut labels: HashMap<Key, Vec<usize>> = HashMap::new();
        let mut continued_pages = BTreeSet::new();
        // Pages that say they continue the page before them.
        let mut continued_from = BTreeSet::new();
        let pages: BTreeSet<u32> = fragments.iter().map(|f| f.page).collect();
        for (index, line) in lines.iter().enumerate() {
            budget.spend(1 + line.fragments.len() as u64)?;
            for &f in &line.fragments {
                let fragment = &fragments[f];
                budget.spend(5 * fragment.raw_text.len() as u64)?;
                line_of[f] = Some(index);
                if layout::is_plain(fragment) {
                    labels
                        .entry(key(&normalize_label(&fragment.raw_text)))
                        .or_default()
                        .push(f);
                }
                if CONTINUATION.iter().any(|m| fragment.raw_text.contains(m)) {
                    continued_pages.insert(fragment.page);
                }
                if CONTINUED_FROM.iter().any(|m| fragment.raw_text.contains(m)) {
                    continued_from.insert(fragment.page);
                }
            }
        }
        // The rows of the nearest earlier page with any fragment go on.
        budget.spend(continued_from.len() as u64)?;
        for page in continued_from {
            if let Some(&before) = pages.range(..page).next_back() {
                continued_pages.insert(before);
            }
        }
        Ok(Self {
            fragments,
            lines,
            texts,
            line_of,
            labels,
            scaled_document,
            continued_pages,
            last_page: fragments.iter().map(|f| f.page).max().unwrap_or(0),
        })
    }

    /// Fill one table field (see the module documentation).
    pub(super) fn fill(
        &self,
        field: &FormField,
        budget: &mut Budget,
    ) -> Result<FieldFill, FillError> {
        // Per line, the fragments matching each column (its label or a hint).
        let mut lines: BTreeMap<usize, Vec<Vec<usize>>> = BTreeMap::new();
        for (c, column) in field.columns.iter().enumerate() {
            let mut found = false;
            for name in column.names() {
                budget.spend(2 * name.len() as u64)?;
                let Some(matches) = self.labels.get(&key(&normalize_label(name))) else {
                    continue;
                };
                found = true;
                budget.spend(matches.len() as u64)?;
                for &f in matches {
                    if let Some(line) = self.line_of[f] {
                        lines
                            .entry(line)
                            .or_insert_with(|| vec![Vec::new(); field.columns.len()])[c]
                            .push(f);
                    }
                }
            }
            if !found {
                return Ok(empty(FillStatus::NotFound));
            }
        }
        // A column label matched twice on one line counts as two headers
        // (tables side by side); such a line is not read.
        let mut count = 0usize;
        let mut headers = Vec::new();
        for (line, cells) in lines {
            budget.spend(cells.len() as u64)?;
            if cells.iter().any(Vec::is_empty) {
                continue;
            }
            if cells.iter().any(|c| c.len() > 1) {
                count += 2;
                continue;
            }
            count += 1;
            headers.push((line, cells.iter().map(|c| c[0]).collect::<Vec<_>>()));
        }
        if count == 0 {
            return Ok(empty(FillStatus::NotFound));
        }
        let mut candidates = Vec::new();
        for (line, cells) in headers.iter().take(MAX_CANDIDATES) {
            budget.check_deadline()?;
            candidates.push(self.read(field, *line, cells, budget)?);
        }
        let status = match &candidates[..] {
            [only] if count == 1 => match only.hold {
                None if only.rows.is_empty() => return Ok(empty(FillStatus::NotFound)),
                None => {
                    let only = candidates.remove(0);
                    return Ok(FieldFill {
                        method: Some("table_match"),
                        evidence: only.evidence,
                        rows: Some(only.rows),
                        ..empty(FillStatus::Filled)
                    });
                }
                Some(Hold::InvalidCell) => FillStatus::Invalid,
                Some(_) => FillStatus::Ambiguous,
            },
            _ => FillStatus::Ambiguous,
        };
        Ok(FieldFill {
            table_candidates: candidates,
            ..empty(status)
        })
    }

    /// Read the table under `line`, whose header cells for the field's
    /// columns are `cells`.
    fn read(
        &self,
        field: &FormField,
        line: usize,
        cells: &[usize],
        budget: &mut Budget,
    ) -> Result<TableCandidate, FillError> {
        let header = &self.lines[line];
        let mut positions = Vec::with_capacity(cells.len());
        // Whether the header cell (equal to the column label or hint) states
        // a scale unit (`金額（千円）`), or the document has a unit note.
        let mut scaled = Vec::with_capacity(cells.len());
        let mut evidence = Vec::with_capacity(cells.len());
        for &f in cells {
            budget.spend(header.fragments.len() as u64)?;
            positions.push(header.fragments.iter().position(|&g| g == f));
            let fragment = &self.fragments[f];
            budget.spend_bytes(&fragment.raw_text)?;
            scaled.push(self.scaled_document || states_scale(&normalize_label(&fragment.raw_text)));
            let span = layout::fragment_span(fragment, budget)?;
            evidence.extend(self.evidence(header.page, &span, budget)?);
        }
        // Column roles for telling an item row from a totals line: the
        // header's first column and requested text columns name an item,
        // requested amount columns carry totals as well.
        let mut roles = vec![ColumnRole::Other; header.fragments.len()];
        for (column, position) in field.columns.iter().zip(&positions) {
            if let Some(role) = position.and_then(|p| roles.get_mut(p)) {
                *role = match column.column_type {
                    FieldType::Amount => ColumnRole::Amount,
                    FieldType::Text => ColumnRole::Key,
                    _ => ColumnRole::Item,
                };
            }
        }
        if let Some(first) = roles.first_mut() {
            *first = ColumnRole::Key;
        }
        let table = layout::read_table(self.fragments, self.lines, line, &roles, budget, |f| {
            totals(&f.raw_text)
        })?;
        // A row is an item row only with a cell in a requested typed column
        // (when the field has one) and a cell in the header's first column or
        // in a requested text column; the table is irregular from the first
        // other row (a remark, a totals line with an unlisted label, the rows
        // of a table beside this one).
        let typed = field
            .columns
            .iter()
            .any(|c| c.column_type != FieldType::Text);
        let has_cell = |row: &[Option<Span>], text: bool| {
            field
                .columns
                .iter()
                .zip(&positions)
                .any(|(column, position)| {
                    (column.column_type == FieldType::Text) == text
                        && position
                            .and_then(|p| row.get(p))
                            .is_some_and(Option::is_some)
                })
        };
        let mut kept = table.rows.len();
        for (n, row) in table.rows.iter().enumerate() {
            budget.spend(field.columns.len() as u64)?;
            let item = if typed {
                has_cell(row, false)
                    && (row.first().is_some_and(Option::is_some) || has_cell(row, true))
            } else {
                has_cell(row, true)
            };
            if !item {
                kept = n;
                break;
            }
        }
        let mut invalid = false;
        let mut rows = Vec::with_capacity(kept);
        for row in &table.rows[..kept] {
            let mut out = TableRow::new();
            for ((column, position), &scaled) in field.columns.iter().zip(&positions).zip(&scaled) {
                let span = position.and_then(|p| row.get(p)).and_then(Option::as_ref);
                let cell = match span {
                    Some(span) => {
                        let cell =
                            self.cell(column.column_type, scaled, table.page, span, budget)?;
                        // A cell whose span does not resolve is never shown blank.
                        invalid |= cell.as_ref().is_none_or(|c| c.value.is_none());
                        cell
                    }
                    None => None,
                };
                budget.spend(column.id.len() as u64)?;
                out.insert(column.id.clone(), cell);
            }
            rows.push(out);
        }
        // Only a closing total closes rows on a page followed by another;
        // a page that says it continues never closes them.
        let continues = self.continued_pages.contains(&table.page)
            || self.last_page > table.page && table.end != TableEnd::Total;
        let hold = match table.end {
            TableEnd::Irregular => Some(Hold::IrregularTable),
            _ if kept < table.rows.len() => Some(Hold::IrregularTable),
            TableEnd::RowLimit => Some(Hold::TooManyRows),
            _ if continues => Some(Hold::ContinuesOnNextPage),
            _ if invalid => Some(Hold::InvalidCell),
            _ if !typed && !rows.is_empty() => Some(Hold::UntypedTable),
            _ => None,
        };
        Ok(TableCandidate {
            hold,
            rows,
            evidence,
        })
    }

    /// One cell: its source text and the column type's normalized value;
    /// `None` when its span does not resolve. Amounts and numbers are not
    /// normalized when `scaled`.
    fn cell(
        &self,
        column_type: FieldType,
        scaled: bool,
        page: u32,
        span: &Span,
        budget: &mut Budget,
    ) -> Result<Option<TableCell>, BudgetError> {
        let Some(evidence) = self.evidence(page, span, budget)? else {
            return Ok(None);
        };
        let raw = evidence.text.clone();
        // Copy and normalization.
        budget.spend(3 * raw.len() as u64)?;
        let value = match column_type {
            FieldType::Amount | FieldType::Number if scaled => None,
            FieldType::Text | FieldType::Table => Some(Value::String(raw.clone())),
            other => normalize_value(other, &raw),
        };
        Ok(Some(TableCell {
            raw,
            value,
            evidence: vec![evidence],
        }))
    }

    fn evidence(
        &self,
        page: u32,
        span: &Span,
        budget: &mut Budget,
    ) -> Result<Option<FillEvidence>, BudgetError> {
        let Some(text) = slice(self.texts, span) else {
            return Ok(None);
        };
        budget.spend(2 * text.len() as u64 + span.fragment_id.len() as u64)?;
        Ok(Some(FillEvidence {
            page,
            fragment_id: span.fragment_id.clone(),
            start: span.start,
            end: span.end,
            text,
            bbox: span.bbox,
            precision: span.precision,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FillLimits, FormError, MAX_HINTS, fill, fill_with_limits, parse_form};
    use super::*;
    use serde_json::json;
    use std::time::Instant;
    use vf_source::Quality;
    use vf_source::geometry::Rect;

    /// A 10 pt high fragment, 10 pt per character, with its top at `top`.
    fn at(id: &str, page: u32, text: &str, left: f64, top: f64) -> SourceFragment {
        SourceFragment {
            id: id.into(),
            page,
            item_index: 0,
            raw_text: text.into(),
            bbox: Rect {
                left,
                top,
                right: left + 10.0 * text.chars().count() as f64,
                bottom: top + 10.0,
            },
            quality: Quality::Usable,
            orientation: vf_source::Orientation::Deg0,
        }
    }

    /// A header `品名 / 数量 / 金額` at top `top` on `page` with `rows`
    /// 18 pt apart; `""` leaves a cell blank.
    fn table(page: u32, top: f64, rows: &[[&str; 3]]) -> Vec<SourceFragment> {
        let columns = [60.0, 300.0, 440.0];
        let mut fragments: Vec<SourceFragment> = ["品名", "数量", "金額"]
            .iter()
            .zip(columns)
            .map(|(text, left)| at(&format!("p{page}t{top}h{left}"), page, text, left, top))
            .collect();
        for (row, cells) in rows.iter().enumerate() {
            let y = top + 18.0 * (row + 1) as f64;
            for (text, left) in cells.iter().zip(columns) {
                if !text.is_empty() {
                    let id = format!("p{page}t{top}r{row}c{left}");
                    fragments.push(at(&id, page, text, left, y));
                }
            }
        }
        fragments
    }

    fn items_form() -> super::super::Form {
        parse_form(
            json!({"fields": [
                {"id": "items", "label": "明細", "type": "table", "columns": [
                    {"id": "name", "label": "品名"},
                    {"id": "qty", "label": "数量", "type": "number"},
                    {"id": "amount", "label": "金額", "type": "amount"}
                ]},
                {"id": "amount", "label": "金額", "type": "amount"}
            ]})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    }

    fn rows_json(fill: &FieldFill) -> serde_json::Value {
        let rows = fill.rows.as_ref().map_or_else(
            || fill.table_candidates[0].rows.clone(),
            std::clone::Clone::clone,
        );
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|(id, cell)| {
                        let cell = cell.as_ref().map(|c| json!([c.raw, c.value]));
                        (id.clone(), cell.unwrap_or(serde_json::Value::Null))
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            })
            .collect::<Vec<serde_json::Value>>()
            .into()
    }

    #[test]
    fn table_forms_are_validated() {
        let parse = |field: serde_json::Value| {
            parse_form(json!({ "fields": [field] }).to_string().as_bytes())
        };
        let columns = json!([{"id": "name", "label": "品名"}, {"id": "qty", "label": "数量", "type": "number"}]);
        assert!(
            parse(json!({"id": "t", "label": "明細", "type": "table", "columns": columns})).is_ok()
        );
        for field in [
            json!({"id": "t", "label": "明細", "type": "table"}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": []}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": columns, "hints": ["品目"]}),
            json!({"id": "t", "label": "明細", "columns": columns}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名"}, {"id": "a", "label": "数量"}]}),
            // Labels equal after normalization would match the same header cell.
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名"}, {"id": "b", "label": "ご品 名："}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "type": "table"}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "（）"}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": (0..=MAX_COLUMNS)
                .map(|i| json!({"id": format!("c{i}"), "label": format!("列{i}")}))
                .collect::<Vec<_>>()}),
            // A hint equal to another column's label or hint, or to its own label.
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "hints": ["品目"]},
                {"id": "b", "label": "数量", "hints": ["品目"]}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "hints": ["数量"]},
                {"id": "b", "label": "数量"}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "hints": ["品 名"]}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "hints": [" "]}]}),
            json!({"id": "t", "label": "明細", "type": "table", "columns": [
                {"id": "a", "label": "品名", "hints": (0..=MAX_HINTS)
                    .map(|i| format!("品目{i}"))
                    .collect::<Vec<_>>()}]}),
        ] {
            assert_eq!(
                parse(field.clone()).unwrap_err(),
                FormError::Columns,
                "{field}"
            );
        }
        let hints = json!({"id": "t", "label": "明細", "type": "table", "columns": [
            {"id": "a", "label": "品名", "hints": ["品目", "商品名"]},
            {"id": "b", "label": "数量", "type": "number", "hints": ["数"]}]});
        assert!(parse(hints).is_ok());
        let unknown = json!({"id": "t", "label": "明細", "type": "table", "columns": [
            {"id": "a", "label": "品名", "names": ["品目"]}]});
        assert_eq!(parse(unknown).unwrap_err(), FormError::Invalid);
    }

    #[test]
    fn a_header_cell_may_equal_a_column_hint() {
        let mut fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        fragments[0].raw_text = "品目".into();
        let form = |hints: serde_json::Value| {
            parse_form(
                json!({"fields": [{"id": "items", "label": "明細", "type": "table", "columns": [
                    {"id": "name", "label": "品名", "hints": hints},
                    {"id": "amount", "label": "金額", "type": "amount"}
                ]}]})
                .to_string()
                .as_bytes(),
            )
            .unwrap()
        };
        let items = &fill("doc", &fragments, &form(json!(["商品名", "品目"]))).fields["items"];
        assert_eq!(items.status, FillStatus::Filled);
        assert_eq!(items.evidence[0].text, "品目");
        assert_eq!(
            rows_json(items),
            json!([
                {"name": ["りんご", "りんご"], "amount": ["¥300", 300]},
                {"name": ["みかん", "みかん"], "amount": ["¥500", 500]}
            ])
        );
        // Without the hint the header is not found.
        let items = &fill("doc", &fragments, &form(json!([]))).fields["items"];
        assert_eq!(items.status, FillStatus::NotFound);
        // The label and a hint both on the header line: the column matches
        // two cells, so the line is not read.
        fragments.push(at("x", 1, "品名", 200.0, 100.0));
        let items = &fill("doc", &fragments, &form(json!(["品目"]))).fields["items"];
        assert_eq!(items.status, FillStatus::Ambiguous);
        assert!(items.table_candidates.is_empty() && items.rows.is_none());
    }

    #[test]
    fn a_header_cell_stating_a_scale_blocks_amounts() {
        let mut fragments = table(1, 100.0, &[["りんご", "3", "300"], ["みかん", "2", "500"]]);
        fragments[2].raw_text = "金額（千円）".into();
        let form = parse_form(
            json!({"fields": [{"id": "items", "label": "明細", "type": "table", "columns": [
                {"id": "name", "label": "品名"},
                {"id": "amount", "label": "金額", "type": "amount", "hints": ["金額（千円）"]}
            ]}]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let items = &fill("doc", &fragments, &form).fields["items"];
        assert_eq!(items.status, FillStatus::Invalid);
        assert_eq!(items.table_candidates[0].hold, Some(Hold::InvalidCell));
    }

    #[test]
    fn a_regular_table_is_filled_with_rows_and_blank_cells() {
        let mut fragments = table(
            1,
            100.0,
            &[
                ["りんご", "3", "¥300"],
                ["みかん", "", "¥500"],
                ["ぶどう", "2個", "¥1,800"],
            ],
        );
        fragments.extend([
            at("t", 1, "小計", 300.0, 172.0),
            at("tv", 1, "¥2,600", 440.0, 172.0),
        ]);
        let result = fill("doc", &fragments, &items_form());
        let items = &result.fields["items"];
        assert_eq!(items.status, FillStatus::Filled);
        assert_eq!(items.method, Some("table_match"));
        assert_eq!(
            rows_json(items),
            json!([
                {"name": ["りんご", "りんご"], "qty": ["3", 3], "amount": ["¥300", 300]},
                {"name": ["みかん", "みかん"], "qty": null, "amount": ["¥500", 500]},
                {"name": ["ぶどう", "ぶどう"], "qty": ["2個", 2], "amount": ["¥1,800", 1800]}
            ])
        );
        // Header evidence in form order; cell evidence is the whole fragment.
        let header: Vec<&str> = items.evidence.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(header, ["品名", "数量", "金額"]);
        let cell = items.rows.as_ref().unwrap()[0]["name"].as_ref().unwrap();
        assert_eq!(cell.evidence[0].bbox, fragments[3].bbox);
        assert_eq!(cell.evidence[0].precision, layout::Precision::Run);
        // The scalar 金額 still never takes a data cell (column → ambiguous).
        let amount = &result.fields["amount"];
        assert_eq!(amount.status, FillStatus::Ambiguous);
        assert_eq!(amount.candidates[0].hold, Some(Hold::GeometricOnly));
        // JSON: rows with null cells, no candidates.
        let json = serde_json::to_value(items).unwrap();
        assert_eq!(json["rows"][1]["qty"], serde_json::Value::Null);
        assert_eq!(json["rows"][0]["amount"]["evidence"][0]["text"], "¥300");
        assert!(json.get("candidates").is_none() && json.get("raw").is_none());
    }

    #[test]
    fn candidates_are_one_json_key() {
        let one = |fill: &FieldFill| {
            let text = serde_json::to_string(fill).unwrap();
            assert_eq!(text.matches("\"candidates\"").count(), 1, "{text}");
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        };
        // A held table: its tables are `candidates`.
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "500円〜"]],
        );
        let items = one(&fill("doc", &fragments, &items_form()).fields["items"]);
        assert_eq!(items["candidates"][0]["hold"], "invalid_cell");
        assert!(items.get("rows").is_none() && items.get("method").is_none());
        // A scalar field's value candidates (the 金額 column) are `candidates`.
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        let amount = one(&fill("doc", &fragments, &items_form()).fields["amount"]);
        assert_eq!(amount["candidates"][0]["hold"], "geometric_only");
    }

    #[test]
    fn a_unit_note_on_another_page_holds_the_table() {
        // One detection path for unit notes: a bracketed scale anywhere in
        // the document, also on another page, leaves table amounts
        // unnormalized (before the combination: its page only).
        let mut fragments = table(1, 100.0, &[["りんご", "3", "300"], ["みかん", "2", "500"]]);
        fragments.push(at("n", 2, "（千円）", 440.0, 60.0));
        fragments.push(at("e", 2, "以上", 60.0, 100.0));
        fragments.push(at("t", 1, "合計", 60.0, 154.0));
        let items = &fill("doc", &fragments, &items_form()).fields["items"];
        assert_eq!(items.status, FillStatus::Invalid);
        assert_eq!(items.table_candidates[0].hold, Some(Hold::InvalidCell));
    }

    #[test]
    fn column_labels_stop_whitespace_splits() {
        // A header row read as one fragment: `品名` is a scalar field and
        // `数量`, `金額` are table columns, so the rest is not offered as a
        // `no_delimiter` value of `品名`.
        let form = parse_form(
            json!({"fields": [
                {"id": "items", "label": "明細", "type": "table", "columns": [
                    {"id": "qty", "label": "数量", "type": "number"},
                    {"id": "amount", "label": "金額", "type": "amount"}
                ]},
                {"id": "name", "label": "品名"}
            ]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let fragments = [at("h", 1, "品名 数量 金額", 60.0, 100.0)];
        let name = &fill("doc", &fragments, &form).fields["name"];
        assert_eq!(name.status, FillStatus::NotFound, "{name:?}");
        // Without the columns the rest is a `no_delimiter` candidate.
        let alone = parse_form(r#"{"fields":[{"id":"name","label":"品名"}]}"#.as_bytes()).unwrap();
        let name = &fill("doc", &fragments, &alone).fields["name"];
        assert_eq!(name.candidates[0].hold, Some(Hold::NoDelimiter));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "never both")]
    fn value_and_table_candidates_together_are_refused() {
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "500円〜"]],
        );
        let result = fill("doc", &fragments, &items_form());
        let mut both = result.fields["amount"].clone();
        both.table_candidates = result.fields["items"].table_candidates.clone();
        assert!(!both.candidates.is_empty() && !both.table_candidates.is_empty());
        let _ = serde_json::to_string(&both);
    }

    #[test]
    fn headers_match_exactly_with_an_honorific_and_equal_qualifiers() {
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        let form = |name: &str| {
            parse_form(
                json!({"fields": [{"id": "items", "label": "明細", "type": "table", "columns": [
                    {"id": "name", "label": name}, {"id": "amount", "label": "金額", "type": "amount"}
                ]}]})
                .to_string()
                .as_bytes(),
            )
            .unwrap()
        };
        let status = |name: &str| fill("doc", &fragments, &form(name)).fields["items"].status;
        assert_eq!(status("ご品名"), FillStatus::Filled);
        assert_eq!(status("品 名"), FillStatus::Filled);
        // A partial label or a qualifier on one side is not a header cell.
        assert_eq!(status("品"), FillStatus::NotFound);
        assert_eq!(status("品名（税込）"), FillStatus::NotFound);
        assert_eq!(status("商品名"), FillStatus::NotFound);
    }

    #[test]
    fn two_headers_or_a_repeated_column_label_are_ambiguous() {
        let mut fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        fragments.extend([
            at("t", 1, "合計", 300.0, 154.0),
            at("tv", 1, "¥800", 440.0, 154.0),
        ]);
        fragments.extend(table(
            1,
            300.0,
            &[["もも", "1", "¥400"], ["なし", "1", "¥200"]],
        ));
        let items = &fill("doc", &fragments, &items_form()).fields["items"];
        assert_eq!(items.status, FillStatus::Ambiguous);
        assert_eq!(items.table_candidates.len(), 2);
        assert!(items.table_candidates.iter().all(|c| c.hold.is_none()));
        assert!(items.rows.is_none() && items.evidence.is_empty());
        // Two 金額 cells on one header line: tables side by side.
        let mut fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        fragments.push(at("x", 1, "金額", 520.0, 100.0));
        let items = &fill("doc", &fragments, &items_form()).fields["items"];
        assert_eq!(items.status, FillStatus::Ambiguous);
        assert!(items.table_candidates.is_empty());
    }

    #[test]
    fn holds_say_why_a_table_was_not_filled() {
        let hold = |fragments: &[SourceFragment]| {
            let items = &fill("doc", fragments, &items_form()).fields["items"];
            (
                items.status,
                items.table_candidates.first().and_then(|c| c.hold),
            )
        };
        // A cell failing its column's grammar.
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "500円〜"]],
        );
        assert_eq!(
            hold(&fragments),
            (FillStatus::Invalid, Some(Hold::InvalidCell))
        );
        // A page with a unit note: amounts are multiples, never normalized.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "300"], ["みかん", "2", "500"]]);
        fragments.push(at("u", 1, "（単位：千円）", 440.0, 60.0));
        assert_eq!(
            hold(&fragments),
            (FillStatus::Invalid, Some(Hold::InvalidCell))
        );
        // A wrapped first-column cell.
        let mut fragments = table(1, 100.0, &[["りんご（青森産", "3", "¥300"]]);
        fragments.push(at("w", 1, "特選）", 60.0, 136.0));
        assert_eq!(
            hold(&fragments),
            (FillStatus::Ambiguous, Some(Hold::IrregularTable))
        );
        // Rows running to the end of the page's text with a later page.
        let mut fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        fragments.push(at("n", 2, "備考", 60.0, 100.0));
        assert_eq!(
            hold(&fragments),
            (FillStatus::Ambiguous, Some(Hold::ContinuesOnNextPage))
        );
        // A later page that is not usable text still counts.
        fragments.last_mut().unwrap().quality = Quality::Suspect;
        assert_eq!(
            hold(&fragments),
            (FillStatus::Ambiguous, Some(Hold::ContinuesOnNextPage))
        );
        // ... and so does a page that says it continues, even closed by totals.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("t", 1, "小計", 300.0, 136.0),
            at("tv", 1, "¥300", 440.0, 136.0),
            at("c", 1, "（次頁へ続く）", 300.0, 400.0),
        ]);
        assert_eq!(
            hold(&fragments),
            (FillStatus::Ambiguous, Some(Hold::ContinuesOnNextPage))
        );
        // ... and so does a closing total when the next page says it
        // continues this one, also across a page without text.
        for (page, marker) in [(2, "（前頁より続く）"), (3, "前頁より繰越"), (2, "繰越")]
        {
            let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
            fragments.extend([
                at("t", 1, "合計", 300.0, 136.0),
                at("tv", 1, "¥300", 440.0, 136.0),
                at("c", page, marker, 60.0, 80.0),
            ]);
            assert_eq!(
                hold(&fragments),
                (FillStatus::Ambiguous, Some(Hold::ContinuesOnNextPage)),
                "{marker}"
            );
        }
        // A marker on the table's own page says nothing about it.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("t", 1, "合計", 300.0, 136.0),
            at("tv", 1, "¥300", 440.0, 136.0),
            at("c", 1, "前月繰越", 60.0, 60.0),
        ]);
        assert_eq!(hold(&fragments), (FillStatus::Filled, None));
        // Totals close the rows even when a later page exists.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("t", 1, "合計（税込）", 300.0, 136.0),
            at("tv", 1, "¥300", 440.0, 136.0),
            at("n", 2, "備考", 60.0, 100.0),
        ]);
        assert_eq!(hold(&fragments), (FillStatus::Filled, None));
        // A header without rows is not found.
        let fragments = table(1, 100.0, &[]);
        assert_eq!(hold(&fragments), (FillStatus::NotFound, None));
        // A header missing one column is not found.
        let fragments: Vec<SourceFragment> = table(1, 100.0, &[["りんご", "3", "¥300"]])
            .into_iter()
            .filter(|f| f.raw_text != "数量")
            .collect();
        assert_eq!(hold(&fragments), (FillStatus::NotFound, None));
    }

    #[test]
    fn totals_labels() {
        for text in [
            "合計",
            "合計（税込）",
            "合計：",
            "合計：¥2,970",
            "ご請求金額",
            "御請求額",
            "お支払金額",
            "差引請求額",
            "税込金額",
            "税込合計",
            "総 合 計",
            "今回御請求額",
            "今回ご請求金額",
            "御見積金額",
            "お見積額",
            "税込総額",
            "ご請求総額",
            "今回お支払金額",
            "以下余白",
            "以上",
        ] {
            assert_eq!(totals(text), Some(Totals::Total), "{text}");
        }
        for text in [
            "計",
            "小計",
            "小計：",
            "10%対象",
            "8%対象計",
            "※印は軽減税率対象",
            "税抜金額",
            "内消費税",
            "消費税（10%）",
            "消費税 10%",
            "消費税１０％",
            "消費税8.5%",
            "源泉徴収税額",
            "値引合計",
            "10%対象合計",
        ] {
            assert_eq!(totals(text), Some(Totals::Subtotal), "{text}");
        }
        for text in [
            "りんご",
            "送料",
            "値引き",
            "10%",
            "%",
            "1,000円以上",
            "合計数量の内訳です（参考）明細",
            "品番:A-100",
            "9:00",
            "りんご酢",
            "おにぎり",
            "ごま油",
            "御中",
        ] {
            assert_eq!(totals(text), None, "{text}");
        }
        assert_eq!(without_honorifics("今回御請求額"), "今回請求額");
        assert_eq!(without_honorifics("今回ご請求金額"), "今回請求金額");
        // `ご` `お` before kana (or at the end) are part of the word.
        assert_eq!(without_honorifics("りんごおにぎり"), "りんごおにぎり");
        assert_eq!(without_honorifics("お見積"), "見積");
    }

    /// Fill `items` (品名 / 数量 / 金額) of `fragments`: status, first hold,
    /// and the rows filled or read.
    fn outcome(fragments: &[SourceFragment]) -> (FillStatus, Option<Hold>, usize) {
        let items = &fill("doc", fragments, &items_form()).fields["items"];
        let rows = items
            .rows
            .as_ref()
            .or_else(|| items.table_candidates.first().map(|c| &c.rows))
            .map_or(0, Vec::len);
        (
            items.status,
            items.table_candidates.first().and_then(|c| c.hold),
            rows,
        )
    }

    #[test]
    fn only_a_closing_total_closes_rows_followed_by_a_page() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        let with = |lines: &[(&str, f64)], page2: bool| {
            let mut fragments = table(1, 100.0, &rows);
            for (n, (label, top)) in lines.iter().enumerate() {
                fragments.extend([
                    at(&format!("t{n}"), 1, label, 300.0, *top),
                    at(&format!("v{n}"), 1, "¥800", 440.0, *top),
                ]);
            }
            if page2 {
                fragments.push(at("n", 2, "ぶどう", 60.0, 100.0));
            }
            outcome(&fragments)
        };
        let filled = (FillStatus::Filled, None, 2);
        let continues = (FillStatus::Ambiguous, Some(Hold::ContinuesOnNextPage), 2);
        // A page subtotal with a later page may continue there.
        assert_eq!(with(&[("小計", 154.0)], true), continues);
        assert_eq!(with(&[("小計", 154.0)], false), filled);
        assert_eq!(
            with(&[("小計", 154.0), ("消費税 10%", 172.0)], true),
            continues
        );
        // A closing total below the subtotal, also after a gap, closes them.
        assert_eq!(with(&[("小計", 154.0), ("合計", 172.0)], true), filled);
        assert_eq!(
            with(&[("小計", 154.0), ("お支払金額", 200.0)], true),
            filled
        );
        // Text in no column with a later page.
        let mut fragments = table(1, 100.0, &rows);
        fragments.extend([
            at("n", 1, "備考：午前着", 60.0, 154.0),
            at("p", 2, "ぶどう", 60.0, 100.0),
        ]);
        assert_eq!(outcome(&fragments), continues);
    }

    #[test]
    fn rows_without_item_cells_make_the_table_irregular() {
        let irregular = |rows| (FillStatus::Ambiguous, Some(Hold::IrregularTable), rows);
        // A totals line with an unlisted label in an unrequested column: no
        // cell in the first column or a requested text column.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("u", 1, "単価", 200.0, 100.0),
            at("x", 1, "差引残高", 200.0, 136.0),
            at("xv", 1, "¥300", 440.0, 136.0),
        ]);
        assert_eq!(outcome(&fragments), irregular(1));
        // A remark row: no cell in a requested typed column.
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("u", 1, "備考欄", 200.0, 100.0),
            at("x", 1, "備考", 60.0, 136.0),
            at("xv", 1, "12月上旬納品", 200.0, 136.0),
        ]);
        assert_eq!(outcome(&fragments), irregular(1));
        // A row with a requested typed cell and a first-column cell is an
        // item, even with blank cells (送料).
        let fragments = table(1, 100.0, &[["りんご", "3", "¥300"], ["送料", "", "¥500"]]);
        assert_eq!(outcome(&fragments), (FillStatus::Filled, None, 2));
    }

    #[test]
    fn text_only_tables_are_held() {
        let fragments = [
            at("h1", 1, "作業内容", 60.0, 100.0),
            at("h2", 1, "担当者", 300.0, 100.0),
            at("a1", 1, "サーバー保守", 60.0, 118.0),
            at("a2", 1, "山田", 300.0, 118.0),
        ];
        let form = |second: serde_json::Value| {
            parse_form(
                json!({"fields": [{"id": "work", "label": "作業", "type": "table", "columns": [
                    {"id": "task", "label": "作業内容"}, second
                ]}]})
                .to_string()
                .as_bytes(),
            )
            .unwrap()
        };
        let work = &fill(
            "doc",
            &fragments,
            &form(json!({"id": "who", "label": "担当者"})),
        )
        .fields["work"];
        assert_eq!(work.status, FillStatus::Ambiguous);
        assert_eq!(work.table_candidates[0].hold, Some(Hold::UntypedTable));
        assert_eq!(work.table_candidates[0].rows.len(), 1);
        // A header without rows stays not_found.
        let work = &fill(
            "doc",
            &fragments[..2],
            &form(json!({"id": "who", "label": "担当者"})),
        )
        .fields["work"];
        assert_eq!(work.status, FillStatus::NotFound);
    }

    #[test]
    fn header_cells_have_no_delimiter() {
        let mut fragments = table(1, 100.0, &[["りんご", "3", "¥300"]]);
        fragments[0].raw_text = "品名：".into();
        assert_eq!(outcome(&fragments), (FillStatus::NotFound, None, 0));
    }

    #[test]
    fn a_bracketed_scale_above_the_table_blocks_amounts() {
        let mut fragments = table(1, 100.0, &[["りんご", "3", "300"], ["みかん", "2", "500"]]);
        fragments.push(at("u", 1, "（千円）", 440.0, 80.0));
        assert_eq!(
            outcome(&fragments),
            (FillStatus::Invalid, Some(Hold::InvalidCell), 2)
        );
    }

    #[test]
    fn table_fill_spends_the_budget() {
        let fragments = table(
            1,
            100.0,
            &[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]],
        );
        let form = items_form();
        let run = |max_work| {
            fill_with_limits(
                "doc",
                &fragments,
                &form,
                &FillLimits {
                    max_work,
                    ..FillLimits::default()
                },
            )
        };
        let mut low = 0;
        let mut high = 1_000_000;
        assert!(run(high).is_ok());
        while low + 1 < high {
            let mid = u64::midpoint(low, high);
            if run(mid).is_ok() {
                high = mid;
            } else {
                low = mid;
            }
        }
        assert_eq!(run(low).unwrap_err(), FillError::BudgetExceeded);
        let late = fill_with_limits(
            "doc",
            &fragments,
            &form,
            &FillLimits {
                deadline: Some(Instant::now()),
                ..FillLimits::default()
            },
        );
        assert_eq!(late.unwrap_err(), FillError::DeadlineExceeded);
    }
}
