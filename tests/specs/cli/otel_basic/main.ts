// Copyright 2018-2026 the Deno authors. MIT license.

const data = {
  spans: [],
  logs: [],
  metrics: [],
};

const isGrpc = Deno.env.get("OTEL_EXPORTER_OTLP_PROTOCOL") === "grpc";

let decodeGrpcBody;

if (isGrpc) {
  const {
    decodeExportTraceRequest,
    decodeExportMetricsRequest,
    decodeExportLogsRequest,
  } = await import("./proto_decode.ts");

  decodeGrpcBody = (path, bytes) => {
    if (path.includes("TraceService")) return decodeExportTraceRequest(bytes);
    if (path.includes("MetricsService")) {
      return decodeExportMetricsRequest(bytes);
    }
    if (path.includes("LogsService")) return decodeExportLogsRequest(bytes);
    return null;
  };
}

// With OTEL_TEST_COLLAPSE_REPEATED_EXPORTS=1, a metrics export identical to
// the one before it (timestamps and the non-deterministic v8js metrics aside)
// is dropped. A periodic export repeats the previous one exactly when nothing
// changed in between, so a slow host that fits in an extra tick before exit
// yields the same output; the periodic export and the one at exit still differ
// (observable instruments are observed only in the periodic one), so both
// must still arrive.
const collapseRepeatedExports =
  Deno.env.get("OTEL_TEST_COLLAPSE_REPEATED_EXPORTS") === "1";
let lastMetricsExport;

function metricsExportKey(metrics) {
  return JSON.stringify(
    metrics
      .filter((m) => !m.name.startsWith("v8js"))
      .sort((a, b) => a.name.localeCompare(b.name)),
    (key, value) =>
      key === "timeUnixNano" || key === "startTimeUnixNano" ? undefined : value,
  );
}

function collectData(body) {
  const exportMetrics = [];
  body.resourceMetrics?.forEach((rMetrics) => {
    rMetrics.scopeMetrics.forEach((sMetrics) => {
      exportMetrics.push(...sMetrics.metrics);
    });
  });
  if (exportMetrics.length > 0) {
    const key = metricsExportKey([...exportMetrics]);
    if (!collapseRepeatedExports || key !== lastMetricsExport) {
      data.metrics.push(...exportMetrics);
    }
    lastMetricsExport = key;
  }
  body.resourceLogs?.forEach((rLogs) => {
    rLogs.scopeLogs.forEach((sLogs) => {
      data.logs.push(...sLogs.logRecords);
    });
  });
  body.resourceSpans?.forEach((rSpans) => {
    rSpans.scopeSpans.forEach((sSpans) => {
      data.spans.push(...sSpans.spans);
    });
  });
}

async function handler(req) {
  if (isGrpc) {
    const url = new URL(req.url);
    const rawBody = new Uint8Array(await req.arrayBuffer());
    // Strip the 5-byte gRPC frame prefix (compression flag + length)
    const msgBytes = rawBody.slice(5);
    const body = decodeGrpcBody(url.pathname, msgBytes);
    if (!body) return new Response(null, { status: 404 });

    collectData(body);

    // Respond with gRPC OK (Trailers-Only form)
    return new Response(null, {
      status: 200,
      headers: {
        "content-type": "application/grpc",
        "grpc-status": "0",
      },
    });
  }

  const body = await req.json();
  collectData(body);
  return Response.json({ partialSuccess: {} }, { status: 200 });
}

let server;

function onListen({ port }) {
  const protocol = Deno.env.get("OTEL_EXPORTER_OTLP_PROTOCOL") || "http/json";
  const endpoint = `https://localhost:${port}`;
  const command = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--env-file=env_file",
      "-A",
      "-q",
      "--unstable-cron",
      Deno.args[0],
    ],
    env: {
      OTEL_EXPORTER_OTLP_ENDPOINT: endpoint,
      OTEL_EXPORTER_OTLP_PROTOCOL: protocol,
    },
    stdout: "null",
  });
  const child = command.spawn();
  child.status
    .then((status) => {
      if (status.signal) {
        throw new Error("child process failed: " + JSON.stringify(status));
      }
      return server.shutdown();
    })
    .then(() => {
      data.logs.sort((a, b) =>
        Number(
          BigInt(a.observedTimeUnixNano) - BigInt(b.observedTimeUnixNano),
        )
      );
      data.spans.sort((a, b) =>
        Number(BigInt(`0x${a.spanId}`) - BigInt(`0x${b.spanId}`))
      );
      // v8js metrics are non-deterministic
      data.metrics = data.metrics.filter((m) => !m.name.startsWith("v8js"));
      data.metrics.sort((a, b) => a.name.localeCompare(b.name));
      for (const metric of data.metrics) {
        if ("histogram" in metric) {
          metric.histogram.dataPoints.sort((a, b) => {
            const aKey = a.attributes
              .sort((x, y) => x.key.localeCompare(y.key))
              .map(({ key, value }) => `${key}:${JSON.stringify(value)}`)
              .join("|");
            const bKey = b.attributes
              .sort((x, y) => x.key.localeCompare(y.key))
              .map(({ key, value }) => `${key}:${JSON.stringify(value)}`)
              .join("|");
            return aKey.localeCompare(bKey);
          });

          for (const dataPoint of metric.histogram.dataPoints) {
            dataPoint.attributes.sort((a, b) => {
              return a.key.localeCompare(b.key);
            });
          }
        }
        if ("sum" in metric) {
          metric.sum.dataPoints.sort((a, b) => {
            const aKey = a.attributes
              .sort((x, y) => x.key.localeCompare(y.key))
              .map(({ key, value }) => `${key}:${JSON.stringify(value)}`)
              .join("|");
            const bKey = b.attributes
              .sort((x, y) => x.key.localeCompare(y.key))
              .map(({ key, value }) => `${key}:${JSON.stringify(value)}`)
              .join("|");
            return aKey.localeCompare(bKey);
          });

          for (const dataPoint of metric.sum.dataPoints) {
            dataPoint.attributes.sort((a, b) => {
              return a.key.localeCompare(b.key);
            });
          }
        }
      }
      console.log(JSON.stringify(data, null, 2));
    });
}

if (Deno.env.get("OTEL_DENO_VSOCK")) {
  server = Deno.serve({
    cid: -1,
    port: 4317,
    onListen,
    handler,
  });
} else {
  server = Deno.serve({
    key: Deno.readTextFileSync("../../../testdata/tls/localhost.key"),
    cert: Deno.readTextFileSync("../../../testdata/tls/localhost.crt"),
    port: 0,
    onListen,
    handler,
  });
}
