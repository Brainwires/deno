// Copyright 2018-2026 the Deno authors. MIT license.

use std::fs;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_ast::MediaType;
use deno_ast::ModuleKind;
use deno_ast::ModuleSpecifier;
use deno_config::glob::FileCollector;
use deno_config::glob::FilePatterns;
use deno_config::glob::PathOrPattern;
use deno_config::glob::PathOrPatternSet;
use deno_core::anyhow::Context;
use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::sourcemap::SourceMap;
use deno_core::url::Url;
use deno_resolver::npm::DenoInNpmPackageChecker;
use node_resolver::InNpmPackageChecker;
use regex::Regex;
use reporter::CoverageReporter;
use text_lines::TextLines;
use text_size::TextSize;

use self::ignore_directives::CoverageComment;
use self::ignore_directives::has_file_ignore_directive;
use self::ignore_directives::lex_comments;
use self::ignore_directives::parse_next_ignore_directives;
use self::ignore_directives::parse_range_ignore_directives;
use crate::args::CliOptions;
use crate::args::CoverageThresholds;
use crate::args::FileFlags;
use crate::args::Flags;
use crate::cdp;
use crate::factory::CliFactory;
use crate::file_fetcher::TextDecodedFile;
use crate::module_loader::CliEmitter;
use crate::resolver::CliCjsTracker;
use crate::sys::CliSys;
use crate::tools::test::is_supported_test_path;
use crate::util::text_encoding::Utf16Map;
use crate::util::text_encoding::source_map_from_code;
use crate::util::text_encoding::str_without_source_map;

mod ignore_directives;
mod merge;
mod range_tree;
pub mod reporter;
mod util;
use merge::ProcessCoverage;

#[derive(Debug, Clone)]
struct BranchCoverageItem {
  line_index: usize,
  block_number: usize,
  /// The function the block is, to match it in a report of another emitted
  /// form of the module, where its number can differ.
  block_key: BlockKey,
  /// The line of the first code of the arm's range, if it has one (the
  /// implicit complement of a single range has none), to tell how often it
  /// ran in a report of another emitted form of the module that lacks it.
  content_line_index: Option<usize>,
  branch_number: usize,
  taken: Option<i64>,
  is_hit: bool,
}

/// A function of a module (a block of its branches), told apart in terms of
/// its original source, so that the reports of different emitted forms of the
/// module (see `merge_reports`) can be matched function by function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BlockKey {
  /// The module's top-level code.
  TopLevel,
  /// A function, by the original line it starts on (if mapped), its name and
  /// its rank among the functions of that line and name.
  Function {
    line_index: Option<usize>,
    name: String,
    ordinal: usize,
  },
}

#[derive(Debug, Clone)]
struct FunctionCoverageItem {
  name: String,
  line_index: usize,
  execution_count: i64,
}

#[derive(Debug, Clone)]
pub struct CoverageReport {
  url: ModuleSpecifier,
  named_functions: Vec<FunctionCoverageItem>,
  branches: Vec<BranchCoverageItem>,
  /// (line_index, number_of_hits)
  found_lines: Vec<(usize, i64)>,
  output: Option<PathBuf>,
}

struct GenerateCoverageReportOptions<'a> {
  script_module_specifier: Url,
  script_media_type: MediaType,
  script_coverage: &'a cdp::ScriptCoverage,
  script_original_source: String,
  script_runtime_source: String,
  maybe_source_map: &'a Option<Vec<u8>>,
  output: &'a Option<PathBuf>,
}

/// The byte offset of the end of the code on the line
/// `line_start_byte_offset..line_end_byte_offset`, with a trailing comment and
/// the whitespace before it removed. A comment does not execute, so it must not
/// decide whether a zero-count range reaches the line's end edge when line hit
/// counts are projected from V8's block ranges. Without this, the same statement
/// is credited to its un-taken branch (0 hits) when nothing follows it and to
/// the covered trailing comment (1 hit) when one does. A line with no trailing
/// comment keeps its raw end, so its attribution is unchanged.
fn line_content_end_byte_offset(
  source: &str,
  line_start_byte_offset: usize,
  line_end_byte_offset: usize,
  comments: &[CoverageComment],
) -> usize {
  let mut end = line_end_byte_offset;
  let mut stripped_comment = false;
  loop {
    let trimmed_end = line_start_byte_offset
      + source[line_start_byte_offset..end].trim_end().len();
    let trailing_comment = comments.iter().find(|comment| {
      comment.range.start >= line_start_byte_offset
        && comment.range.start < trimmed_end
        && comment.range.end >= trimmed_end
    });
    match trailing_comment {
      Some(comment) => {
        stripped_comment = true;
        end = comment.range.start;
      }
      None => {
        if stripped_comment {
          end = trimmed_end;
        }
        return end;
      }
    }
  }
}

/// The UTF-16 offset of `byte_offset` in the source `utf16_map` was built over.
///
/// V8 counts a position in a script the way JavaScript indexes a string, in
/// UTF-16 code units, and reports its coverage range offsets in those.
/// `TextLines` counts Unicode scalar values, one per Rust `char`, and the two
/// part company once the source holds a scalar value outside the Basic
/// Multilingual Plane. A line's edges are put in V8's unit through this before
/// a range is compared against them.
fn utf16_offset(utf16_map: &Utf16Map, byte_offset: usize) -> usize {
  utf16_map
    .utf8_to_utf16_offset(TextSize::from(byte_offset as u32))
    .map(|offset| u32::from(offset) as usize)
    .expect("a byte offset within the source has a UTF-16 offset")
}

/// The byte offset of `utf16_offset` in the source `utf16_map` was built over,
/// for reading the source a coverage range covers or locating the line it is
/// on. Reading a range's offset as anything but a UTF-16 offset lands this far
/// enough along the source to cross a line end, which files a branch under the
/// line below the one it is on.
fn byte_offset(utf16_map: &Utf16Map, utf16_offset: usize) -> usize {
  utf16_map
    .utf16_to_utf8_offset(TextSize::from(utf16_offset as u32))
    .map(|offset| u32::from(offset) as usize)
    .expect("a UTF-16 offset within the source has a byte offset")
}

fn generate_coverage_report(
  options: GenerateCoverageReportOptions,
) -> Result<CoverageReport, AnyError> {
  let original_comments =
    lex_comments(&options.script_original_source, options.script_media_type);
  let url = Url::parse(&options.script_coverage.url).unwrap();

  if has_file_ignore_directive(&original_comments) {
    return Ok(CoverageReport {
      url,
      named_functions: Vec::new(),
      branches: Vec::new(),
      found_lines: Vec::new(),
      output: options.output.clone(),
    });
  }

  let maybe_source_map = options
    .maybe_source_map
    .as_ref()
    .map(|source_map| SourceMap::from_slice(source_map).unwrap());
  let mut coverage_report = CoverageReport {
    url,
    named_functions: Vec::with_capacity(
      options
        .script_coverage
        .functions
        .iter()
        .filter(|f| !f.function_name.is_empty())
        .count(),
    ),
    branches: Vec::new(),
    found_lines: Vec::new(),
    output: options.output.clone(),
  };

  let original_text_lines = TextLines::new(&options.script_original_source);
  let coverage_ignore_next_directives = parse_next_ignore_directives(
    &original_comments.comments,
    &original_text_lines,
  );
  let coverage_ignore_range_directives = parse_range_ignore_directives(
    &options.script_module_specifier,
    &original_comments.comments,
    &original_text_lines,
  );

  let runtime_comments =
    lex_comments(&options.script_runtime_source, MediaType::JavaScript);
  let runtime_text_lines = TextLines::new(&options.script_runtime_source);
  let utf16_map = Utf16Map::new(&options.script_runtime_source);
  for function in &options.script_coverage.functions {
    if function.function_name.is_empty() {
      continue;
    }

    // Skip functions that the source map can't trace back to the original
    // source — these are injected by the transformer (e.g. SWC's decorator
    // runtime helpers) and shouldn't show up in user-visible coverage.
    let Some(line_index) = range_to_src_line_index(
      &function.ranges[0],
      &runtime_text_lines,
      &utf16_map,
      &maybe_source_map,
    ) else {
      continue;
    };

    if line_index > 0
      && coverage_ignore_next_directives.contains(&(line_index - 1_usize))
    {
      continue;
    }

    if coverage_ignore_range_directives.iter().any(|range| {
      range.start_line_index <= line_index
        && range.stop_line_index >= line_index
    }) {
      continue;
    }

    coverage_report.named_functions.push(FunctionCoverageItem {
      name: function.function_name.clone(),
      line_index,
      execution_count: function.ranges[0].count,
    });
  }

  let mut block_ordinals: std::collections::HashMap<
    (Option<usize>, &str),
    usize,
  > = std::collections::HashMap::new();
  for (block_number, function) in
    options.script_coverage.functions.iter().enumerate()
  {
    let block_hits = function.ranges[0].count;
    let block_key = if function.function_name.is_empty()
      && function.ranges[0].start_char_offset == 0
    {
      BlockKey::TopLevel
    } else {
      let line_index = range_to_src_line_index(
        &function.ranges[0],
        &runtime_text_lines,
        &utf16_map,
        &maybe_source_map,
      );
      let ordinal = block_ordinals
        .entry((line_index, function.function_name.as_str()))
        .or_insert(0);
      *ordinal += 1;
      BlockKey::Function {
        line_index,
        name: function.function_name.clone(),
        ordinal: *ordinal - 1,
      }
    };

    // For each sub-range, find the parent count: the count of the smallest
    // enclosing range. V8 ranges are nested (ranges[0] is the function body,
    // inner ranges are blocks/branches/loops). The parent count is needed to
    // correctly compute complement branches — e.g. for an if/else inside a
    // loop, the parent is the loop body, not the function.
    let mut parent_counts: Vec<i64> =
      Vec::with_capacity(function.ranges.len().saturating_sub(1));
    for range in &function.ranges[1..] {
      let mut parent_count = block_hits;
      let mut parent_size = usize::MAX;
      for candidate in &function.ranges {
        if std::ptr::eq(candidate, range) {
          continue;
        }
        if candidate.start_char_offset <= range.start_char_offset
          && candidate.end_char_offset >= range.end_char_offset
        {
          let size = candidate.end_char_offset - candidate.start_char_offset;
          if size < parent_size {
            parent_size = size;
            parent_count = candidate.count;
          }
        }
      }
      parent_counts.push(parent_count);
    }

    // Group sub-ranges by their source line to detect branch points.
    // When multiple ranges map to the same source line, they represent
    // different arms of the same branch (e.g. if/else).
    let mut branches_by_line: std::collections::BTreeMap<
      usize,
      Vec<(usize, &cdp::CoverageRange, usize)>,
    > = std::collections::BTreeMap::new();
    for (idx, range) in function.ranges[1..].iter().enumerate() {
      // Skip whitespace-only "gap" ranges V8 emits between blocks (e.g. the
      // sliver between a catch's `return` and `finally`). They aren't real
      // branch arms, so counting them yields phantom branches (#35765).
      let start_byte_offset = byte_offset(&utf16_map, range.start_char_offset);
      let end_byte_offset = byte_offset(&utf16_map, range.end_char_offset);
      if options.script_runtime_source[start_byte_offset..end_byte_offset]
        .trim()
        .is_empty()
      {
        continue;
      }

      // Same rationale as above: drop sub-ranges with no source mapping —
      // they belong to transformer-injected helper code, not the user's
      // source.
      let Some(line_index) = range_to_src_line_index(
        range,
        &runtime_text_lines,
        &utf16_map,
        &maybe_source_map,
      ) else {
        continue;
      };
      // The line of the first code the range covers (a range after a `return`
      // starts at the end of the line before it).
      let range_source =
        &options.script_runtime_source[start_byte_offset..end_byte_offset];
      let content_line_index = byte_offset_to_src_line_index(
        start_byte_offset + range_source.len()
          - range_source.trim_start().len(),
        &runtime_text_lines,
        &utf16_map,
        &maybe_source_map,
      )
      .unwrap_or(line_index);
      branches_by_line.entry(line_index).or_default().push((
        idx,
        range,
        content_line_index,
      ));
    }

    for (line_index, ranges) in &branches_by_line {
      if *line_index > 0
        && coverage_ignore_next_directives.contains(&(line_index - 1_usize))
      {
        continue;
      }

      if coverage_ignore_range_directives.iter().any(|range| {
        range.start_line_index <= *line_index
          && range.stop_line_index >= *line_index
      }) {
        continue;
      }

      if ranges.len() == 1 {
        let (idx, range, content_line_index) = &ranges[0];
        let enclosing_count = parent_counts[*idx];

        if range.count > enclosing_count {
          // Range executes more often than its enclosing scope — this is a
          // loop body, not a branch arm. Report it without a complement.
          coverage_report.branches.push(BranchCoverageItem {
            line_index: *line_index,
            block_number,
            block_key: block_key.clone(),
            content_line_index: Some(*content_line_index),
            branch_number: 0,
            taken: if enclosing_count > 0 {
              Some(range.count)
            } else {
              None
            },
            is_hit: range.count > 0,
          });
        } else {
          // Single range at this line: one arm of a branch. The complement
          // (e.g. the else for an if) is implicit with count equal to the
          // enclosing range's count minus this range's count. Using the
          // enclosing (parent) range rather than the function-level count
          // gives correct complements for branches inside loops.
          let taken = if enclosing_count > 0 {
            Some(range.count)
          } else {
            None
          };
          let complement_taken = if enclosing_count > 0 {
            Some(enclosing_count - range.count)
          } else {
            None
          };
          coverage_report.branches.push(BranchCoverageItem {
            line_index: *line_index,
            block_number,
            block_key: block_key.clone(),
            content_line_index: Some(*content_line_index),
            branch_number: 0,
            taken,
            is_hit: range.count > 0,
          });
          coverage_report.branches.push(BranchCoverageItem {
            line_index: *line_index,
            block_number,
            block_key: block_key.clone(),
            content_line_index: None,
            branch_number: 1,
            taken: complement_taken,
            is_hit: complement_taken.is_some_and(|c| c > 0),
          });
        }
      } else {
        // Multiple ranges at the same line: these are explicit branch arms
        // (e.g. both if and else blocks reported by V8).
        for (branch_number, (_, range, content_line_index)) in
          ranges.iter().enumerate()
        {
          let taken = if block_hits > 0 {
            Some(range.count)
          } else {
            None
          };
          coverage_report.branches.push(BranchCoverageItem {
            line_index: *line_index,
            block_number,
            block_key: block_key.clone(),
            content_line_index: Some(*content_line_index),
            branch_number,
            taken,
            is_hit: range.count > 0,
          });
        }
      }
    }
  }

  // TODO(caspervonb): collect uncovered ranges on the lines so that we can highlight specific
  // parts of a line in color (word diff style) instead of the entire line.
  let mut line_counts = Vec::with_capacity(runtime_text_lines.lines_count());
  for line_index in 0..runtime_text_lines.lines_count() {
    let (line_start_byte_offset, line_end_byte_offset) =
      runtime_text_lines.line_range(line_index);
    let line_start_utf16_offset =
      utf16_offset(&utf16_map, line_start_byte_offset);
    let line_end_utf16_offset = utf16_offset(&utf16_map, line_end_byte_offset);
    // The line's end edge for the reset pass below is the end of its code,
    // excluding a trailing comment so a comment cannot change what the line is
    // credited with.
    let content_end_byte_offset = line_content_end_byte_offset(
      &options.script_runtime_source,
      line_start_byte_offset,
      line_end_byte_offset,
      &runtime_comments.comments,
    );
    let line_content_end_utf16_offset =
      utf16_offset(&utf16_map, content_end_byte_offset);
    let ignore = runtime_comments.comments.iter().any(|comment| {
      comment.range.start <= line_start_byte_offset
        && comment.range.end >= line_end_byte_offset
    }) || options.script_runtime_source
      [line_start_byte_offset..line_end_byte_offset]
      .trim()
      .is_empty();
    let mut count = 0;

    if ignore {
      count = 1;
    } else {
      // Find the count from the most specific (smallest/innermost) range that
      // fully covers this line. V8 coverage ranges are nested: ranges[0] is the
      // function body and ranges[1..] are inner blocks/branches. The innermost
      // range that covers a line gives the correct execution count for that line.
      let mut best_range_size = usize::MAX;
      for function in &options.script_coverage.functions {
        for range in &function.ranges {
          if range.start_char_offset <= line_start_utf16_offset
            && range.end_char_offset >= line_end_utf16_offset
          {
            let range_size = range.end_char_offset - range.start_char_offset;
            if range_size < best_range_size {
              best_range_size = range_size;
              count = range.count;
            }
          }
        }
      }

      // Reset the count when a zero-count range genuinely covers this line's
      // code, judged three ways below. Merely reaching one edge is not enough:
      // where a covered arm and a zero-count sibling arm meet on one line, the
      // covered side is real code that ran and must not be hidden. That is the
      // case at a branch junction such as `} else {`, `} catch {`, or
      // `} finally {`: when only one arm ran, the other arm's zero-count range
      // clips the opposite edge, and zeroing the whole line on that single edge
      // is what otherwise makes the junction count as covered only when both
      // arms run in the same coverage process. Splitting the two arms across
      // separate test files then drops the junction from the merged line
      // coverage. A zero-count range floating in the middle of a line (matching
      // none of the tests) is a V8 block-boundary gap, e.g. the unreachable path
      // between a catch's return and finally, and is ignored.
      for (range_index, range) in options
        .script_coverage
        .functions
        .iter()
        .flat_map(|function| function.ranges.iter().enumerate())
      {
        if range.count > 0 {
          continue;
        }

        let overlaps = range.start_char_offset < line_end_utf16_offset
          && range.end_char_offset > line_start_utf16_offset;
        if !overlaps {
          continue;
        }

        // The line sits entirely inside the uncovered range: a fully uncovered
        // line, or one in the middle of a multi-line uncovered block.
        let spans_content = range.start_char_offset <= line_start_utf16_offset
          && range.end_char_offset >= line_content_end_utf16_offset;
        // An uncovered statement confined to this line that runs to the end of
        // its code, e.g. the never-taken `throw` in `if (!x) throw …`. A
        // multi-line block whose brace only clips a junction extends past the
        // line end and is excluded here, so the covered half survives.
        let inline_uncovered = range.start_char_offset
          >= line_start_utf16_offset
          && range.end_char_offset <= line_end_utf16_offset
          && range.end_char_offset >= line_content_end_utf16_offset;
        // A never-called function is a single count-0 range (ranges[0]) covering
        // the whole function. Its signature line stays uncovered like its body,
        // even though the range starts a few characters into that line, after an
        // `export`/`async` keyword, which the span test above would miss.
        let whole_function = range_index == 0
          && (range.start_char_offset <= line_start_utf16_offset
            || range.end_char_offset >= line_content_end_utf16_offset);
        if spans_content || inline_uncovered || whole_function {
          count = 0;
        }
      }
    }

    line_counts.push(count);
  }

  let found_lines_coverage_filter = |(line, _): &(usize, i64)| -> bool {
    if coverage_ignore_range_directives.iter().any(|range| {
      range.start_line_index <= *line && range.stop_line_index >= *line
    }) {
      return false;
    }

    if coverage_ignore_next_directives.contains(line) {
      return false;
    }

    if *line == 0_usize {
      return true;
    }

    if coverage_ignore_next_directives.contains(&(line - 1_usize)) {
      return false;
    }

    true
  };

  coverage_report.found_lines =
    if let Some(source_map) = maybe_source_map.as_ref() {
      let script_runtime_source_lines =
        options.script_runtime_source.lines().collect::<Vec<_>>();
      let mut found_lines = line_counts
        .iter()
        .enumerate()
        .flat_map(|(index, count)| {
          // get all the mappings from this destination line to a different src line
          let mut results = source_map
            .tokens()
            .filter(|token| {
              let dst_line = token.get_dst_line() as usize;
              dst_line == index && {
                let dst_col = token.get_dst_col() as usize;
                let content = script_runtime_source_lines
                  .get(dst_line)
                  .and_then(|line| {
                    line.get(dst_col..std::cmp::min(dst_col + 2, line.len()))
                  })
                  .unwrap_or("");

                !content.is_empty()
                  && content != "/*"
                  && content != "*/"
                  && content != "//"
              }
            })
            .map(move |token| (token.get_src_line() as usize, *count))
            .collect::<Vec<_>>();
          // only keep the results that point at different src lines
          results.sort_unstable_by_key(|(index, _)| *index);
          results.dedup_by_key(|(index, _)| *index);
          results.into_iter()
        })
        .filter(found_lines_coverage_filter)
        .collect::<Vec<(usize, i64)>>();

      found_lines.sort_unstable_by_key(|(index, _)| *index);
      // combine duplicated lines - when multiple compiled JS lines map to the
      // same source line, use the maximum count rather than summing, since
      // hitting the same source line from different compiled lines doesn't mean
      // it was executed more times.
      for i in (1..found_lines.len()).rev() {
        if found_lines[i].0 == found_lines[i - 1].0 {
          found_lines[i - 1].1 =
            std::cmp::max(found_lines[i - 1].1, found_lines[i].1);
          found_lines.remove(i);
        }
      }
      found_lines
    } else {
      line_counts
        .into_iter()
        .enumerate()
        .filter(found_lines_coverage_filter)
        .collect::<Vec<(usize, i64)>>()
    };

  Ok(coverage_report)
}

/// Maps a runtime coverage range back to a line in the original source.
///
/// Returns `None` when the source map exists but has no mapping for this
/// position. That happens for code injected by the transformer (e.g. SWC's
/// decorator runtime helpers like `applyDecs2203R`) — the bytes exist in
/// the runtime JS but didn't come from the user's source, so callers
/// should drop these coverage entries rather than reporting them as if
/// they were the user's own code. So does code injected after all of the
/// user's code (e.g. the export setters of `deno test`'s Vitest
/// compatibility): the closest mapping before it is the user's last one.
fn range_to_src_line_index(
  range: &cdp::CoverageRange,
  text_lines: &TextLines,
  utf16_map: &Utf16Map,
  maybe_source_map: &Option<SourceMap>,
) -> Option<usize> {
  byte_offset_to_src_line_index(
    byte_offset(utf16_map, range.start_char_offset),
    text_lines,
    utf16_map,
    maybe_source_map,
  )
}

/// [`range_to_src_line_index`] for a byte offset in the runtime source.
fn byte_offset_to_src_line_index(
  runtime_byte_offset: usize,
  text_lines: &TextLines,
  utf16_map: &Utf16Map,
  maybe_source_map: &Option<SourceMap>,
) -> Option<usize> {
  let line_index = text_lines.line_index(runtime_byte_offset);
  if let Some(source_map) = maybe_source_map.as_ref() {
    // A source map's columns are UTF-16 code units as well, so the column is
    // the distance from the line's start in that unit.
    let column = utf16_offset(utf16_map, runtime_byte_offset)
      - utf16_offset(utf16_map, text_lines.line_start(line_index));
    let last_mapped_line = source_map
      .get_token_count()
      .checked_sub(1)
      .and_then(|index| source_map.get_token(index as usize))
      .map(|token| token.get_dst_line() as usize);
    if last_mapped_line.is_none_or(|last_line| line_index > last_line) {
      return None;
    }
    source_map
      .lookup_token(line_index as u32, column as u32)
      .map(|token| token.get_src_line() as usize)
  } else {
    Some(line_index)
  }
}

fn collect_coverages(
  cli_options: &CliOptions,
  files: FileFlags,
  initial_cwd: &Path,
) -> Result<Vec<cdp::ScriptCoverage>, AnyError> {
  let mut coverages: Vec<cdp::ScriptCoverage> = Vec::new();
  let file_patterns = FilePatterns {
    base: initial_cwd.to_path_buf(),
    include: Some({
      if files.include.is_empty() {
        PathOrPatternSet::new(vec![PathOrPattern::Path(
          initial_cwd.to_path_buf(),
        )])
      } else {
        PathOrPatternSet::from_include_relative_path_or_patterns(
          initial_cwd,
          &files.include,
        )?
      }
    }),
    exclude: PathOrPatternSet::new(vec![]),
  };
  let file_paths = FileCollector::new(|e| {
    e.path.extension().map(|ext| ext == "json").unwrap_or(false)
  })
  .ignore_git_folder()
  .ignore_node_modules()
  .set_vendor_folder(cli_options.vendor_dir_path().map(ToOwned::to_owned))
  .collect_file_patterns(&CliSys::default(), &file_patterns);

  let coverage_patterns = FilePatterns {
    base: initial_cwd.to_path_buf(),
    include: None,
    exclude: PathOrPatternSet::from_exclude_relative_path_or_patterns(
      initial_cwd,
      &files.ignore,
    )
    .context("Invalid ignore pattern.")?,
  };

  for file_path in file_paths {
    let new_coverage = fs::read_to_string(file_path.as_path())
      .map_err(AnyError::from)
      .and_then(|json| {
        serde_json::from_str::<cdp::ScriptCoverage>(&json)
          .map_err(AnyError::from)
      })
      .with_context(|| format!("Failed reading '{}'", file_path.display()))?;
    let url = Url::parse(&new_coverage.url)?;
    if coverage_patterns.matches_specifier(&url) {
      coverages.push(new_coverage);
    }
  }

  coverages.sort_by_key(|k| k.url.clone());

  Ok(coverages)
}

fn filter_coverages(
  coverages: Vec<cdp::ScriptCoverage>,
  include: Vec<String>,
  exclude: Vec<String>,
  in_npm_pkg_checker: &DenoInNpmPackageChecker,
  link_dir_urls: &[String],
) -> Vec<cdp::ScriptCoverage> {
  let include: Vec<Regex> =
    include.iter().map(|e| Regex::new(e).unwrap()).collect();

  let exclude: Vec<Regex> =
    exclude.iter().map(|e| Regex::new(e).unwrap()).collect();

  // Matches virtual file paths for doc testing
  // e.g. file:///path/to/mod.ts#23-29.ts
  let doc_test_re =
    Regex::new(r"#\d+-\d+\.(js|mjs|cjs|jsx|ts|mts|cts|tsx)$").unwrap();

  coverages
    .into_iter()
    .filter(|e| {
      let is_internal = e.url.starts_with("ext:")
        || e.url.starts_with("data:")
        || e.url.starts_with("blob:")
        // the Vitest compatibility of `deno test`: Vitest's own (vendored)
        // runtime and the generated mock modules
        || e.url.starts_with("deno-vitest:")
        || e.url.ends_with("__anonymous__")
        || e.url.ends_with("$deno$test.mjs")
        || e.url.contains("/$deno$stdin.")
        || e.url.ends_with(".snap")
        || is_supported_test_path(Path::new(e.url.as_str()))
        || doc_test_re.is_match(e.url.as_str())
        // Exclude packages brought in via the "links" (formerly "patch")
        // feature. These are third-party dependencies replacing a registry
        // version, so they shouldn't show up in the user's coverage report,
        // matching how npm packages are excluded above.
        || link_dir_urls.iter().any(|dir| e.url.starts_with(dir))
        || Url::parse(&e.url)
          .ok()
          .map(|url| in_npm_pkg_checker.in_npm_package(&url))
          .unwrap_or(false);

      let is_included = include.iter().any(|p| p.is_match(&e.url));
      let is_excluded = exclude.iter().any(|p| p.is_match(&e.url));

      (include.is_empty() || is_included) && !is_excluded && !is_internal
    })
    .collect::<Vec<cdp::ScriptCoverage>>()
}

#[allow(clippy::too_many_arguments, reason = "coverage entry point")]
pub fn cover_files(
  flags: Arc<Flags>,
  files_include: Vec<String>,
  files_ignore: Vec<String>,
  include: Vec<String>,
  exclude: Vec<String>,
  output: Option<String>,
  cli_threshold: Option<f64>,
  reporters: &[&dyn CoverageReporter],
) -> Result<(), AnyError> {
  if files_include.is_empty() {
    return Err(anyhow!("No matching coverage profiles found"));
  }

  let factory = CliFactory::from_flags(flags);
  let cli_options = factory.cli_options()?;
  let in_npm_pkg_checker = factory.in_npm_pkg_checker()?;
  let file_fetcher = factory.file_fetcher()?;
  let emitter = factory.emitter()?;
  let cjs_tracker = factory.cjs_tracker()?;

  // Use the first include path as the default output path.
  let coverage_root = cli_options.initial_cwd().join(&files_include[0]);
  let script_coverages = collect_coverages(
    cli_options,
    FileFlags {
      include: files_include,
      ignore: files_ignore,
    },
    cli_options.initial_cwd(),
  )?;
  if script_coverages.is_empty() {
    return Err(anyhow!("No coverage files found"));
  }
  let link_dir_urls = cli_options
    .workspace()
    .link_folders()
    .keys()
    .map(|url| url.as_str().to_string())
    .collect::<Vec<_>>();
  let script_coverages = filter_coverages(
    script_coverages,
    include,
    exclude,
    in_npm_pkg_checker,
    &link_dir_urls,
  );
  if script_coverages.is_empty() {
    return Err(anyhow!("No covered files included in the report"));
  }

  // The profiles of each module, the same module loaded under different
  // specifiers being one.
  let mut url_to_coverages: std::collections::BTreeMap<
    String,
    Vec<cdp::ScriptCoverage>,
  > = std::collections::BTreeMap::new();
  for mut cov in script_coverages {
    // Merge the same module loaded under different specifiers. Fragments
    // never affect the source, so strip them for any file/http(s) URL.
    // Query strings are only stripped for `file:` (same file on disk);
    // http(s) servers can return different bytes per query and the cache
    // keys on the full URL, so keep it there.
    if let Ok(mut url) = Url::parse(&cov.url) {
      let strip_fragment = matches!(url.scheme(), "file" | "http" | "https")
        && url.fragment().is_some();
      let strip_query = url.scheme() == "file" && url.query().is_some();
      if strip_fragment {
        url.set_fragment(None);
      }
      if strip_query {
        url.set_query(None);
      }
      if strip_fragment || strip_query {
        cov.url = url.to_string();
      }
    }
    url_to_coverages
      .entry(cov.url.clone())
      .or_default()
      .push(cov);
  }

  let out_mode = match output {
    Some(ref path) => match File::create(path) {
      Ok(_) => Some(PathBuf::from(path)),
      Err(e) => {
        return Err(anyhow!("Failed to create output file: {}", e));
      }
    },
    None => None,
  };
  let get_message = |specifier: &ModuleSpecifier| -> String {
    format!(
      "Source not found for \"{}\" (was it deleted after coverage was collected?). Skipping.",
      specifier,
    )
  };

  let mut file_reports = Vec::with_capacity(url_to_coverages.len());

  for (url, coverages) in url_to_coverages {
    let module_specifier =
      deno_path_util::resolve_url_or_path(&url, cli_options.initial_cwd())?;

    let maybe_file_result =
      file_fetcher.get_cached_source_or_local(&module_specifier);
    let file = match maybe_file_result {
      Ok(Some(file)) => TextDecodedFile::decode(file)?,
      Ok(None) => {
        log::warn!("{}", get_message(&module_specifier),);
        continue;
      }
      Err(err) => {
        log::warn!("{}: {}", get_message(&module_specifier), err,);
        continue;
      }
    };

    let original_source = file.source.clone();
    let candidates =
      runtime_code_candidates(&file, emitter.as_ref(), cjs_tracker.as_ref())?;

    // The profiles of one emitted form of the module merge range by range.
    // A module can have run in more than one form (with and without `deno
    // test`'s Vitest transform, which changes the emitted code), whose ranges
    // are offsets into different code: each form gets its own report, through
    // its own source map, and the reports merge line by line below.
    let mut forms: Vec<(usize, Vec<ProcessCoverage>)> = Vec::new();
    let mut missing = false;
    for coverage in coverages {
      let Some(index) =
        runtime_code_index(&candidates, script_runtime_len(&coverage))
      else {
        missing = true;
        continue;
      };
      let process = ProcessCoverage {
        result: vec![coverage],
      };
      match forms.iter_mut().find(|(form, _)| *form == index) {
        Some((_, processes)) => processes.push(process),
        None => forms.push((index, vec![process])),
      }
    }
    if missing {
      log::warn!(
        "Missing transpiled source code for: \"{}\" (was it deleted after coverage was collected?). Skipping.",
        file.specifier,
      );
    }

    let mut form_reports = Vec::with_capacity(forms.len());
    for (index, processes) in forms {
      let Some(script_coverage) = merge::merge_processes(processes)
        .and_then(|process| process.result.into_iter().next())
      else {
        continue;
      };
      let runtime_code = &candidates[index].code;
      let source_map = source_map_from_code(runtime_code.as_bytes());
      let coverage_report =
        generate_coverage_report(GenerateCoverageReportOptions {
          script_module_specifier: module_specifier.clone(),
          script_media_type: file.media_type,
          script_coverage: &script_coverage,
          script_original_source: original_source.to_string(),
          script_runtime_source: runtime_code.clone(),
          maybe_source_map: &source_map,
          output: &out_mode,
        })
        .with_context(|| {
          format!(
            "Failed to generate coverage report for file ({module_specifier})"
          )
        })?;
      form_reports.push(coverage_report);
    }
    let Some(coverage_report) = merge_reports(form_reports) else {
      continue;
    };

    if !coverage_report.found_lines.is_empty() {
      file_reports.push((coverage_report, original_source.to_string()));
    }
  }

  // All covered files, might have had ignore directive and we can end up
  // with no reports at this point.
  if file_reports.is_empty() {
    return Err(anyhow!("No covered files included in the report"));
  }

  for reporter in reporters {
    reporter.done(&coverage_root, &file_reports);
  }

  // Layer the `--threshold` CLI flag over per-metric thresholds from deno.json
  // (the flag wins) and fail the command if any configured threshold is unmet.
  let config_thresholds = cli_options.resolve_coverage_thresholds()?;
  let thresholds = CoverageThresholds {
    lines: cli_threshold.or(config_thresholds.lines),
    branches: cli_threshold.or(config_thresholds.branches),
    functions: cli_threshold.or(config_thresholds.functions),
  };
  check_coverage_thresholds(&file_reports, thresholds)?;

  Ok(())
}

/// The UTF-16 length of the code V8 ran for a script: the end of its
/// top-level function's range, if the coverage has it.
fn script_runtime_len(script_coverage: &cdp::ScriptCoverage) -> Option<usize> {
  script_coverage
    .functions
    .iter()
    .filter(|function| function.function_name.is_empty())
    .filter_map(|function| function.ranges.first())
    .filter(|range| range.start_char_offset == 0)
    .map(|range| range.end_char_offset)
    .max()
}

/// Whether `code` (with its inline source map, which the module loader
/// strips) can be the code V8 ran, which was `runtime_len` long, if known.
fn is_runtime_code(code: &str, runtime_len: Option<usize>) -> bool {
  runtime_len.is_none_or(|runtime_len| {
    str_without_source_map(code).encode_utf16().count() == runtime_len
  })
}

/// A form of a module's code that V8 may have run.
struct RuntimeCode {
  code: String,
  /// The module's usual form (not changed by a program transform).
  plain: bool,
}

/// The forms of `file`'s code a coverage profile of it can be of: its usual
/// one (the source itself, or its cached emit) and, when `deno test`'s Vitest
/// compatibility ran it, the emit its program transform changed. A plain run
/// and a Vitest run of a module each leave theirs in the emit cache.
fn runtime_code_candidates(
  file: &TextDecodedFile,
  emitter: &CliEmitter,
  cjs_tracker: &CliCjsTracker,
) -> Result<Vec<RuntimeCode>, AnyError> {
  let original_source = &file.source;
  let plain = |code: String| RuntimeCode { code, plain: true };
  let mut candidates = match file.media_type {
    MediaType::Unknown
    | MediaType::Css
    | MediaType::Html
    | MediaType::Markdown
    | MediaType::Sql
    | MediaType::Wasm
    | MediaType::Cjs
    | MediaType::Json
    | MediaType::Jsonc
    | MediaType::Json5 => return Ok(vec![plain(original_source.to_string())]),
    // Served as is, unless a program transform applied to it, which emits it.
    MediaType::JavaScript | MediaType::Mjs => {
      vec![plain(original_source.to_string())]
    }
    MediaType::Dts | MediaType::Dmts | MediaType::Dcts => {
      return Ok(vec![plain(String::new())]);
    }
    MediaType::TypeScript
    | MediaType::Jsx
    | MediaType::Mts
    | MediaType::Cts
    | MediaType::Tsx => {
      let module_kind = ModuleKind::from_is_cjs(
        cjs_tracker.is_maybe_cjs(&file.specifier, file.media_type)?,
      );
      emitter
        .maybe_cached_emit(&file.specifier, module_kind, original_source)?
        .map(plain)
        .into_iter()
        .collect()
    }
    MediaType::SourceMap => {
      unreachable!()
    }
  };
  if let Some(code) =
    transformed_emit(emitter, &file.specifier, original_source)
    && candidates.iter().all(|candidate| candidate.code != code)
  {
    candidates.push(RuntimeCode { code, plain: false });
  }
  Ok(candidates)
}

/// Which of `candidates` a profile whose code was `runtime_len` long is of:
/// the one of that length, else (a length that tells nothing, as for a
/// CommonJS module V8 ran wrapped) the module's usual form, if there is one.
fn runtime_code_index(
  candidates: &[RuntimeCode],
  runtime_len: Option<usize>,
) -> Option<usize> {
  candidates
    .iter()
    .position(|candidate| is_runtime_code(&candidate.code, runtime_len))
    .or_else(|| candidates.iter().position(|candidate| candidate.plain))
}

/// The cached emit of a module that a program transform changed (`deno
/// test`'s Vitest compatibility hoists `vi.mock()` calls and makes exports
/// mutable), which `deno coverage` cannot reproduce and so cannot look up by
/// its cache key: whatever the cache holds for the module's transformed
/// emit, when its source map says it was emitted from `original_source`.
fn transformed_emit(
  emitter: &CliEmitter,
  specifier: &ModuleSpecifier,
  original_source: &str,
) -> Option<String> {
  let code =
    emitter.maybe_cached_transformed_emit_any_source_hash(specifier)?;
  is_emit_of(&code, original_source).then_some(code)
}

/// Merges the reports of one module made from profiles of different emitted
/// forms of it (see `cover_files`), whose ranges cannot be merged as such, in
/// terms of its original source: a line's, function's and branch's counts
/// are the sums of theirs in each report, and a line, function or branch in
/// only one of them is kept. The blocks of the branches are matched by the
/// function they are in (see [`BlockKey`]), keeping the first report's block
/// numbers.
fn merge_reports(reports: Vec<CoverageReport>) -> Option<CoverageReport> {
  let mut reports = reports.into_iter();
  let mut merged = reports.next()?;
  let mut merged_any = false;
  for report in reports {
    merged_any = true;
    let merged_lines: std::collections::HashMap<usize, i64> =
      merged.found_lines.iter().copied().collect();
    let report_lines: std::collections::HashMap<usize, i64> =
      report.found_lines.iter().copied().collect();

    let mut lines: std::collections::BTreeMap<usize, i64> =
      merged.found_lines.drain(..).collect();
    for (line_index, count) in report.found_lines {
      let merged_count = lines.entry(line_index).or_insert(0);
      *merged_count = merged_count.saturating_add(count);
    }
    merged.found_lines = lines.into_iter().collect();

    for function in report.named_functions {
      match merged.named_functions.iter_mut().find(|merged_function| {
        merged_function.name == function.name
          && merged_function.line_index == function.line_index
      }) {
        Some(merged_function) => {
          merged_function.execution_count = merged_function
            .execution_count
            .saturating_add(function.execution_count);
        }
        None => merged.named_functions.push(function),
      }
    }

    // A branch one report has and the other lacks is one whose range V8 left
    // out of the other's profile, as it does a block that ran as often as the
    // code around it (see `elided_branch_taken`).
    for merged_branch in &mut merged.branches {
      if !report
        .branches
        .iter()
        .any(|branch| is_same_branch(branch, merged_branch))
      {
        add_branch_taken(
          merged_branch,
          elided_branch_taken(&report_lines, merged_branch),
        );
      }
    }
    let mut block_numbers: std::collections::HashMap<BlockKey, usize> = merged
      .branches
      .iter()
      .map(|branch| (branch.block_key.clone(), branch.block_number))
      .collect();
    let mut next_block_number = merged
      .branches
      .iter()
      .map(|branch| branch.block_number + 1)
      .max()
      .unwrap_or(0);
    for mut branch in report.branches {
      match merged
        .branches
        .iter_mut()
        .find(|merged_branch| is_same_branch(merged_branch, &branch))
      {
        Some(merged_branch) => add_branch_taken(merged_branch, branch.taken),
        None => {
          let taken = elided_branch_taken(&merged_lines, &branch);
          add_branch_taken(&mut branch, taken);
          branch.block_number = *block_numbers
            .entry(branch.block_key.clone())
            .or_insert_with(|| {
              next_block_number += 1;
              next_block_number - 1
            });
          merged.branches.push(branch);
        }
      }
    }
  }
  if merged_any {
    merged
      .named_functions
      .sort_by_key(|function| function.line_index);
    merged.branches.sort_by_key(|branch| {
      (branch.block_number, branch.line_index, branch.branch_number)
    });
  }
  Some(merged)
}

/// Whether two branches of reports of different emitted forms of a module
/// are the same arm of the same branch (see `merge_reports`).
fn is_same_branch(a: &BranchCoverageItem, b: &BranchCoverageItem) -> bool {
  a.block_key == b.block_key
    && a.line_index == b.line_index
    && a.branch_number == b.branch_number
}

/// How often the arm `branch` was taken in a report that lacks it, whose line
/// counts are `lines` (see `merge_reports`): V8 left out a range that ran as
/// often as the code around it, so an arm with a range ran as often as its
/// first line of code did there, and the implicit complement of such a range
/// never ran. Nothing is known for code that did not run there.
fn elided_branch_taken(
  lines: &std::collections::HashMap<usize, i64>,
  branch: &BranchCoverageItem,
) -> Option<i64> {
  match branch.content_line_index {
    Some(content_line_index) => lines
      .get(&content_line_index)
      .copied()
      .filter(|count| *count > 0),
    None => lines
      .get(&branch.line_index)
      .is_some_and(|count| *count > 0)
      .then_some(0),
  }
}

/// Adds `taken` to how often `branch` was taken.
fn add_branch_taken(branch: &mut BranchCoverageItem, taken: Option<i64>) {
  branch.taken = match (branch.taken, taken) {
    (Some(a), Some(b)) => Some(a.saturating_add(b)),
    (a, b) => a.or(b),
  };
  branch.is_hit |= branch.taken.is_some_and(|taken| taken > 0);
}

/// Whether the inline source map of the emitted `code` maps it to the single
/// source `original_source` (when the map has no sources content, only that
/// it has a single source), or maps none of it (all of it was generated,
/// like the export stubs of a module with only type exports).
fn is_emit_of(code: &str, original_source: &str) -> bool {
  let Some(source_map) = source_map_from_code(code.as_bytes())
    .and_then(|source_map| SourceMap::from_slice(&source_map).ok())
  else {
    return false;
  };
  match source_map.get_source_count() {
    0 => true,
    1 => source_map
      .get_source_contents(0)
      .is_none_or(|contents| contents == original_source),
    _ => false,
  }
}

/// Computes the aggregate line, branch, and function coverage percentages
/// across all reported files. A metric with no measurable items counts as 100%
/// (e.g. a `branches` threshold passes vacuously for files that have no
/// branches). Reuses the same accumulation and percentage helpers as the
/// summary reporter so the checked numbers match the printed ones.
fn aggregate_coverage_percentages(
  file_reports: &[(CoverageReport, String)],
) -> (f64, f64, f64) {
  let mut stats = reporter::CoverageStats::default();
  for (report, _) in file_reports {
    stats.add_report(report);
  }
  let percent = |hit: usize, miss: usize| -> f64 {
    util::calc_coverage_display_info(hit, miss).1 as f64
  };
  (
    percent(stats.line_hit, stats.line_miss),
    percent(stats.branch_hit, stats.branch_miss),
    percent(stats.fn_hit, stats.fn_miss),
  )
}

fn check_coverage_thresholds(
  file_reports: &[(CoverageReport, String)],
  thresholds: CoverageThresholds,
) -> Result<(), AnyError> {
  if thresholds.lines.is_none()
    && thresholds.branches.is_none()
    && thresholds.functions.is_none()
  {
    return Ok(());
  }

  let (line_percent, branch_percent, fn_percent) =
    aggregate_coverage_percentages(file_reports);

  let mut failures = Vec::new();
  let mut check = |name: &str, actual: f64, threshold: Option<f64>| {
    if let Some(threshold) = threshold
      && actual < threshold
    {
      // Floor the displayed value to two decimals so a near-miss like 89.999%
      // doesn't render as "90.00% is below the threshold of 90.00%".
      let actual = (actual * 100.0).floor() / 100.0;
      failures.push(format!(
        "  - {name} coverage {actual:.2}% is below the threshold of {threshold:.2}%"
      ));
    }
  };
  check("Line", line_percent, thresholds.lines);
  check("Branch", branch_percent, thresholds.branches);
  check("Function", fn_percent, thresholds.functions);

  if failures.is_empty() {
    Ok(())
  } else {
    Err(
      CoverageThresholdError(format!(
        "Coverage threshold not met:\n{}",
        failures.join("\n")
      ))
      .into(),
    )
  }
}

/// Returned when a configured coverage threshold is not met. A distinct type so
/// `deno test --coverage` can treat it as fatal while still tolerating benign
/// report-generation errors.
#[derive(Debug)]
pub struct CoverageThresholdError(pub String);

impl std::fmt::Display for CoverageThresholdError {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl std::error::Error for CoverageThresholdError {}

#[cfg(test)]
mod tests {
  use super::*;

  fn coverage_range(start: usize, end: usize) -> cdp::CoverageRange {
    cdp::CoverageRange {
      start_char_offset: start,
      end_char_offset: end,
      count: 1,
    }
  }

  fn function_coverage(
    name: &str,
    ranges: Vec<cdp::CoverageRange>,
  ) -> cdp::FunctionCoverage {
    cdp::FunctionCoverage {
      function_name: name.to_string(),
      ranges,
      is_block_coverage: true,
    }
  }

  fn with_inline_source_map(code: &str, source_map: &str) -> String {
    use base64::Engine;
    format!(
      "{code}//# sourceMappingURL=data:application/json;base64,{}",
      base64::prelude::BASE64_STANDARD.encode(source_map)
    )
  }

  #[test]
  fn script_runtime_len_is_the_top_level_range_end() {
    let coverage = cdp::ScriptCoverage {
      script_id: "1".to_string(),
      url: "file:///a.ts".to_string(),
      functions: vec![
        // an anonymous function at the start of the script
        function_coverage("", vec![coverage_range(0, 10)]),
        function_coverage("", vec![coverage_range(0, 42)]),
        function_coverage("f", vec![coverage_range(0, 50)]),
        function_coverage("", vec![coverage_range(12, 60)]),
      ],
    };
    assert_eq!(script_runtime_len(&coverage), Some(42));
    let coverage = cdp::ScriptCoverage {
      functions: vec![],
      ..coverage
    };
    assert_eq!(script_runtime_len(&coverage), None);
  }

  #[test]
  fn runtime_code_is_measured_without_its_source_map_in_utf16() {
    let code = with_inline_source_map("let a = \"\u{1F600}\";\n", "{}");
    // the emoji is two UTF-16 code units
    assert!(is_runtime_code(&code, Some(14)));
    assert!(!is_runtime_code(&code, Some(13)));
    assert!(is_runtime_code(&code, None));
  }

  #[test]
  fn emit_of_a_source_is_told_by_its_source_map() {
    let source_map = |contents: &str| {
      format!(
        r#"{{"version":3,"sources":["file:///a.ts"],"sourcesContent":[{}],"names":[],"mappings":"AAAA"}}"#,
        serde_json::to_string(contents).unwrap()
      )
    };
    let code = with_inline_source_map("a();\n", &source_map("a();"));
    assert!(is_emit_of(&code, "a();"));
    // an emit of the module's previous source
    assert!(!is_emit_of(&code, "b();"));
    // no source map
    assert!(!is_emit_of("a();\n", "a();"));
    // no sources content: a single source is all there is to check
    let code = with_inline_source_map(
      "a();\n",
      r#"{"version":3,"sources":["file:///a.ts"],"names":[],"mappings":"AAAA"}"#,
    );
    assert!(is_emit_of(&code, "a();"));
    let code = with_inline_source_map(
      "a();\n",
      r#"{"version":3,"sources":["file:///a.ts","file:///b.ts"],"names":[],"mappings":"AAAA"}"#,
    );
    assert!(!is_emit_of(&code, "a();"));
    // all generated (the type export stubs of a module of types)
    let code = with_inline_source_map(
      "let __vi_type_export_0__;\n",
      r#"{"version":3,"sources":[],"names":[],"mappings":""}"#,
    );
    assert!(is_emit_of(&code, "export type A = string;\n"));
  }

  fn report(
    found_lines: Vec<(usize, i64)>,
    named_functions: Vec<(&str, usize, i64)>,
    branches: Vec<(BlockKey, usize, usize, usize, Option<i64>)>,
  ) -> CoverageReport {
    CoverageReport {
      url: Url::parse("file:///a.ts").unwrap(),
      named_functions: named_functions
        .into_iter()
        .map(|(name, line_index, execution_count)| FunctionCoverageItem {
          name: name.to_string(),
          line_index,
          execution_count,
        })
        .collect(),
      branches: branches
        .into_iter()
        .map(
          |(block_key, block_number, line_index, branch_number, taken)| {
            BranchCoverageItem {
              line_index,
              block_number,
              branch_number,
              is_hit: taken.is_some_and(|taken| taken > 0),
              taken,
              block_key,
              // a range starting on its line, for the first arm
              content_line_index: (branch_number == 0).then_some(line_index),
            }
          },
        )
        .collect(),
      found_lines,
      output: None,
    }
  }

  fn function_key(line_index: usize, name: &str) -> BlockKey {
    BlockKey::Function {
      line_index: Some(line_index),
      name: name.to_string(),
      ordinal: 0,
    }
  }

  #[test]
  fn reports_of_different_emits_merge_line_by_line() {
    // `kind` and `plainOnly` ran in a plain run, `kind` and `vitestOnly` in a
    // Vitest one, whose emit numbers the functions differently
    let plain = report(
      vec![
        (0, 1),
        (1, 1),
        (2, 1),
        (4, 0),
        (7, 1),
        (8, 1),
        (11, 0),
        (12, 0),
      ],
      vec![("kind", 0, 1), ("plainOnly", 7, 1), ("vitestOnly", 11, 0)],
      vec![
        (function_key(0, "kind"), 1, 1, 0, Some(1)),
        (function_key(0, "kind"), 1, 1, 1, Some(0)),
      ],
    );
    let vitest = report(
      vec![
        (0, 1),
        (1, 1),
        (2, 0),
        (4, 1),
        (7, 0),
        (8, 0),
        (11, 1),
        (12, 1),
      ],
      vec![("kind", 0, 1), ("plainOnly", 7, 0), ("vitestOnly", 11, 1)],
      vec![
        (BlockKey::TopLevel, 0, 9, 0, Some(1)),
        (function_key(0, "kind"), 3, 1, 0, Some(0)),
        (function_key(0, "kind"), 3, 1, 1, Some(1)),
        (function_key(11, "vitestOnly"), 5, 12, 0, Some(1)),
      ],
    );
    let merged = merge_reports(vec![plain, vitest]).unwrap();
    assert_eq!(
      merged.found_lines,
      vec![
        (0, 2),
        (1, 2),
        (2, 1),
        (4, 1),
        (7, 1),
        (8, 1),
        (11, 1),
        (12, 1)
      ]
    );
    let functions = merged
      .named_functions
      .iter()
      .map(|f| (f.name.as_str(), f.line_index, f.execution_count))
      .collect::<Vec<_>>();
    assert_eq!(
      functions,
      vec![("kind", 0, 2), ("plainOnly", 7, 1), ("vitestOnly", 11, 1)]
    );
    let branches = merged
      .branches
      .iter()
      .map(|b| {
        (
          b.block_number,
          b.line_index,
          b.branch_number,
          b.taken,
          b.is_hit,
        )
      })
      .collect::<Vec<_>>();
    // the plain report's block numbers are kept; blocks only in the Vitest
    // one get new ones
    assert_eq!(
      branches,
      vec![
        (1, 1, 0, Some(1), true),
        (1, 1, 1, Some(1), true),
        (2, 9, 0, Some(1), true),
        (3, 12, 0, Some(1), true),
      ]
    );
  }

  #[test]
  fn branches_v8_left_out_of_one_report_are_inferred() {
    // `kind(-1)` in one form: the `if` block ran as often as the function, so
    // V8 reported only the code after it, which starts at the end of the
    // block's last line (3) and never ran; `kind(1)` in the other: the `if`
    // block (line 1) never ran, the rest ran as often as the function
    let mut negative = report(
      vec![(0, 1), (1, 1), (2, 1), (3, 1), (4, 0), (5, 1)],
      vec![("kind", 0, 1)],
      vec![
        (function_key(0, "kind"), 1, 3, 0, Some(0)),
        (function_key(0, "kind"), 1, 3, 1, Some(1)),
      ],
    );
    negative.branches[0].content_line_index = Some(4);
    let positive = report(
      vec![(0, 1), (1, 1), (2, 0), (3, 0), (4, 1), (5, 1)],
      vec![("kind", 0, 1)],
      vec![
        (function_key(0, "kind"), 2, 1, 0, Some(0)),
        (function_key(0, "kind"), 2, 1, 1, Some(1)),
      ],
    );
    let merged = merge_reports(vec![negative, positive]).unwrap();
    let branches = merged
      .branches
      .iter()
      .map(|b| {
        (
          b.block_number,
          b.line_index,
          b.branch_number,
          b.taken,
          b.is_hit,
        )
      })
      .collect::<Vec<_>>();
    // both arms of both branches were taken once, as in a profile of one
    // process running both
    assert_eq!(
      branches,
      vec![
        (1, 1, 0, Some(1), true),
        (1, 1, 1, Some(1), true),
        (1, 3, 0, Some(1), true),
        (1, 3, 1, Some(1), true),
      ]
    );
    assert_eq!(
      merged.found_lines,
      vec![(0, 2), (1, 2), (2, 1), (3, 1), (4, 1), (5, 2)]
    );
  }

  #[test]
  fn a_single_report_is_left_as_is() {
    let single = report(
      vec![(3, 0), (1, 1)],
      vec![("b", 3, 0), ("a", 1, 1)],
      vec![(function_key(3, "b"), 4, 3, 0, None)],
    );
    let merged = merge_reports(vec![single]).unwrap();
    assert_eq!(merged.found_lines, vec![(3, 0), (1, 1)]);
    assert_eq!(merged.named_functions[0].name, "b");
    assert_eq!(merged.branches[0].block_number, 4);
    assert!(merge_reports(vec![]).is_none());
  }

  #[test]
  fn branch_counts_of_a_block_that_did_not_run_merge() {
    let a = report(vec![], vec![], vec![(function_key(0, "f"), 0, 1, 0, None)]);
    let b = report(
      vec![],
      vec![],
      vec![(function_key(0, "f"), 2, 1, 0, Some(3))],
    );
    let merged = merge_reports(vec![a, b]).unwrap();
    assert_eq!(merged.branches.len(), 1);
    assert_eq!(merged.branches[0].block_number, 0);
    assert_eq!(merged.branches[0].taken, Some(3));
    assert!(merged.branches[0].is_hit);
  }

  #[test]
  fn a_profile_is_of_the_form_of_its_length() {
    let candidates = vec![
      RuntimeCode {
        code: "a();\n".to_string(),
        plain: true,
      },
      RuntimeCode {
        code: "a();\nlet b;\n".to_string(),
        plain: false,
      },
    ];
    assert_eq!(runtime_code_index(&candidates, Some(5)), Some(0));
    assert_eq!(runtime_code_index(&candidates, Some(12)), Some(1));
    // a length that tells nothing: the usual form
    assert_eq!(runtime_code_index(&candidates, Some(99)), Some(0));
    assert_eq!(runtime_code_index(&candidates, None), Some(0));
    // no usual form to fall back on
    assert_eq!(runtime_code_index(&candidates[1..], Some(5)), None);
    assert_eq!(runtime_code_index(&candidates[1..], Some(12)), Some(0));
  }

  /// The source substring credited as code on `line_index`: the line with any
  /// trailing comment and the whitespace before it removed from its end.
  /// Exercises `line_content_end_byte_offset` with real lexer comment ranges.
  fn code_on_line(source: &str, line_index: usize) -> &str {
    let comments = lex_comments(source, MediaType::JavaScript).comments;
    let text_lines = TextLines::new(source);
    let (start, end) = text_lines.line_range(line_index);
    let content_end =
      line_content_end_byte_offset(source, start, end, &comments);
    &source[start..content_end]
  }

  #[test]
  fn no_trailing_comment_keeps_whole_line() {
    assert_eq!(
      code_on_line("if (!x) throw new Error(\"e\");", 0),
      "if (!x) throw new Error(\"e\");",
    );
  }

  #[test]
  fn strips_trailing_line_comment() {
    assert_eq!(
      code_on_line("if (!x) throw new Error(\"e\"); // never taken", 0),
      "if (!x) throw new Error(\"e\");",
    );
  }

  #[test]
  fn strips_trailing_block_comment() {
    assert_eq!(code_on_line("foo(); /* done */", 0), "foo();");
  }

  #[test]
  fn strips_multiple_stacked_trailing_comments() {
    assert_eq!(code_on_line("foo(); /* a */ // b", 0), "foo();");
  }

  #[test]
  fn keeps_interior_comment_when_code_follows() {
    assert_eq!(
      code_on_line("foo(/* inline */ x);", 0),
      "foo(/* inline */ x);"
    );
  }

  #[test]
  fn keeps_leading_indentation() {
    assert_eq!(code_on_line("  return 1; // done", 0), "  return 1;");
  }

  #[test]
  fn comment_only_line_has_no_code() {
    assert_eq!(code_on_line("  // just a comment", 0), "");
  }

  #[test]
  fn a_line_below_a_surrogate_pair_starts_where_v8_says_it_does() {
    // The emoji is two scalar values, each one a surrogate pair, so V8 puts the
    // second line two code units further along than its scalar-value index. A
    // range covering the first line ends at that line's newline, UTF-16 offset
    // 7: short of the 8 the second line starts at, but past the 6 it used to be
    // compared against, which put the end of one line's range inside the line
    // below it.
    let source = "\"🕵🏻\";\nlet a;\n";
    let text_lines = TextLines::new(source);
    let utf16_map = Utf16Map::new(source);
    let second_line_start = text_lines.line_start(1_usize);

    assert_eq!(utf16_offset(&utf16_map, second_line_start), 8);
    assert_eq!(text_lines.char_index(second_line_start), 6);
    assert_eq!(utf16_offset(&utf16_map, text_lines.line_end(0_usize)), 7);

    // A range starting at that 7 belongs to the first line. Read as a
    // scalar-value offset instead it reaches into the line below, which is what
    // files a branch under the wrong line.
    assert_eq!(byte_offset(&utf16_map, 7), text_lines.line_end(0_usize));
    assert_eq!(text_lines.line_index(byte_offset(&utf16_map, 7)), 0);
    assert_eq!(
      text_lines.line_index(text_lines.byte_index_from_char_index(7)),
      1,
    );
  }
}
