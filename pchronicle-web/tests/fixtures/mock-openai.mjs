import http from "node:http";

const plan = {
  intent_summary: "Compare run outcomes by tool",
  scope_summary: "Current visible analysis scope",
  filters: [],
  groupings: ["status", "tool_name"],
  measures: ["run count", "average latency", "error rate"],
  expected_columns: ["status", "tool_name", "avg_latency_ms", "error_rate", "run_count"],
  suggested_view: "distribution",
  sql: "SELECT 'success' AS status, 'read_file' AS tool_name, 284.0 AS avg_latency_ms, 0.0 AS error_rate, 48 AS run_count UNION ALL SELECT 'failed', 'shell', 912.0, 0.25, 12 UNION ALL SELECT 'success', 'shell', 521.0, 0.05, 31",
  warnings: []
};

const interpretation = {
  observations: ["The returned rows contain more than one status and tool group."],
  inferences: ["Tool mix may help explain the observed outcome difference."],
  limitations: ["This interpretation is limited to the returned query evidence."],
  follow_ups: ["Only compare failed runs"],
  references: []
};

const server = http.createServer((request, response) => {
  response.setHeader("Access-Control-Allow-Origin", "*");
  response.setHeader("Access-Control-Allow-Headers", "authorization,content-type");
  response.setHeader("Access-Control-Allow-Methods", "POST,OPTIONS");
  if (request.method === "OPTIONS") {
    response.writeHead(204);
    response.end();
    return;
  }
  let raw = "";
  request.on("data", chunk => { raw += chunk; });
  request.on("end", () => {
    const body = JSON.parse(raw || "{}");
    const system = body.messages?.[0]?.content || "";
    const payload = system.includes("AnalysisInterpretation") ? interpretation : plan;
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({
      choices: [{ message: { role: "assistant", content: JSON.stringify(payload) } }]
    }));
  });
});

server.listen(9988, "127.0.0.1", () => {
  process.stdout.write("mock-openai http://127.0.0.1:9988/v1\n");
});
