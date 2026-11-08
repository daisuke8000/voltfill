//! Table hypotheses: rows of cells under a header line.
//!
//! The caller chooses the header line (for example by matching form
//! labels); every fragment of that line is a column. [`read_table`] reads
//! the lines below it as rows of cells aligned to those columns and reports
//! why the rows ended ([`TableEnd`]). It never decides whether a table may
//! be used: that is the caller's policy, and so is the vocabulary of totals
//! labels ([`Totals`]).
//!
//! Rules ("a wrong value is worse than an empty one"):
//! - A fragment is in a column when its left edges, right edges or centres
//!   are within one line height (the taller of the header cell and the
//!   fragment) of the header cell's, or when the two overlap horizontally.
//! - The header line has 2 to [`MAX_TABLE_COLUMNS`] fragments, none with a
//!   `label：` delimiter; otherwise it is irregular (one column cannot tell
//!   wrapped cells from rows, and `品名：` is a label, not a header cell).
//! - The first row starts within one line height below the header line.
//!   Later lines follow within the gap from the header to the first row (at
//!   least one line height) plus half a line height (the reach), as for
//!   [`super::Placement::Column`].
//! - A line is a totals line when the caller reads any of its fragments as
//!   a totals label ([`Totals`]) and the fragments left of the first such
//!   label have at most one plain fragment in a column. A totals label
//!   precedes its amounts, so fragments left of it are not totals; with two
//!   or more of them in columns (an item row with a totals block beside it,
//!   `ぶどう 1 ¥900 … 合計 ¥1,700`) the line is irregular and may be an item
//!   row. A note in the first column beside `小計` stays a totals line.
//! - Otherwise a line is a row when each of its fragments is plain (no
//!   `label：` delimiter) and in exactly one column, no column has two
//!   fragments, and at least two columns have a cell. Blank cells are `None`.
//! - A line is irregular when a plain fragment is in two or more columns (a
//!   cell spanning columns), two fragments share a column, a fragment
//!   outside every column sits beside aligned cells, only one column has a
//!   cell (a wrapped cell cannot be told from a one-cell row), or the line
//!   has more than [`MAX_TABLE_COLUMNS`] fragments.
//! - Turned text: a line with a turned fragment beside upright ones is
//!   irregular. A line whose fragments are all turned (a margin stamp; such
//!   lines follow the page's upright lines) is skipped unless its box meets
//!   the table's region: from the header's left to its right (the columns)
//!   and from the header's top to the page bottom while rows are read, or to
//!   the bottom of the last row (the header's, without rows) below a stop.
//!   A turned line meeting the region is irregular (it may be in the rows).
//! - Row pitch: the distance between the baselines of the header and the
//!   first row and of consecutive rows. A row closer to the line above than
//!   3/4 of the table's pitch (the upper median of those distances)
//!   continues a row wrapped in several columns at once: the table is
//!   irregular from that row on.
//!
//! The rows end ([`TableEnd`]) at the first line that is not a row within
//! the reach of the line above, or at the first line after a wider gap:
//! - an irregular line, also right after a gap, or a row right after a gap
//!   (the table resumes after a blank): [`TableEnd::Irregular`];
//! - a totals line (also right after a gap), a line within the reach with
//!   no plain fragment in any column ([`TableEnd::Unaligned`], for example
//!   `備考：…`), or such a line after a gap ([`TableEnd::Gap`]): the lines
//!   below are then read as well (see below);
//! - no more lines on the page ([`TableEnd::PageEnd`]) or another row after
//!   [`MAX_TABLE_ROWS`] rows ([`TableEnd::RowLimit`]). Rows never continue
//!   onto another page.
//!
//! Below a totals, unaligned or gap end, the following lines of the page
//! are read in blocks (lines each within the reach of the line above): the
//! stop's block, any block that starts with a totals line, and one more
//! block after a wider gap, unless a closing total ([`Totals::Total`]) has
//! been read by then (another table may follow a closing total). When any
//! line read may be an item row (a row, or an irregular line with plain
//! fragments in two or more columns), items follow the stop and the end
//! is [`TableEnd::Irregular`]. Otherwise the totals lines read decide:
//! [`TableEnd::Total`] when any of them is a closing total,
//! [`TableEnd::Subtotal`] when all are subtotals, and without totals lines
//! the stop's own end ([`TableEnd::Unaligned`] or [`TableEnd::Gap`]).
//!
//! Work: one unit per line read and two per fragment of a line (the turned
//! check and reading), two per byte
//! of a line fragment's text (the caller's totals check and the delimiter
//! check) and one per byte of a header fragment's, one per column compared
//! with a fragment, one per row for the pitch, and the cost of
//! [`super::fragment_span`] per cell.
use super::{Kind, Line, Span, classify, fragment_span};
use crate::budget::{Budget, BudgetError};
use crate::geometry::Rect;
use crate::source::{Orientation, SourceFragment};
use serde::Serialize;

/// Most fragments on a header line (columns) or on a data line.
pub const MAX_TABLE_COLUMNS: usize = 32;
/// Most data rows read under one header line.
pub const MAX_TABLE_ROWS: usize = 100;

/// How the caller reads a totals label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Totals {
    /// A partial sum (`小計`, `消費税`): items may follow it, for example on a
    /// later page.
    Subtotal,
    /// A closing total or end mark (`合計`, `以下余白`): the items end.
    Total,
}

/// Why the rows of a table ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableEnd {
    /// Totals lines, directly or after a gap, none of them a closing total
    /// ([`Totals::Subtotal`]), and no item row below them.
    Subtotal,
    /// Totals lines, at least one of them a closing total
    /// ([`Totals::Total`]), and no item row below them.
    Total,
    /// A line within the reach with no plain fragment in any column, and no
    /// item row or totals line below it.
    Unaligned,
    /// A gap wider than the reach, then a line with no plain fragment in any
    /// column, and no item row or totals line below it.
    Gap,
    /// No more lines on the page.
    PageEnd,
    /// The header line or a line within the table is not regular, the rows
    /// resume after a gap, a row is closer than the row pitch, or an item
    /// row follows the totals or unaligned line where the rows stopped (see
    /// the module documentation); the rows before it are kept.
    Irregular,
    /// Another row after [`MAX_TABLE_ROWS`] rows.
    RowLimit,
}

/// Rows read under a header line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Table {
    pub page: u32,
    /// One cell per header fragment, left to right; `None` for a blank cell.
    /// Each cell is the whole text of one fragment.
    pub rows: Vec<Vec<Option<Span>>>,
    pub end: TableEnd,
}

/// How one line below the header reads.
enum Read {
    /// Fragment indexes per column.
    Row(Vec<Option<usize>>),
    /// The strongest totals label among the line's fragments; the
    /// fragments left of the first label are not an item row.
    Totals(Totals),
    Unaligned,
    /// `row_like`: two or more plain fragments lie in columns, so the line
    /// may be an item row (or the line is crowded).
    Irregular {
        row_like: bool,
    },
}

/// Read the rows under line `header` of `lines` (as built by
/// [`super::analyze`] from `fragments`). `totals` tells whether a fragment
/// is a totals label (`小計`, `合計`, …) and of which kind; a line with one
/// ends the rows.
///
/// # Errors
/// As [`Budget::spend`].
///
/// # Panics
/// When `header` is not an index into `lines`.
pub fn read_table(
    fragments: &[SourceFragment],
    lines: &[Line],
    header: usize,
    budget: &mut Budget,
    mut totals: impl FnMut(&SourceFragment) -> Option<Totals>,
) -> Result<Table, BudgetError> {
    budget.check_deadline()?;
    let head = &lines[header];
    budget.spend(1 + head.fragments.len() as u64)?;
    let mut table = Table {
        page: head.page,
        rows: Vec::new(),
        end: TableEnd::Irregular,
    };
    if !(2..=MAX_TABLE_COLUMNS).contains(&head.fragments.len()) {
        return Ok(table);
    }
    for &f in &head.fragments {
        budget.spend_bytes(&fragments[f].raw_text)?;
        // Columns and rows are read along the page's x and y, so a turned
        // header is never a table header.
        if classify(&fragments[f]) != Kind::Plain || fragments[f].orientation != Orientation::Deg0 {
            return Ok(table);
        }
    }
    let columns: Vec<Rect> = head.fragments.iter().map(|&f| fragments[f].bbox).collect();
    let reader = Reader {
        fragments,
        columns: &columns,
        page: head.page,
        // Rows are open: the region reaches the page bottom.
        region: Rect {
            left: columns.iter().map(|c| c.left).fold(f64::INFINITY, f64::min),
            top: head.bbox.top,
            right: columns
                .iter()
                .map(|c| c.right)
                .fold(f64::NEG_INFINITY, f64::max),
            bottom: f64::INFINITY,
        },
    };
    let body = &lines[header + 1..];
    let mut previous = head.bbox;
    let mut reach: Option<f64> = None;
    // Baselines of the header and of each row, for the row pitch.
    let mut baselines = vec![head.bbox.bottom];
    table.end = TableEnd::PageEnd;
    for (i, line) in body.iter().enumerate() {
        budget.spend(1)?;
        if line.page != head.page {
            break;
        }
        if reader.outside(line, budget)? {
            continue;
        }
        let gap = line.bbox.top - previous.bottom;
        let height = head.bbox.height().max(line.bbox.height());
        let resumes = gap > reach.unwrap_or(height);
        if !resumes {
            // The reach is measured from the header to the first line.
            reach.get_or_insert(height.max(gap) + 0.5 * height);
        }
        let limit = reach.unwrap_or(height);
        table.end = match reader.read(line, budget, &mut totals)? {
            Read::Row(_) if !resumes && table.rows.len() >= MAX_TABLE_ROWS => TableEnd::RowLimit,
            Read::Row(cells) if !resumes => {
                let mut row = Vec::with_capacity(cells.len());
                for cell in cells {
                    row.push(match cell {
                        Some(f) => Some(fragment_span(&fragments[f], budget)?),
                        None => None,
                    });
                }
                table.rows.push(row);
                baselines.push(line.bbox.bottom);
                previous = line.bbox;
                continue;
            }
            Read::Row(_) | Read::Irregular { .. } => TableEnd::Irregular,
            Read::Totals(kind) => reader.stopped(previous).below(
                &body[i..],
                limit,
                Some(kind),
                TableEnd::Subtotal,
                budget,
                &mut totals,
            )?,
            Read::Unaligned => {
                let end = if resumes {
                    TableEnd::Gap
                } else {
                    TableEnd::Unaligned
                };
                reader
                    .stopped(previous)
                    .below(&body[i..], limit, None, end, budget, &mut totals)?
            }
        };
        break;
    }
    budget.spend(baselines.len() as u64)?;
    if let Some(wrapped) = wrapped_row(&baselines) {
        table.rows.truncate(wrapped);
        table.end = TableEnd::Irregular;
    }
    Ok(table)
}

/// The index of the first row closer to the line above it than 3/4 of the
/// row pitch (the upper median of the distances between `baselines`: the
/// header's, then each row's).
fn wrapped_row(baselines: &[f64]) -> Option<usize> {
    let pitches: Vec<f64> = baselines.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted = pitches.clone();
    sorted.sort_by(f64::total_cmp);
    let pitch = *sorted.get(sorted.len() / 2)?;
    pitches.iter().position(|&p| p < 0.75 * pitch)
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    fragments: &'a [SourceFragment],
    /// Header fragment boxes, left to right.
    columns: &'a [Rect],
    page: u32,
    /// The table's region for turned lines: the columns' left to right, the
    /// header's top to the page bottom (rows open) or to the last row's
    /// bottom (rows stopped).
    region: Rect,
}

impl Reader<'_> {
    /// The reader below rows that stopped after `last` (the last row, or the
    /// header without rows).
    fn stopped(self, last: Rect) -> Self {
        Reader {
            region: Rect {
                bottom: last.bottom,
                ..self.region
            },
            ..self
        }
    }

    /// Whether every fragment of `line` is turned and its box misses the
    /// table's region (touching edges meet it): such a line is skipped.
    fn outside(&self, line: &Line, budget: &mut Budget) -> Result<bool, BudgetError> {
        budget.spend(line.fragments.len() as u64)?;
        let turned = line
            .fragments
            .iter()
            .all(|&f| self.fragments[f].orientation != Orientation::Deg0);
        let r = self.region;
        let meets = line.bbox.left <= r.right
            && r.left <= line.bbox.right
            && line.bbox.top <= r.bottom
            && r.top <= line.bbox.bottom;
        Ok(turned && !meets)
    }

    fn read(
        &self,
        line: &Line,
        budget: &mut Budget,
        totals: &mut impl FnMut(&SourceFragment) -> Option<Totals>,
    ) -> Result<Read, BudgetError> {
        budget.spend(line.fragments.len() as u64)?;
        // A turned fragment among the rows cannot be placed in a column.
        if line.fragments.len() > MAX_TABLE_COLUMNS
            || line
                .fragments
                .iter()
                .any(|&f| self.fragments[f].orientation != Orientation::Deg0)
        {
            return Ok(Read::Irregular { row_like: true });
        }
        // The strongest totals label, and the position of the first one.
        let mut kind = None;
        let mut first = None;
        for (n, &f) in line.fragments.iter().enumerate() {
            let fragment = &self.fragments[f];
            budget.spend_bytes(&fragment.raw_text)?;
            if let Some(found) = totals(fragment) {
                kind = kind.max(Some(found));
                first.get_or_insert(n);
            }
        }
        let (Some(kind), Some(first)) = (kind, first) else {
            return self.cells(&line.fragments, budget);
        };
        // A totals label precedes its amounts: the fragments left of the
        // first label are not totals. When they may be an item row (a
        // totals block beside the last row), the line is irregular.
        Ok(match self.cells(&line.fragments[..first], budget)? {
            Read::Row(_) | Read::Irregular { row_like: true } => Read::Irregular { row_like: true },
            _ => Read::Totals(kind),
        })
    }

    /// How fragments `line` (indexes, left to right) read as cells, with no
    /// totals label among them.
    fn cells(&self, line: &[usize], budget: &mut Budget) -> Result<Read, BudgetError> {
        let mut cells = vec![None; self.columns.len()];
        let mut aligned = 0usize;
        // Plain fragments in one or more columns.
        let mut in_columns = 0usize;
        // A cell spanning columns or two fragments in one column.
        let mut irregular = false;
        // A fragment outside every column, or one with a delimiter.
        let mut stray = false;
        for &f in line {
            let fragment = &self.fragments[f];
            budget.spend_bytes(&fragment.raw_text)?;
            if classify(fragment) != Kind::Plain {
                stray = true;
                continue;
            }
            budget.spend(self.columns.len() as u64)?;
            let mut hits = self
                .columns
                .iter()
                .enumerate()
                .filter(|(_, column)| in_column(**column, fragment.bbox))
                .map(|(i, _)| i);
            match (hits.next(), hits.next()) {
                (None, _) => stray = true,
                (Some(column), None) => {
                    in_columns += 1;
                    if cells[column].replace(f).is_some() {
                        irregular = true;
                    } else {
                        aligned += 1;
                    }
                }
                (Some(_), Some(_)) => {
                    in_columns += 1;
                    irregular = true;
                }
            }
        }
        Ok(if aligned == 0 && !irregular {
            Read::Unaligned
        } else if irregular || stray || aligned < 2 {
            Read::Irregular {
                row_like: in_columns >= 2,
            }
        } else {
            Read::Row(cells)
        })
    }

    /// The end of rows that stopped at `lines[0]`: a totals line of `kind`,
    /// or a line ending them as `end` (unaligned or after a gap). Reads the
    /// lines below it (see the module documentation).
    fn below(
        &self,
        lines: &[Line],
        reach: f64,
        kind: Option<Totals>,
        end: TableEnd,
        budget: &mut Budget,
        totals: &mut impl FnMut(&SourceFragment) -> Option<Totals>,
    ) -> Result<TableEnd, BudgetError> {
        let mut closing = kind;
        // Whether a wider gap to a line other than a totals line was crossed.
        let mut crossed = false;
        let mut previous = lines[0].bbox;
        for line in &lines[1..] {
            budget.spend(1)?;
            if line.page != self.page {
                break;
            }
            if self.outside(line, budget)? {
                continue;
            }
            let gap = line.bbox.top - previous.bottom;
            let read = self.read(line, budget, totals)?;
            if gap > reach && !matches!(read, Read::Totals(_)) {
                // Items may resume after one gap, unless a closing total
                // ended them (another table may follow it).
                if crossed || closing == Some(Totals::Total) {
                    break;
                }
                crossed = true;
            }
            match read {
                Read::Totals(kind) => closing = closing.max(Some(kind)),
                Read::Row(_) | Read::Irregular { row_like: true } => {
                    return Ok(TableEnd::Irregular);
                }
                Read::Unaligned | Read::Irregular { row_like: false } => {}
            }
            previous = line.bbox;
        }
        Ok(match closing {
            Some(Totals::Total) => TableEnd::Total,
            Some(Totals::Subtotal) => TableEnd::Subtotal,
            None => end,
        })
    }
}

/// A fragment in a header cell's column: left edges, right edges or centres
/// within one line height (the taller of the two), or overlapping
/// horizontally.
fn in_column(column: Rect, cell: Rect) -> bool {
    let tolerance = column.height().max(cell.height());
    (column.left - cell.left).abs() <= tolerance
        || (column.right - cell.right).abs() <= tolerance
        || ((column.left + column.right) - (cell.left + cell.right)).abs() <= 2.0 * tolerance
        || column.left.max(cell.left) < column.right.min(cell.right)
}

#[cfg(test)]
mod tests {
    use super::super::{LayoutLimits, analyze};
    use super::*;
    use crate::source::Quality;
    use std::time::Instant;

    fn frag(id: &str, page: u32, text: &str, [l, t, r, b]: [f64; 4]) -> SourceFragment {
        SourceFragment {
            id: id.into(),
            page,
            item_index: 0,
            raw_text: text.into(),
            bbox: Rect {
                left: l,
                top: t,
                right: r,
                bottom: b,
            },
            quality: Quality::Usable,
            orientation: Orientation::Deg0,
        }
    }

    /// A one-line fragment on page 1, 10 pt high, with its top at `top`.
    fn at(id: &str, text: &str, left: f64, top: f64) -> SourceFragment {
        let width = 10.0 * text.chars().count() as f64;
        frag(id, 1, text, [left, top, left + width, top + 10.0])
    }

    /// Header `品名 / 数量 / 金額` at top 100, then `rows` 18 pt apart.
    fn table(rows: &[[&str; 3]]) -> Vec<SourceFragment> {
        let mut fragments = vec![
            at("h1", "品名", 60.0, 100.0),
            at("h2", "数量", 300.0, 100.0),
            at("h3", "金額", 440.0, 100.0),
        ];
        for (row, cells) in rows.iter().enumerate() {
            let top = 118.0 + 18.0 * row as f64;
            for (column, (text, left)) in cells.iter().zip([60.0, 300.0, 440.0]).enumerate() {
                if !text.is_empty() {
                    fragments.push(at(&format!("c{row}{column}"), text, left, top));
                }
            }
        }
        fragments
    }

    fn totals(f: &SourceFragment) -> Option<Totals> {
        match f.raw_text.as_str() {
            "小計" => Some(Totals::Subtotal),
            "合計" => Some(Totals::Total),
            _ => None,
        }
    }

    /// Read the table under the first line; rows as text (`""` for blank).
    fn read(fragments: &[SourceFragment]) -> (Vec<Vec<String>>, TableEnd) {
        let lines = analyze(fragments, &LayoutLimits::default()).unwrap().lines;
        let mut budget = Budget::new(u64::MAX, None);
        let table = read_table(fragments, &lines, 0, &mut budget, totals).unwrap();
        let rows = table
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| {
                        cell.as_ref().map_or(String::new(), |span| {
                            let f = fragments.iter().find(|f| f.id == span.fragment_id).unwrap();
                            f.raw_text[span.start..span.end].to_owned()
                        })
                    })
                    .collect()
            })
            .collect();
        (rows, table.end)
    }

    fn texts(rows: &[[&str; 3]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    #[test]
    fn rows_under_a_header_with_blank_cells() {
        let rows = [
            ["りんご", "3", "¥300"],
            ["みかん", "", "¥500"],
            ["ぶどう", "2", "¥800"],
        ];
        let (read_rows, end) = read(&table(&rows));
        assert_eq!(read_rows, texts(&rows));
        assert_eq!(end, TableEnd::PageEnd);
        // Cells are whole fragments with the fragment's own box.
        let fragments = table(&rows);
        let lines = analyze(&fragments, &LayoutLimits::default()).unwrap().lines;
        let mut budget = Budget::new(u64::MAX, None);
        let read = read_table(&fragments, &lines, 0, &mut budget, totals).unwrap();
        let cell = read.rows[0][0].as_ref().unwrap();
        assert_eq!(cell.precision, super::super::Precision::Run);
        assert_eq!(cell.bbox, fragments[3].bbox);
    }

    #[test]
    fn totals_unaligned_lines_and_gaps_end_the_rows() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Subtotal));
        // A subtotal, then a closing total: the block holds a total.
        fragments.extend([
            at("g", "合計", 300.0, 172.0),
            at("gv", "¥800", 440.0, 172.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // Any fragment of a line may be the totals label: a note in the
        // first column beside 小計 does not make the line a row.
        let mut fragments = table(&rows);
        fragments.extend([
            at("n", "※は軽減税率", 60.0, 154.0),
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Subtotal));
        // `label：value` within the row pitch is not a row.
        let mut fragments = table(&rows);
        fragments.push(at("n", "備考：午前着", 60.0, 154.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Unaligned));
        // A gap, then text in no column.
        let mut fragments = table(&rows);
        fragments.push(at("n", "以上", 180.0, 200.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Gap));
        // A gap, then one cell in a column: irregular (a heading before more
        // rows cannot be told from text after the table).
        let mut fragments = table(&rows);
        fragments.push(at("n", "以上", 60.0, 200.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // A gap, then totals.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "合計", 300.0, 200.0),
            at("tv", "¥800", 440.0, 200.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // A gap, then another row: the table resumes after a blank row.
        let mut fragments = table(&rows);
        fragments.extend([at("r1", "もも", 60.0, 172.0), at("r2", "1", 300.0, 172.0)]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // A header followed by a gap has no rows.
        let header = &table(&rows)[..3];
        let mut fragments = header.to_vec();
        fragments.push(at("n", "以上", 180.0, 200.0));
        assert_eq!(read(&fragments), (Vec::new(), TableEnd::Gap));
    }

    #[test]
    fn item_rows_below_a_stop_make_the_table_irregular() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        let more = |top: f64| {
            [
                at("m1", "ぶどう", 60.0, top),
                at("m2", "1", 300.0, top),
                at("m3", "¥900", 440.0, top),
            ]
        };
        // A group subtotal, then more items.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
        ]);
        fragments.extend(more(172.0));
        fragments.extend([
            at("g", "合計", 300.0, 190.0),
            at("gv", "¥1,700", 440.0, 190.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // `納品日：…` or text outside the columns between items.
        for stray in [
            at("n", "納品日：11月1日", 60.0, 154.0),
            at("n", "条件", 180.0, 154.0),
        ] {
            let mut fragments = table(&rows);
            fragments.push(stray);
            fragments.extend(more(172.0));
            assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        }
        // A gap, a one-cell heading, then items.
        let mut fragments = table(&rows);
        fragments.push(at("n", "追加注文", 180.0, 190.0));
        fragments.extend(more(208.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // Below a subtotal, a line with cells in two columns, one of them
        // spanning, may be an item row.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
            frag(
                "s1",
                1,
                "ぶどう（特選品の大箱）",
                [60.0, 172.0, 310.0, 182.0],
            ),
            at("s2", "¥900", 440.0, 172.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // A long note spanning columns alone is not an item row, and items
        // beyond a wider gap below the totals are another block.
        let mut fragments = table(&rows);
        fragments.extend([
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
            frag(
                "n",
                1,
                "お支払いは月末までにお願いします",
                [60.0, 172.0, 400.0, 182.0],
            ),
        ]);
        fragments.extend(more(260.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // Group subtotals separated by a blank line: items after the gap.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
        ]);
        fragments.extend(more(200.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // A subtotal, a gap, a heading in no column, then items.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
            at("n", "追加注文", 180.0, 200.0),
        ]);
        fragments.extend(more(218.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // Two gaps below a subtotal: the second block is not read.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
            at("n", "以下の通り", 180.0, 200.0),
        ]);
        fragments.extend(more(260.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Subtotal));
        // A gap, a heading in no column, another gap, then items.
        let mut fragments = table(&rows);
        fragments.push(at("n", "追加注文", 180.0, 190.0));
        fragments.extend(more(240.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // After a closing total, a gap, then another table's header.
        let mut fragments = table(&rows);
        fragments.extend([
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
            at("h1", "品名", 60.0, 300.0),
            at("h2", "数量", 300.0, 300.0),
            at("h3", "金額", 440.0, 300.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // A subtotal, a gap, then a closing total: the total block counts.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
            at("g", "合計", 300.0, 200.0),
            at("gv", "¥800", 440.0, 200.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // Rows on the next page are not read below a stop either.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
            frag("p1", 2, "ぶどう", [60.0, 100.0, 90.0, 110.0]),
            frag("p2", 2, "1", [300.0, 100.0, 305.0, 110.0]),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Subtotal));
    }

    #[test]
    fn a_totals_block_beside_an_item_row_is_irregular() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        let beside =
            |label: &str, top: f64| [at("b1", label, 560.0, top), at("b2", "¥1,700", 640.0, top)];
        let item = |top: f64| {
            [
                at("m1", "ぶどう", 60.0, top),
                at("m2", "1", 300.0, top),
                at("m3", "¥900", 440.0, top),
            ]
        };
        // The last item row with `合計 ¥1,700` beside it, outside every column.
        let mut fragments = table(&rows);
        fragments.extend(item(154.0));
        fragments.extend(beside("合計", 154.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // The same with the label in an extra header column (`備考`).
        let mut fragments = table(&rows);
        fragments.push(at("h4", "備考", 560.0, 100.0));
        fragments.extend(item(154.0));
        fragments.extend(beside("合計", 154.0));
        let (read_rows, end) = read(&fragments);
        assert_eq!((read_rows.len(), end), (2, TableEnd::Irregular));
        // A totals block beside the second row keeps the first row only.
        let mut fragments = table(&rows[..1]);
        fragments.extend([
            at("r1", "みかん", 60.0, 136.0),
            at("r2", "2", 300.0, 136.0),
            at("r3", "¥500", 440.0, 136.0),
        ]);
        fragments.extend(beside("小計", 136.0));
        assert_eq!(read(&fragments), (texts(&rows[..1]), TableEnd::Irregular));
        // Below a subtotal, an item row with a closing total beside it.
        let mut fragments = table(&rows);
        fragments.extend([
            at("t", "小計", 300.0, 154.0),
            at("tv", "¥800", 440.0, 154.0),
        ]);
        fragments.extend(item(172.0));
        fragments.extend(beside("合計", 172.0));
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Irregular));
        // A totals label in the first column has nothing left of it.
        let mut fragments = table(&rows);
        fragments.extend([
            at("g", "合計", 60.0, 154.0),
            at("gq", "5", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // One fragment left of the label (a note) is not an item row.
        let mut fragments = table(&rows);
        fragments.extend([
            at("n", "※軽減税率", 60.0, 154.0),
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
        ]);
        assert_eq!(read(&fragments), (texts(&rows), TableEnd::Total));
        // Two fragments in columns left of the label may be an item row,
        // even when they are a count (`点数 5 合計 ¥800` is held).
        let mut fragments = vec![
            at("h1", "品名", 60.0, 100.0),
            at("h2", "数量", 300.0, 100.0),
            at("h3", "単価", 380.0, 100.0),
            at("h4", "金額", 440.0, 100.0),
            at("a1", "りんご", 60.0, 118.0),
            at("a2", "3", 300.0, 118.0),
            at("a4", "¥300", 440.0, 118.0),
            at("p1", "点数", 60.0, 136.0),
            at("p2", "3", 300.0, 136.0),
            at("p3", "合計", 380.0, 136.0),
            at("p4", "¥300", 440.0, 136.0),
        ];
        let (read_rows, end) = read(&fragments);
        assert_eq!((read_rows.len(), end), (1, TableEnd::Irregular));
        // Without the count the line is a closing total.
        fragments.remove(8);
        assert_eq!(read(&fragments).1, TableEnd::Total);
    }

    #[test]
    fn a_row_closer_than_the_row_pitch_continues_a_wrapped_row() {
        // `特選品 / のし付き` wraps two cells of the first row 12 pt below it;
        // rows are 18 pt apart.
        let mut fragments = vec![
            at("h1", "品名", 60.0, 100.0),
            at("h2", "摘要", 220.0, 100.0),
            at("h3", "金額", 440.0, 100.0),
            at("a1", "りんご", 60.0, 118.0),
            at("a2", "贈答用", 220.0, 118.0),
            at("a3", "¥300", 440.0, 118.0),
            at("w1", "特選品", 60.0, 130.0),
            at("w2", "のし付き", 220.0, 130.0),
            at("b1", "みかん", 60.0, 148.0),
            at("b2", "家庭用", 220.0, 148.0),
            at("b3", "¥500", 440.0, 148.0),
        ];
        let (rows, end) = read(&fragments);
        assert_eq!((rows.len(), end), (1, TableEnd::Irregular));
        // The same at the end of the table.
        fragments.truncate(8);
        let (rows, end) = read(&fragments);
        assert_eq!((rows.len(), end), (1, TableEnd::Irregular));
        // A header farther from the first row (20 pt) than three rows are
        // apart (14 pt): the pitch is the rows'.
        let mut fragments = vec![
            at("h1", "品名", 60.0, 100.0),
            at("h2", "数量", 300.0, 100.0),
            at("h3", "金額", 440.0, 100.0),
        ];
        for (n, top) in [120.0, 134.0, 148.0].into_iter().enumerate() {
            fragments.extend([
                at(&format!("r{n}a"), "りんご", 60.0, top),
                at(&format!("r{n}b"), "1", 300.0, top),
            ]);
        }
        assert_eq!(
            read(&fragments),
            (
                vec![vec!["りんご".to_owned(), "1".to_owned(), String::new()]; 3],
                TableEnd::PageEnd
            )
        );
        assert_eq!(wrapped_row(&[100.0, 118.0, 130.0, 148.0]), Some(1));
        assert_eq!(wrapped_row(&[100.0, 120.0, 134.0, 148.0]), None);
        // With two rows the header's distance counts as much as the rows'.
        assert_eq!(wrapped_row(&[100.0, 120.0, 134.0]), Some(1));
        assert_eq!(wrapped_row(&[100.0]), None);
    }

    #[test]
    fn wrapped_spanning_and_crowded_lines_are_irregular() {
        // A wrapped first-column cell: the extra line has one cell only.
        let mut fragments = table(&[["りんご（青森産", "3", "¥300"]]);
        fragments.extend([
            at("w", "特選）", 60.0, 136.0),
            at("r1", "みかん", 60.0, 154.0),
            at("r2", "2", 300.0, 154.0),
            at("r3", "¥500", 440.0, 154.0),
        ]);
        let (rows, end) = read(&fragments);
        assert_eq!((rows.len(), end), (1, TableEnd::Irregular));
        // A cell reaching under the next header spans two columns.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments.extend([
            frag(
                "s1",
                1,
                "みかん（愛媛県産・特選品の大箱）",
                [60.0, 136.0, 310.0, 146.0],
            ),
            at("s2", "¥500", 440.0, 136.0),
        ]);
        assert_eq!(read(&fragments).1, TableEnd::Irregular);
        // Two fragments in one column.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("d1", "みかん", 60.0, 136.0),
            frag("d2", 1, "箱", [72.0, 136.0, 82.0, 146.0]),
            at("d3", "¥500", 440.0, 136.0),
        ]);
        assert_eq!(read(&fragments).1, TableEnd::Irregular);
        // A fragment outside every column beside aligned cells.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments.extend([
            at("o1", "みかん", 60.0, 136.0),
            at("o2", "*", 200.0, 136.0),
            at("o3", "¥500", 440.0, 136.0),
        ]);
        assert_eq!(read(&fragments).1, TableEnd::Irregular);
        // More fragments than MAX_TABLE_COLUMNS on one line.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments.extend(
            (0..=MAX_TABLE_COLUMNS).map(|i| at(&format!("x{i}"), "x", 10.0 * i as f64, 136.0)),
        );
        assert_eq!(read(&fragments).1, TableEnd::Irregular);
        // A one-cell header line cannot tell wrapped cells from rows.
        let list = [
            at("h", "品名", 60.0, 100.0),
            at("a", "りんご", 60.0, 118.0),
            at("b", "みかん", 60.0, 136.0),
        ];
        assert_eq!(read(&list), (Vec::new(), TableEnd::Irregular));
        // A header cell with a delimiter is a label, not a header cell.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments[0].raw_text = "品名：".into();
        assert_eq!(read(&fragments), (Vec::new(), TableEnd::Irregular));
    }

    #[test]
    fn extra_columns_are_read_and_alignment_uses_edges_or_overlap() {
        // Right-aligned amounts under a right-aligned header share right edges.
        let fragments = [
            at("h1", "品名", 60.0, 100.0),
            at("h2", "単価", 300.0, 100.0),
            frag("h3", 1, "金額", [500.0, 100.0, 520.0, 110.0]),
            at("a1", "りんご", 60.0, 118.0),
            at("a2", "¥100", 300.0, 118.0),
            frag("a3", 1, "¥12,300", [485.0, 118.0, 520.0, 128.0]),
        ];
        let (rows, end) = read(&fragments);
        assert_eq!(rows, [["りんご", "¥100", "¥12,300"]]);
        assert_eq!(end, TableEnd::PageEnd);
        let rect = |left, right| Rect {
            left,
            top: 0.0,
            right,
            bottom: 10.0,
        };
        // Centred header over a right-aligned number: no shared edge, centre
        // or overlap, so the number is in no column.
        assert!(!in_column(rect(440.0, 460.0), rect(485.0, 520.0)));
        assert!(in_column(rect(440.0, 460.0), rect(435.0, 465.0)));
        assert!(in_column(rect(300.0, 320.0), rect(300.0, 305.0)));
    }

    #[test]
    fn rows_stop_at_the_page_and_the_row_limit() {
        // Rows on the next page are not read.
        let mut fragments = table(&[["りんご", "3", "¥300"]]);
        fragments.extend([
            frag("p1", 2, "みかん", [60.0, 118.0, 90.0, 128.0]),
            frag("p2", 2, "2", [300.0, 118.0, 305.0, 128.0]),
        ]);
        assert_eq!(read(&fragments).1, TableEnd::PageEnd);
        assert_eq!(read(&fragments).0.len(), 1);
        let rows: Vec<[&str; 3]> = (0..=MAX_TABLE_ROWS).map(|_| ["品", "1", "¥1"]).collect();
        let (read_rows, end) = read(&table(&rows));
        assert_eq!((read_rows.len(), end), (MAX_TABLE_ROWS, TableEnd::RowLimit));
        let (read_rows, end) = read(&table(&rows[..MAX_TABLE_ROWS]));
        assert_eq!((read_rows.len(), end), (MAX_TABLE_ROWS, TableEnd::PageEnd));
    }

    #[test]
    fn turned_headers_and_rows_are_never_read_as_a_table() {
        let rows = [["りんご", "1", "¥300"], ["みかん", "2", "¥500"]];
        let upright = table(&rows);
        let lines = analyze(&upright, &LayoutLimits::default()).unwrap().lines;
        let read_with = |fragments: &[SourceFragment]| {
            let mut budget = Budget::new(u64::MAX, None);
            read_table(fragments, &lines, 0, &mut budget, totals).unwrap()
        };
        assert_eq!(read_with(&upright).rows.len(), 2);
        // Lines are read along x and y: a turned header or a turned
        // fragment among the rows is never placed in a column.
        let mut turned = upright.clone();
        turned[0].orientation = Orientation::Deg90;
        let header = read_with(&turned);
        assert!(header.rows.is_empty());
        assert_eq!(header.end, TableEnd::Irregular);
        let mut turned = upright.clone();
        let last = turned.len() - 1;
        turned[last].orientation = Orientation::Deg270;
        assert_eq!(read_with(&turned).end, TableEnd::Irregular);
    }

    /// `text` turned a quarter turn on page 1, 10 pt wide, from `top` down.
    fn turned_at(id: &str, text: &str, left: f64, top: f64) -> SourceFragment {
        let height = 10.0 * text.chars().count() as f64;
        SourceFragment {
            orientation: Orientation::Deg90,
            ..frag(id, 1, text, [left, top, left + 10.0, top + height])
        }
    }

    /// `read`, after checking that `fragments` make `lines` lines.
    fn read_lines(fragments: &[SourceFragment], lines: usize) -> (Vec<Vec<String>>, TableEnd) {
        let layout = analyze(fragments, &LayoutLimits::default()).unwrap();
        assert_eq!(layout.lines.len(), lines, "{:?}", layout.lines);
        read(fragments)
    }

    #[test]
    fn turned_text_away_from_the_table_is_skipped() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        // A margin stamp right of the columns (which end at 460), beside the
        // header and the rows.
        let stamp = turned_at("s", "CONFIDENTIAL", 560.0, 100.0);
        let mut fragments = table(&rows);
        fragments.push(stamp.clone());
        assert_eq!(read_lines(&fragments, 4), (texts(&rows), TableEnd::PageEnd));
        // The same below a closing total.
        fragments.extend([
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
        ]);
        assert_eq!(read_lines(&fragments, 5), (texts(&rows), TableEnd::Total));
        // A stamp above the header in the columns' span.
        let mut fragments = table(&rows);
        fragments.push(turned_at("s", "DRAFT", 200.0, 20.0));
        assert_eq!(read_lines(&fragments, 4), (texts(&rows), TableEnd::PageEnd));
        // Below a stop, turned text under the last row (beside the totals)
        // is not in the rows.
        let mut fragments = table(&rows);
        fragments.extend([
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
            turned_at("s", "CONFIDENTIAL", 200.0, 150.0),
        ]);
        assert_eq!(read_lines(&fragments, 5), (texts(&rows), TableEnd::Total));
    }

    #[test]
    fn turned_text_in_the_table_region_holds_the_table() {
        let rows = [["りんご", "3", "¥300"], ["みかん", "2", "¥500"]];
        // Across the rows between two columns, or touching the header.
        for stamp in [
            turned_at("s", "CONFIDENTIAL", 200.0, 110.0),
            turned_at("s", "DRAFT", 200.0, 50.0),
        ] {
            let mut fragments = table(&rows);
            fragments.push(stamp);
            assert_eq!(read_lines(&fragments, 4).1, TableEnd::Irregular);
        }
        // While rows are read, the region reaches the page bottom.
        let mut fragments = table(&rows);
        fragments.push(turned_at("s", "CONFIDENTIAL", 200.0, 400.0));
        assert_eq!(
            read_lines(&fragments, 4),
            (texts(&rows), TableEnd::Irregular)
        );
        // Below a stop, turned text reaching into the rows.
        let mut fragments = table(&rows);
        fragments.extend([
            at("g", "合計", 300.0, 154.0),
            at("gv", "¥800", 440.0, 154.0),
            turned_at("s", "CONFIDENTIAL", 200.0, 130.0),
        ]);
        assert_eq!(
            read_lines(&fragments, 5),
            (texts(&rows), TableEnd::Irregular)
        );
    }

    #[test]
    fn reading_spends_the_budget_and_honours_the_deadline() {
        let fragments = table(&[["りんご", "3", "¥300"], ["みかん", "2", "¥500"]]);
        let lines = analyze(&fragments, &LayoutLimits::default()).unwrap().lines;
        let mut budget = Budget::new(u64::MAX, None);
        read_table(&fragments, &lines, 0, &mut budget, totals).unwrap();
        let used = u64::MAX - budget.remaining();
        assert!(used > 0);
        let mut short = Budget::new(used - 1, None);
        assert_eq!(
            read_table(&fragments, &lines, 0, &mut short, totals),
            Err(BudgetError::WorkExceeded)
        );
        let mut late = Budget::new(u64::MAX, Some(Instant::now()));
        assert_eq!(
            read_table(&fragments, &lines, 0, &mut late, totals),
            Err(BudgetError::DeadlineExceeded)
        );
    }
}
