/** Pinned Pi 0.83.0 replay bridge, launched by the Rust engine. */

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

function load(filename) {
  return JSON.parse(fs.readFileSync(filename, "utf8"));
}

function writeJson(filename, value) {
  fs.mkdirSync(path.dirname(filename), { recursive: true, mode: 0o700 });
  fs.writeFileSync(filename, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}

function writeJsonl(filename, values) {
  fs.mkdirSync(path.dirname(filename), { recursive: true, mode: 0o700 });
  const body = values.map((value) => JSON.stringify(value)).join("\n");
  fs.writeFileSync(filename, body ? `${body}\n` : "", { mode: 0o600 });
}

function toolCalls(message) {
  return (Array.isArray(message?.content) ? message.content : []).filter(
    (part) => part?.type === "toolCall",
  );
}

function assistantConfig(events) {
  for (const event of events) {
    if (event?.type === "turn_end" && event?.message?.role === "assistant") {
      return event.message;
    }
  }
  throw new Error("Pi source trajectory has no assistant turn");
}

function safeHeaders() {
  const raw = process.env.PVISOR_PI_HEADERS_JSON;
  if (!raw) return {};
  const headers = JSON.parse(raw);
  if (!headers || typeof headers !== "object" || Array.isArray(headers)) {
    throw new Error("PVISOR_PI_HEADERS_JSON must contain an object");
  }
  for (const [name, value] of Object.entries(headers)) {
    if (/authorization|api-key|x-api-key/i.test(name) || /[\0\r\n]/.test(`${name}${value}`)) {
      throw new Error("PVISOR_PI_HEADERS_JSON contains an unsafe header");
    }
  }
  return Object.fromEntries(Object.entries(headers).map(([name, value]) => [name, String(value)]));
}

function modelSettings(request, events) {
  const original = assistantConfig(events);
  const model = process.env.MODEL_NAME || original.model;
  if (!model) throw new Error("Pi Replay requires MODEL_NAME or a model in the source trajectory");
  const api = process.env.PVISOR_PI_API || original.api || "openai-completions";
  const thinking = request.disable_thinking
    ? "off"
    : process.env.PVISOR_PI_THINKING_LEVEL || "medium";
  const definition = {
    id: model,
    name: model,
    reasoning: thinking !== "off",
    input: ["text"],
  };
  if (process.env.PVISOR_PI_CONTEXT_WINDOW) {
    definition.contextWindow = Number.parseInt(process.env.PVISOR_PI_CONTEXT_WINDOW, 10);
  }
  if (process.env.PVISOR_PI_MAX_OUTPUT_TOKENS) {
    definition.maxTokens = Number.parseInt(process.env.PVISOR_PI_MAX_OUTPUT_TOKENS, 10);
  }
  const headers = safeHeaders();
  if (request.session_id) headers["X-LiteLLM-Session-ID"] = request.session_id;
  return { provider: "sweeval", model, api, thinking, definition, headers };
}

async function importPi(packageJson) {
  const manifest = load(packageJson);
  const exported = manifest?.exports?.["."]?.import;
  if (manifest?.name !== "@earendil-works/pi-coding-agent" || typeof exported !== "string") {
    throw new Error("Pi runtime package manifest has no ESM root export");
  }
  const packageRoot = path.dirname(packageJson);
  const entrypoint = path.resolve(packageRoot, exported);
  if (!entrypoint.startsWith(`${packageRoot}${path.sep}`) || !fs.statSync(entrypoint).isFile()) {
    throw new Error("Pi runtime ESM root export escapes the package or is not a file");
  }
  return import(pathToFileURL(entrypoint).href);
}

function freshToolResult(call, result, isError) {
  return {
    role: "toolResult",
    toolCallId: call.id,
    toolName: call.name,
    content: result?.content ?? [],
    details: result?.details ?? {},
    ...(result?.usage !== undefined ? { usage: result.usage } : {}),
    ...(result?.addedToolNames?.length ? { addedToolNames: result.addedToolNames } : {}),
    isError,
    timestamp: Date.now(),
  };
}

async function executeCall(tools, call) {
  const tool = tools.get(call.name);
  if (!tool) throw new Error(`Pi Replay profile does not support tool ${JSON.stringify(call.name)}`);
  const started = Date.now();
  let result;
  let isError = false;
  try {
    result = await tool.execute(call.id, call.arguments ?? {}, undefined, () => {});
  } catch (error) {
    isError = true;
    result = {
      content: [{ type: "text", text: error instanceof Error ? error.message : String(error) }],
      details: {},
    };
  }
  const message = freshToolResult(call, result, isError);
  const returnCode =
    Number.isInteger(message.details?.exitCode) ? message.details.exitCode
      : Number.isInteger(message.details?.exit_code) ? message.details.exit_code
        : null;
  return {
    message,
    observation: {
      call_id: call.id,
      content: message.content,
      is_error: isError,
      return_code: returnCode,
      duration_ms: Date.now() - started,
      details: message.details,
    },
  };
}

async function run(request) {
  const events = load(request.source);
  if (!Array.isArray(events)) throw new Error("Pi replay source must contain an event array");
  if (!new Set(["replay_only", "replay_and_continue"]).has(request.mode)) {
    throw new Error(`unsupported Pi replay mode: ${request.mode}`);
  }
  const pi = await importPi(request.package_json);
  const settings = modelSettings(request, events);
  const baseUrl = process.env.OPENAI_BASE_URL || process.env.OPENAI_API_BASE || process.env.LLM_BASE_URL;
  const apiKey = process.env.OPENAI_API_KEY || process.env.LLM_API_KEY;
  if (request.mode === "replay_and_continue" && (!baseUrl || !apiKey)) {
    throw new Error("Pi Replay continuation requires an OpenAI-compatible base URL and API key");
  }

  fs.mkdirSync(request.config_dir, { recursive: true, mode: 0o700 });
  fs.mkdirSync(request.session_dir, { recursive: true, mode: 0o700 });
  if (baseUrl && apiKey) {
    writeJson(path.join(request.config_dir, "models.json"), {
      providers: {
        [settings.provider]: {
          baseUrl,
          api: settings.api,
          apiKey: "$OPENAI_API_KEY",
          headers: settings.headers,
          models: [settings.definition],
        },
      },
    });
  }
  writeJson(path.join(request.config_dir, "settings.json"), {
    defaultProvider: settings.provider,
    defaultModel: settings.model,
    defaultThinkingLevel: settings.thinking,
  });

  const nativeTools = new Map(pi.createCodingTools(request.workspace).map((tool) => [tool.name, tool]));
  const sessionManager = pi.SessionManager.create(request.workspace, request.session_dir, {
    id: "pvisor-replay",
  });
  const reconstructedEvents = [];
  const observations = [];
  let replayedBatches = 0;
  let prefixTurns = 0;

  for (const event of events) {
    if (event?.type === "message_end" && event?.message?.role === "user") {
      sessionManager.appendMessage(structuredClone(event.message));
      reconstructedEvents.push(structuredClone(event));
      continue;
    }
    if (event?.type !== "turn_end" || event?.message?.role !== "assistant") continue;
    const calls = toolCalls(event.message);
    if (calls.length > 0 && replayedBatches >= request.after_step) break;
    prefixTurns += 1;
    const assistant = structuredClone(event.message);
    sessionManager.appendMessage(assistant);
    const freshResults = [];
    for (const call of calls) {
      const fresh = await executeCall(nativeTools, call);
      freshResults.push(fresh.message);
      observations.push(fresh.observation);
      sessionManager.appendMessage(fresh.message);
    }
    reconstructedEvents.push({ ...structuredClone(event), toolResults: freshResults });
    if (calls.length > 0) {
      replayedBatches += 1;
      if (replayedBatches === request.after_step) break;
    }
  }
  if (replayedBatches !== request.after_step || prefixTurns !== request.prefix_model_turns) {
    throw new Error("Pi replay runner reconstructed a different boundary than the Rust plan");
  }
  writeJsonl(request.reconstructed, reconstructedEvents);
  writeJson(request.observations, observations);

  if (request.mode === "replay_only") {
    writeJson(request.result, {
      phase: "replayed",
      agent_status: "not_started",
      replayed_steps: replayedBatches,
      continued_steps: 0,
      trajectory: request.reconstructed,
      reconstructed: request.reconstructed,
      boundary_user_prompt_injected: false,
    });
    return;
  }

  const modelRuntime = await pi.ModelRuntime.create({
    authPath: path.join(request.config_dir, "auth.json"),
    modelsPath: path.join(request.config_dir, "models.json"),
  });
  const model = modelRuntime.getModel(settings.provider, settings.model);
  if (!model) throw new Error(`Pi Replay could not resolve model ${settings.provider}/${settings.model}`);
  const settingsManager = pi.SettingsManager.create(request.workspace, request.config_dir);
  const resourceLoader = new pi.DefaultResourceLoader({
    cwd: request.workspace,
    agentDir: request.config_dir,
    settingsManager,
    noExtensions: true,
    noSkills: true,
    noPromptTemplates: true,
    noThemes: true,
    noContextFiles: true,
  });
  await resourceLoader.reload();
  const created = await pi.createAgentSession({
    cwd: request.workspace,
    agentDir: request.config_dir,
    modelRuntime,
    model,
    thinkingLevel: settings.thinking,
    tools: ["read", "bash", "edit", "write"],
    sessionManager,
    settingsManager,
    resourceLoader,
  });
  const session = created.session;
  const liveEvents = [];
  let continuedSteps = 0;
  let reachedLimit = false;
  let terminalError = null;
  const remaining = request.max_steps == null ? null : request.max_steps - prefixTurns;
  const unsubscribe = session.subscribe((event) => {
    liveEvents.push(structuredClone(event));
    if (event.type === "turn_end") {
      continuedSteps += 1;
      if (event.message?.stopReason === "error") {
        terminalError = event.message.errorMessage || "Pi model request failed";
      }
      if (remaining !== null && continuedSteps >= remaining) {
        reachedLimit = true;
        session.agent.abort();
      }
    }
  });
  const boundaryPrompt = request.boundary_user_prompt;
  if (boundaryPrompt) {
    await session.prompt(String(boundaryPrompt), {
      expandPromptTemplates: false,
      source: "rpc",
    });
  } else {
    await session.agent.continue();
  }
  unsubscribe();
  session.dispose();
  if (terminalError) throw new Error(terminalError);
  if (continuedSteps === 0) throw new Error("Pi produced no live continuation turn");
  writeJsonl(request.continued, [...reconstructedEvents, ...liveEvents]);
  writeJson(request.result, {
    phase: "continued",
    agent_status: reachedLimit ? "max_steps" : "completed",
    replayed_steps: replayedBatches,
    continued_steps: continuedSteps,
    trajectory: request.continued,
    reconstructed: request.reconstructed,
    boundary_user_prompt_injected: Boolean(boundaryPrompt),
  });
}

if (process.argv.length !== 3) throw new Error("Pi runner expects one JSON request path");
await run(load(process.argv[2]));
