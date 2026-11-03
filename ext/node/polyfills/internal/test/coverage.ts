// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent and Node contributors. All rights reserved. MIT license.

// `--experimental-test-coverage`, Node 26.5.1's line, branch and function
// accounting (`lib/internal/test_runner/coverage.js`) and its table
// (`lib/internal/test_runner/utils.js` getCoverageReport), over the V8 coverage
// files a test file's process writes under `DENO_COVERAGE_DIR`. A transpiled
// file's (`.ts`) offsets are those of the emitted code; the process writes the
// emitted code's source map beside them (`<uuid>.sourcemap`, see
// `runtime/coverage.rs`), and its ranges are mapped back to the file's, as
// Node's `mapCoverageWithSourceMap` maps a source-mapped file's.

(function () {
"use strict";

const { primordials } = __bootstrap;
const {
  ArrayFrom,
  ArrayPrototypeFlatMap,
  ArrayPrototypeJoin,
  ArrayPrototypeMap,
  ArrayPrototypePop,
  ArrayPrototypePush,
  ArrayPrototypeReduce,
  ArrayPrototypeSort,
  JSONParse,
  JSONStringify,
  MathFloor,
  MathMax,
  MathMin,
  NumberParseInt,
  NumberPrototypeToFixed,
  ObjectAssign,
  RegExpPrototypeExec,
  RegExpPrototypeSymbolSplit,
  RegExpPrototypeTest,
  SafeMap,
  SafeMapIterator,
  SafeArrayIterator,
  SafeRegExp,
  SafeSet,
  StringPrototypeIncludes,
  StringPrototypeLocaleCompare,
  StringPrototypePadEnd,
  StringPrototypePadStart,
  StringPrototypeRepeat,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
} = primordials;

const kIgnoreRegex = new SafeRegExp(
  "/\\* node:coverage ignore next (?<count>\\d+ )?\\*/",
);
const kLineEndingRegex = new SafeRegExp("\\r?\\n$", "u");
const kLineSplitRegex = new SafeRegExp("(?<=\\r?\\n)", "u");
const kStatusRegex = new SafeRegExp(
  "/\\* node:coverage (?<status>enable|disable) \\*/",
);

const kJavaScriptRegex = new SafeRegExp("\\.(?:js|cjs|mjs)$");
const kSourceMapFileRegex = new SafeRegExp("\\.sourcemap$");

function builtin(name) {
  return globalThis.process.getBuiltinModule(name);
}

function coverageLine(line, startOffset, src) {
  const newlineLength =
    RegExpPrototypeExec(kLineEndingRegex, src)?.[0].length ??
      0;
  const endOffset = startOffset + src.length - newlineLength;
  return {
    __proto__: null,
    line,
    src,
    startOffset,
    endOffset,
    ignore: false,
    count: startOffset === endOffset ? 1 : 0,
  };
}

function getLines(fileUrl) {
  let source;
  try {
    source = builtin("node:fs").readFileSync(
      builtin("node:url").fileURLToPath(fileUrl),
      "utf8",
    );
  } catch {
    return null;
  }
  const linesWithBreaks = RegExpPrototypeSymbolSplit(kLineSplitRegex, source);
  let ignoreCount = 0;
  let enabled = true;
  let offset = 0;
  return ArrayPrototypeMap(linesWithBreaks, (line, i) => {
    const entry = coverageLine(i + 1, offset, line);
    offset += line.length;
    if (ignoreCount > 0) {
      ignoreCount--;
      entry.ignore = true;
    } else if (!enabled) {
      entry.ignore = true;
    }
    if (!entry.ignore) {
      const match = RegExpPrototypeExec(kIgnoreRegex, line);
      if (match !== null) {
        ignoreCount = NumberParseInt(match.groups?.count ?? 1, 10);
      }
    }
    const status = RegExpPrototypeExec(kStatusRegex, line)?.groups?.status;
    if (status) {
      ignoreCount = 0;
      enabled = status === "enable";
    }
    return entry;
  });
}

function toPercentage(covered, total) {
  return total === 0 ? 100 : (covered / total) * 100;
}

function mapRangeToLines(range, lines) {
  const { startOffset, endOffset, count } = range;
  const mappedLines = [];
  let ignoredLines = 0;
  let start = 0;
  let end = lines.length;
  let mid;
  while (start <= end) {
    mid = MathFloor((start + end) / 2);
    let line = lines[mid];
    if (startOffset >= line?.startOffset && startOffset <= line?.endOffset) {
      while (endOffset > line?.startOffset) {
        if (startOffset <= line.startOffset && endOffset >= line.endOffset) {
          line.count = count;
        }
        ArrayPrototypePush(mappedLines, line);
        if (line.ignore) ignoredLines++;
        mid++;
        line = lines[mid];
      }
      break;
    } else if (startOffset >= line?.endOffset) {
      start = mid + 1;
    } else {
      end = mid - 1;
    }
  }
  return { __proto__: null, lines: mappedLines, ignoredLines };
}

function doesRangeEqualOtherRange(range, other) {
  return range.startOffset === other.startOffset &&
    range.endOffset === other.endOffset;
}

function doesRangeContainOtherRange(range, other) {
  return range.startOffset <= other.startOffset &&
    range.endOffset >= other.endOffset;
}

function mergeCoverageRanges(oldFn, newFn) {
  const mergedRanges = new SafeSet();
  for (let i = 0; i < oldFn.ranges.length; ++i) {
    if (oldFn.ranges[i].count > 0) mergedRanges.add(oldFn.ranges[i]);
  }
  for (let i = 0; i < newFn.ranges.length; ++i) {
    const newRange = newFn.ranges[i];
    let exactMatch = false;
    for (let j = 0; j < oldFn.ranges.length; ++j) {
      const oldRange = oldFn.ranges[j];
      if (doesRangeEqualOtherRange(newRange, oldRange)) {
        oldRange.count += newRange.count;
        mergedRanges.add(oldRange);
        exactMatch = true;
        break;
      }
      if (oldRange.count === 0 && newRange.count === 0) {
        if (doesRangeContainOtherRange(oldRange, newRange)) {
          mergedRanges.add(newRange);
        } else if (doesRangeContainOtherRange(newRange, oldRange)) {
          mergedRanges.add(oldRange);
        }
      }
    }
    if (newRange.count > 0 && !exactMatch) mergedRanges.add(newRange);
  }
  oldFn.ranges = ArrayFrom(mergedRanges);
}

function mergeCoverageScripts(oldScript, newScript) {
  for (let i = 0; i < newScript.functions.length; ++i) {
    const newFn = newScript.functions[i];
    let found = false;
    for (let j = 0; j < oldScript.functions.length; ++j) {
      const oldFn = oldScript.functions[j];
      if (
        newFn.functionName === oldFn.functionName &&
        newFn.ranges?.[0].startOffset === oldFn.ranges?.[0].startOffset &&
        newFn.ranges?.[0].endOffset === oldFn.ranges?.[0].endOffset
      ) {
        found = true;
        if (newFn.isBlockCoverage) {
          if (oldFn.isBlockCoverage) {
            mergeCoverageRanges(oldFn, newFn);
          } else {
            oldFn.isBlockCoverage = true;
            oldFn.ranges = newFn.ranges;
          }
        }
        break;
      }
    }
    if (!found) ArrayPrototypePush(oldScript.functions, newFn);
  }
}

// The options' include / exclude globs, and `node_modules`.
function shouldSkip(url, options) {
  if (!StringPrototypeStartsWith(url, "file:")) return true;
  const path = builtin("node:path");
  const absolutePath = builtin("node:url").fileURLToPath(url);
  const relativePath = path.relative(options.cwd, absolutePath);
  const matches = (pattern) =>
    StringPrototypeStartsWith(pattern, "!")
      ? !(path.matchesGlob(relativePath, StringPrototypeSlice(pattern, 1)) ||
        path.matchesGlob(absolutePath, StringPrototypeSlice(pattern, 1)))
      : path.matchesGlob(relativePath, pattern) ||
        path.matchesGlob(absolutePath, pattern);
  for (
    const pattern of new SafeArrayIterator(options.coverageExcludeGlobs ?? [])
  ) {
    if (matches(pattern)) return true;
  }
  const include = options.coverageIncludeGlobs ?? [];
  if (include.length > 0) {
    for (const pattern of new SafeArrayIterator(include)) {
      if (matches(pattern)) return false;
    }
    return true;
  }
  if (StringPrototypeIncludes(url, "node-test-mock")) return true;
  return StringPrototypeIncludes(url, "/node_modules/");
}

// An `import` of a CommonJS file leaves two scripts under the file's URL: the
// file, and the generated module that re-exports it, whose offsets aren't the
// file's. A JavaScript file's own script spans the file.
function isFilesOwnScript(script, lengths) {
  if (!RegExpPrototypeTest(kJavaScriptRegex, script.url)) return true;
  let length = lengths.get(script.url);
  if (length === undefined) {
    try {
      length = builtin("node:fs").readFileSync(
        builtin("node:url").fileURLToPath(script.url),
        "utf8",
      ).length;
    } catch {
      length = -1;
    }
    lengths.set(script.url, length);
  }
  return script.functions[0]?.ranges[0]?.endOffset === length;
}

// The source maps the processes wrote beside their transpiled scripts'
// coverage, by script URL.
function readSourceMaps(directory) {
  const fs = builtin("node:fs");
  const path = builtin("node:path");
  const maps = new SafeMap();
  for (const name of new SafeArrayIterator(fs.readdirSync(directory))) {
    if (!RegExpPrototypeTest(kSourceMapFileRegex, name)) continue;
    try {
      const entry = JSONParse(
        fs.readFileSync(path.resolve(directory, name), "utf8"),
      );
      if (typeof entry?.url === "string" && !maps.has(entry.url)) {
        maps.set(entry.url, entry);
      }
    } catch { /* partial file */ }
  }
  return maps;
}

function lineStarts(lengths, separator) {
  const starts = [];
  let offset = 0;
  for (let i = 0; i < lengths.length; i++) {
    ArrayPrototypePush(starts, offset);
    offset += lengths[i] + separator;
  }
  return starts;
}

// The line of `offset` among `starts` (sorted), by binary search.
function lineOf(starts, offset) {
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const mid = MathFloor((low + high + 1) / 2);
    if (starts[mid] <= offset) low = mid;
    else high = mid - 1;
  }
  return low;
}

// A transpiled script's coverage in its file's offsets: each range's ends
// through the emitted code's source map (Node's `mapCoverageWithSourceMap`).
// A range that does not map is left out, as in Node; the script's own range
// spans the file.
function mapScriptWithSourceMap(script, entry) {
  let source;
  try {
    source = builtin("node:fs").readFileSync(
      builtin("node:url").fileURLToPath(script.url),
      "utf8",
    );
  } catch {
    return script;
  }
  let sourceMap;
  try {
    const { SourceMap } = builtin("node:module");
    sourceMap = new SourceMap(entry.data, {
      __proto__: null,
      lineLengths: entry.lineLengths,
    });
  } catch {
    return script;
  }
  const generatedStarts = lineStarts(entry.lineLengths, 1);
  const lastLine = entry.lineLengths.length - 1;
  const generatedLength = generatedStarts[lastLine] +
    entry.lineLengths[lastLine];
  const sourceLines = RegExpPrototypeSymbolSplit(kLineSplitRegex, source);
  const sourceStarts = lineStarts(
    ArrayPrototypeMap(sourceLines, (line) => line.length),
    0,
  );
  const toSource = (offset, isEnd) => {
    const at = MathMax(0, isEnd ? offset - 1 : offset);
    const line = lineOf(generatedStarts, at);
    const found = sourceMap.findEntry(line, at - generatedStarts[line]);
    if (
      found?.originalSource === undefined ||
      found.originalLine >= sourceLines.length
    ) {
      return -1;
    }
    const lineText = sourceLines[found.originalLine];
    const column = MathMin(found.originalColumn, lineText.length);
    return sourceStarts[found.originalLine] + column + (isEnd ? 1 : 0);
  };
  const functions = [];
  for (let j = 0; j < script.functions.length; j++) {
    const fn = script.functions[j];
    const ranges = [];
    let ownRangeMapped = false;
    for (let k = 0; k < fn.ranges.length; k++) {
      const range = fn.ranges[k];
      if (range.startOffset === 0 && range.endOffset >= generatedLength) {
        if (k === 0) ownRangeMapped = true;
        ArrayPrototypePush(ranges, {
          __proto__: null,
          startOffset: 0,
          endOffset: source.length,
          count: range.count,
        });
        continue;
      }
      const startOffset = toSource(range.startOffset, false);
      const endOffset = toSource(range.endOffset, true);
      if (startOffset < 0 || endOffset <= startOffset) continue;
      if (k === 0) ownRangeMapped = true;
      ArrayPrototypePush(ranges, {
        __proto__: null,
        startOffset,
        endOffset,
        count: range.count,
      });
    }
    // A function whose own range does not map is left out.
    if (!ownRangeMapped) continue;
    ArrayPrototypePush(functions, {
      __proto__: null,
      functionName: fn.functionName,
      isBlockCoverage: fn.isBlockCoverage,
      ranges,
    });
  }
  return {
    __proto__: null,
    scriptId: script.scriptId,
    url: script.url,
    functions,
  };
}

function readCoverage(directory, options) {
  const fs = builtin("node:fs");
  const path = builtin("node:path");
  const merged = new SafeMap();
  const lengths = new SafeMap();
  const mergedScripts = [];
  const sourceMaps = readSourceMaps(directory);
  for (const name of new SafeArrayIterator(fs.readdirSync(directory))) {
    if (RegExpPrototypeTest(kSourceMapFileRegex, name)) continue;
    let parsed;
    try {
      const file = path.resolve(directory, name);
      parsed = JSONParse(fs.readFileSync(file, "utf8"));
    } catch {
      continue;
    }
    const scripts = parsed.result ?? [parsed];
    for (let i = 0; i < scripts.length; i++) {
      let script = scripts[i];
      if (typeof script?.url !== "string") continue;
      if (shouldSkip(script.url, options)) continue;
      if (!isFilesOwnScript(script, lengths)) continue;
      const sourceMap = sourceMaps.get(script.url);
      if (sourceMap !== undefined) {
        script = mapScriptWithSourceMap(script, sourceMap);
      }
      const old = merged.get(script.url);
      if (old === undefined) {
        merged.set(script.url, script);
        ArrayPrototypePush(mergedScripts, script);
      } else mergeCoverageScripts(old, script);
    }
  }
  return mergedScripts;
}

function summarize(directory, options) {
  const coverage = readCoverage(directory, options);
  const summary = {
    __proto__: null,
    workingDirectory: options.cwd,
    files: [],
    totals: {
      __proto__: null,
      totalLineCount: 0,
      totalBranchCount: 0,
      totalFunctionCount: 0,
      coveredLineCount: 0,
      coveredBranchCount: 0,
      coveredFunctionCount: 0,
      coveredLinePercent: 0,
      coveredBranchPercent: 0,
      coveredFunctionPercent: 0,
    },
    thresholds: {
      __proto__: null,
      line: options.lineCoverage ?? 0,
      branch: options.branchCoverage ?? 0,
      function: options.functionCoverage ?? 0,
    },
  };
  for (let i = 0; i < coverage.length; ++i) {
    const { functions, url } = coverage[i];
    let totalBranches = 0;
    let totalFunctions = 0;
    let branchesCovered = 0;
    let functionsCovered = 0;
    const functionReports = [];
    const branchReports = [];
    const lines = getLines(url);
    if (!lines) continue;
    for (let j = 0; j < functions.length; ++j) {
      const { isBlockCoverage, ranges } = functions[j];
      let maxCountPerFunction = 0;
      for (let k = 0; k < ranges.length; ++k) {
        const range = ranges[k];
        maxCountPerFunction = MathMax(maxCountPerFunction, range.count);
        ObjectAssign(range, mapRangeToLines(range, lines));
        if (isBlockCoverage) {
          ArrayPrototypePush(branchReports, {
            __proto__: null,
            line: range.lines[0]?.line,
            count: range.count,
          });
          if (range.count !== 0 || range.ignoredLines === range.lines.length) {
            branchesCovered++;
          }
          totalBranches++;
        }
      }
      if (j > 0 && ranges.length > 0) {
        const range = ranges[0];
        ArrayPrototypePush(functionReports, {
          __proto__: null,
          name: functions[j].functionName,
          count: maxCountPerFunction,
          line: range.lines[0]?.line,
        });
        if (range.count !== 0 || range.ignoredLines === range.lines.length) {
          functionsCovered++;
        }
        totalFunctions++;
      }
    }
    let coveredCount = 0;
    const lineReports = [];
    for (let j = 0; j < lines.length; ++j) {
      const line = lines[j];
      if (!line.ignore) {
        ArrayPrototypePush(lineReports, {
          __proto__: null,
          line: line.line,
          count: line.count,
        });
      }
      if (line.count > 0 || line.ignore) coveredCount++;
    }
    ArrayPrototypePush(summary.files, {
      __proto__: null,
      path: builtin("node:url").fileURLToPath(url),
      totalLineCount: lines.length,
      totalBranchCount: totalBranches,
      totalFunctionCount: totalFunctions,
      coveredLineCount: coveredCount,
      coveredBranchCount: branchesCovered,
      coveredFunctionCount: functionsCovered,
      coveredLinePercent: toPercentage(coveredCount, lines.length),
      coveredBranchPercent: toPercentage(branchesCovered, totalBranches),
      coveredFunctionPercent: toPercentage(functionsCovered, totalFunctions),
      functions: functionReports,
      branches: branchReports,
      lines: lineReports,
    });
    const totals = summary.totals;
    totals.totalLineCount += lines.length;
    totals.totalBranchCount += totalBranches;
    totals.totalFunctionCount += totalFunctions;
    totals.coveredLineCount += coveredCount;
    totals.coveredBranchCount += branchesCovered;
    totals.coveredFunctionCount += functionsCovered;
  }
  const totals = summary.totals;
  totals.coveredLinePercent = toPercentage(
    totals.coveredLineCount,
    totals.totalLineCount,
  );
  totals.coveredBranchPercent = toPercentage(
    totals.coveredBranchCount,
    totals.totalBranchCount,
  );
  totals.coveredFunctionPercent = toPercentage(
    totals.coveredFunctionCount,
    totals.totalFunctionCount,
  );
  ArrayPrototypeSort(
    summary.files,
    (a, b) => StringPrototypeLocaleCompare(a.path, b.path),
  );
  return summary;
}

// The report table.
const kColumns = ["line %", "branch %", "funcs %"];
const kColumnsKeys = [
  "coveredLinePercent",
  "coveredBranchPercent",
  "coveredFunctionPercent",
];
const kSeparator = " | ";
const kHorizontalEllipsis = "\u2026";

function buildFileTree(summary) {
  const path = builtin("node:path");
  const tree = { __proto__: null };
  let treeDepth = 1;
  let longestFile = 0;
  for (let i = 0; i < summary.files.length; i++) {
    const file = summary.files[i];
    let longestPart = 0;
    const parts = StringPrototypeSplit(
      path.relative(summary.workingDirectory, file.path),
      path.sep,
    );
    let current = tree;
    for (let index = 0; index < parts.length; index++) {
      const part = parts[index];
      current[part] ||= { __proto__: null };
      current = current[part];
      if (index === parts.length - 1) current.file = file;
      longestPart = MathMax(longestPart, part.length);
    }
    treeDepth = MathMax(treeDepth, parts.length);
    longestFile = MathMax(longestPart, longestFile);
  }
  return { __proto__: null, tree, treeDepth, longestFile };
}

function truncateStart(string, width) {
  return string.length > width
    ? `${kHorizontalEllipsis}${
      StringPrototypeSlice(string, string.length - width + 1)
    }`
    : string;
}

function truncateEnd(string, width) {
  return string.length > width
    ? `${StringPrototypeSlice(string, 0, width - 1)}${kHorizontalEllipsis}`
    : string;
}

function formatLinesToRanges(values) {
  return ArrayPrototypeMap(
    ArrayPrototypeReduce(values, (prev, current, index, array) => {
      if ((index > 0) && ((current - array[index - 1]) === 1)) {
        prev[prev.length - 1][1] = current;
      } else {
        ArrayPrototypePush(prev, [current]);
      }
      return prev;
    }, []),
    (range) => ArrayPrototypeJoin(range, "-"),
  );
}

function getUncoveredLines(lines) {
  return ArrayPrototypeFlatMap(
    lines,
    (line) => (line.count === 0 ? line.line : []),
  );
}

function formatUncoveredLines(lines) {
  return ArrayPrototypeJoin(formatLinesToRanges(lines), " ");
}

function addTableLine(prefix, width) {
  return `${prefix}${StringPrototypeRepeat("-", width)}\n`;
}

// Node's `getCoverageReport(pad, summary, symbol, color, true)`.
function getCoverageReport(pad, summary, symbol, color, colors) {
  const process = globalThis.process;
  const prefix = `${pad}${symbol}`;
  let report = `${color}${prefix}start of coverage report\n`;
  const { tree, treeDepth, longestFile } = buildFileTree(summary);
  let filePadLength = longestFile + treeDepth - 1;
  if (color) filePadLength += 2;
  filePadLength = MathMax(filePadLength, "all files".length);
  if (filePadLength > (process.stdout.columns / 2)) {
    filePadLength = MathFloor(process.stdout.columns / 2);
  }
  const fileWidth = filePadLength + 2;
  const columnPadLengths = ArrayPrototypeMap(
    kColumns,
    (column) => MathMax(column.length, 6),
  );
  const columnsWidth = ArrayPrototypeReduce(
    columnPadLengths,
    (acc, columnPadLength) => acc + columnPadLength + 3,
    0,
  );
  let uncoveredLinesPadLength = ArrayPrototypeReduce(
    summary.files,
    (acc, file) =>
      MathMax(acc, formatUncoveredLines(getUncoveredLines(file.lines)).length),
    0,
  );
  uncoveredLinesPadLength = MathMax(
    uncoveredLinesPadLength,
    "uncovered lines".length,
  );
  const uncoveredLinesWidth = uncoveredLinesPadLength + 2;
  let tableWidth = fileWidth + columnsWidth + uncoveredLinesWidth;
  const availableWidth = (process.stdout.columns || Infinity) - prefix.length;
  if (tableWidth - availableWidth > 0) {
    filePadLength = MathMin(availableWidth * 0.5, filePadLength);
    uncoveredLinesPadLength = MathMax(
      availableWidth - columnsWidth - (filePadLength + 2) - 2,
      1,
    );
    tableWidth = availableWidth;
  } else {
    uncoveredLinesPadLength = Infinity;
  }

  function getCell(string, width, pad, truncate, coverage) {
    let result = string;
    if (pad) result = pad(result, width);
    if (truncate) result = truncate(result, width);
    if (color && coverage !== undefined) {
      if (coverage > 90) return `${colors.green}${result}${color}`;
      if (coverage > 50) return `${colors.yellow}${result}${color}`;
      return `${colors.red}${result}${color}`;
    }
    return result;
  }

  function writeReportLine(
    { file, depth = 0, coveragesColumns, fileCoverage, uncoveredLines },
  ) {
    const fileColumn = `${prefix}${StringPrototypeRepeat(" ", depth)}${
      getCell(
        file,
        filePadLength - depth,
        StringPrototypePadEnd,
        truncateStart,
        fileCoverage,
      )
    }`;
    const coverageColumns = ArrayPrototypeJoin(
      ArrayPrototypeMap(coveragesColumns, (coverage, j) => {
        const coverageText = typeof coverage === "number"
          ? NumberPrototypeToFixed(coverage, 2)
          : coverage;
        return getCell(
          coverageText,
          columnPadLengths[j],
          StringPrototypePadStart,
          false,
          coverage,
        );
      }),
      kSeparator,
    );
    const uncoveredLinesColumn = getCell(
      uncoveredLines,
      uncoveredLinesPadLength,
      false,
      truncateEnd,
    );
    return `${fileColumn}${kSeparator}${coverageColumns}${kSeparator}${uncoveredLinesColumn}\n`;
  }

  function printCoverageBodyTree(tree, depth = 0) {
    const path = builtin("node:path");
    for (const key in tree) {
      if (tree[key].file?.path) {
        const file = tree[key].file;
        const fileName = ArrayPrototypePop(
          StringPrototypeSplit(file.path, path.sep),
        );
        let fileCoverage = 0;
        const coverages = ArrayPrototypeMap(kColumnsKeys, (columnKey) => {
          const percent = file[columnKey];
          fileCoverage += percent;
          return percent;
        });
        fileCoverage /= kColumnsKeys.length;
        report += writeReportLine({
          __proto__: null,
          file: fileName,
          depth,
          coveragesColumns: coverages,
          fileCoverage,
          uncoveredLines: formatUncoveredLines(getUncoveredLines(file.lines)),
        });
      } else {
        report += writeReportLine({
          __proto__: null,
          file: key,
          depth,
          coveragesColumns: ArrayPrototypeMap(columnPadLengths, () => ""),
          fileCoverage: undefined,
          uncoveredLines: "",
        });
        printCoverageBodyTree(tree[key], depth + 1);
      }
    }
  }

  report += addTableLine(prefix, tableWidth);
  report += writeReportLine({
    __proto__: null,
    file: "file",
    coveragesColumns: kColumns,
    fileCoverage: undefined,
    uncoveredLines: "uncovered lines",
  });
  report += addTableLine(prefix, tableWidth);
  printCoverageBodyTree(tree);
  report += addTableLine(prefix, tableWidth);
  report += writeReportLine({
    __proto__: null,
    file: "all files",
    coveragesColumns: ArrayPrototypeMap(
      kColumnsKeys,
      (columnKey) => summary.totals[columnKey],
    ),
    fileCoverage: undefined,
    uncoveredLines: "",
  });
  report += addTableLine(prefix, tableWidth);
  report += `${prefix}end of coverage report\n`;
  if (color) report += colors.white;
  return report;
}

// A directory's scripts as the one file Node writes for a process.
function writeMerged(directory, file) {
  const fs = builtin("node:fs");
  const path = builtin("node:path");
  const result = [];
  for (const name of new SafeArrayIterator(fs.readdirSync(directory))) {
    if (RegExpPrototypeTest(kSourceMapFileRegex, name)) continue;
    try {
      const parsed = JSONParse(
        fs.readFileSync(path.resolve(directory, name), "utf8"),
      );
      const scripts = parsed.result ?? [parsed];
      for (let i = 0; i < scripts.length; i++) {
        ArrayPrototypePush(result, scripts[i]);
      }
    } catch { /* partial file */ }
  }
  // Node's `source-map-cache`: a transpiled script's map, by its URL.
  const sourceMapCache = { __proto__: null };
  let hasSourceMaps = false;
  for (
    const { 0: url, 1: entry } of new SafeMapIterator(readSourceMaps(directory))
  ) {
    hasSourceMaps = true;
    sourceMapCache[url] = {
      __proto__: null,
      lineLengths: entry.lineLengths,
      data: entry.data,
    };
  }
  fs.writeFileSync(
    file,
    JSONStringify(
      hasSourceMaps
        ? { result, "source-map-cache": sourceMapCache }
        : { result },
    ),
  );
}

// The thresholds' failures, as Node's root test reports them.
function thresholdFailures(summary) {
  const failures = [];
  const checks = [
    ["line", summary.totals.coveredLinePercent, summary.thresholds.line],
    ["branch", summary.totals.coveredBranchPercent, summary.thresholds.branch],
    [
      "function",
      summary.totals.coveredFunctionPercent,
      summary.thresholds.function,
    ],
  ];
  for (let i = 0; i < checks.length; i++) {
    const { 0: name, 1: actual, 2: threshold } = checks[i];
    if (actual < threshold) {
      ArrayPrototypePush(
        failures,
        `Error: ${
          NumberPrototypeToFixed(actual, 2)
        }% ${name} coverage does not meet threshold of ${threshold}%.`,
      );
    }
  }
  return failures;
}

return { summarize, getCoverageReport, thresholdFailures, writeMerged };
})();
