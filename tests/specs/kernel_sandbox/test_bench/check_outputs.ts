// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The confined test run still wrote its coverage and its JUnit report.
const names = [...Deno.readDirSync("cov")].map((e) => e.name);
const coverage = names.some((name) => name.endsWith(".json"));
const junit = Deno.readTextFileSync("report.xml").includes("<testsuites");
console.log(`coverage written: ${coverage}`);
console.log(`junit written: ${junit}`);
