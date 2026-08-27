import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type CuaEnv = Record<string, string | undefined>;

export interface CuaOptions {
  profile?: string;
  bin?: string;
  env?: CuaEnv;
}

export interface RunebookOptions {
  traceDir?: string;
}

export interface RpcOptions {
  sessionId?: string;
}

export interface OwnerSession {
  sessionId: string;
  raw: Json;
}

export interface RunStep {
  id?: string;
  do: string;
  save_as?: string;
  on_error?: "stop" | "continue" | "ask" | "rollback";
  [key: string]: Json | undefined;
}

export interface RunStepsOptions extends RunebookOptions {
  name?: string;
  trace?: boolean;
  onError?: "stop" | "continue" | "ask" | "rollback";
}

export interface ScreenshotOptions {
  maxWidth?: number;
  encoding?: "png" | "jpeg";
  forceFresh?: boolean;
  includeBytes?: boolean;
}

export interface WindowCaptureOptions {
  windowId: number;
  maxWidth?: number;
  encoding?: "png" | "jpeg";
  includeBytes?: boolean;
}

export interface EventsOptions {
  after?: number;
  timeoutMs?: number;
}

export interface UiStepOptions {
  label: string;
  source?: string;
  task?: string;
  tool?: string;
  stepIndex?: number;
  stepTotal?: number;
  ttlMs?: number;
}

export interface UiReplyOptions {
  text: string;
  source?: string;
  ttlMs?: number;
}

export interface ProfileCreateOptions {
  name: string;
  mode?: "observe" | "supervised" | "autonomous";
  durationMs?: number;
  capabilities?: Json;
}

export interface DispatchOptions {
  session?: OwnerSession | string;
}

export interface DispatchFrameOptions extends DispatchOptions {
  sourceFrame: Json;
  action: Json;
}

export class Cua {
  private readonly profile: string;
  private readonly bin: string;
  private readonly env: CuaEnv;

  private constructor(options: CuaOptions = {}) {
    this.profile = options.profile ?? "default";
    this.bin = options.bin ?? "cua";
    this.env = { ...process.env, ...options.env };
  }

  static async connect(options: CuaOptions = {}): Promise<Cua> {
    return new Cua(options);
  }

  async run(file: string, options: RunebookOptions = {}): Promise<Json> {
    const args = ["--profile", this.profile, "run", file, "--json"];
    if (options.traceDir) {
      args.push("--trace-dir", options.traceDir);
    }
    return this.execJson(args);
  }

  async runInline(runebookToml: string, options: RunebookOptions = {}): Promise<Json> {
    const dir = await mkdtemp(join(tmpdir(), "cua-runebook-"));
    const file = join(dir, `${randomUUID()}.cua.toml`);
    try {
      await writeFile(file, runebookToml, "utf8");
      return await this.run(file, options);
    } finally {
      await rm(dir, { force: true, recursive: true });
    }
  }

  async runSteps(steps: RunStep[], options: RunStepsOptions = {}): Promise<Json> {
    const runebook = renderRunebook({
      profile: this.profile,
      name: options.name ?? "typescript-sdk",
      trace: options.trace ?? false,
      onError: options.onError,
      steps,
    });
    return this.runInline(runebook, options);
  }

  async rpc(method: string, params: Json = {}, options: RpcOptions = {}): Promise<Json> {
    const runebook = [
      'schema = "cua.runebook.v1"',
      "",
      "[run]",
      'name = "typescript-rpc"',
      `profile = ${tomlString(this.profile)}`,
      "trace = false",
      "",
      "[[steps]]",
      `id = ${tomlString("rpc")}`,
      `do = ${tomlString(method)}`,
      `params = ${tomlValue(params)}`,
      options.sessionId ? `session_id = ${tomlString(options.sessionId)}` : "",
    ]
      .filter(Boolean)
      .join("\n");
    const report = await this.runInline(runebook);
    return report;
  }

  async manifest(): Promise<Json> {
    return this.step("manifest", {}, "manifest");
  }

  async schemas(): Promise<Json> {
    return this.step("schemas", {}, "schemas");
  }

  async metrics(): Promise<Json> {
    return this.step("metrics", {}, "metrics");
  }

  async status(): Promise<Json> {
    return this.execJson(["--profile", this.profile, "status", "--json"]);
  }

  async configStatus(): Promise<Json> {
    return this.execJson(["--profile", this.profile, "config", "status", "--json"]);
  }

  async acquireOwner(clientName = "typescript sdk", ttlMs?: number): Promise<OwnerSession> {
    const args = [
      "--profile",
      this.profile,
      "session",
      "acquire",
      "--role",
      "owner",
      "--client-name",
      clientName,
      "--json",
    ];
    if (ttlMs !== undefined) {
      args.push("--ttl-ms", String(ttlMs));
    }
    const raw = await this.execJson(args);
    const sessionId = readSessionId(raw);
    return { sessionId, raw };
  }

  async cancelSession(session: OwnerSession | string, targetSessionId?: string): Promise<Json> {
    return this.rpc(
      "session.cancel",
      compact({
        schema_version: "cua.v1",
        session_id: sessionIdOf(session),
        target_session_id: targetSessionId,
      }),
    );
  }

  async sessionStatus(): Promise<Json> {
    return this.rpc("session.status");
  }

  async profileStatus(): Promise<Json> {
    return this.step("profile.status", {}, "profile");
  }

  async createProfile(options: ProfileCreateOptions, session?: OwnerSession | string): Promise<Json> {
    if (session) {
      return this.rpc(
        "profile.create",
        compact({
          name: options.name,
          mode: options.mode ?? "supervised",
          duration_ms: options.durationMs,
          capabilities: options.capabilities,
        }),
        { sessionId: sessionIdOf(session) },
      );
    }
    return this.step(
      "profile.create",
      {
        name: options.name,
        mode: options.mode ?? "supervised",
        duration_ms: options.durationMs,
        capabilities: options.capabilities,
      },
      "profile",
    );
  }

  async activateProfile(session?: OwnerSession | string): Promise<Json> {
    if (session) {
      return this.rpc("profile.activate", {}, { sessionId: sessionIdOf(session) });
    }
    return this.step("profile.activate", {}, "profile");
  }

  async requestAccessibility(): Promise<Json> {
    return this.step("permissions.request_accessibility", {}, "permissions");
  }

  async observe(): Promise<Json> {
    return this.step("observe", {}, "desktop");
  }

  async screenshot(options: ScreenshotOptions = {}): Promise<Json> {
    return this.step(
      "screenshot",
      {
        max_width: options.maxWidth,
        encoding: options.encoding,
        force_fresh: options.forceFresh,
        include_bytes: options.includeBytes,
      },
      "screenshot",
    );
  }

  async windowCapture(options: WindowCaptureOptions): Promise<Json> {
    return this.step(
      "window.capture",
      {
        window_id: options.windowId,
        max_width: options.maxWidth,
        encoding: options.encoding,
        include_bytes: options.includeBytes,
      },
      "window",
    );
  }

  async context(options: ScreenshotOptions = {}): Promise<Json> {
    return this.step(
      "context",
      {
        max_width: options.maxWidth,
        encoding: options.encoding,
        force_fresh: options.forceFresh,
        include_bytes: options.includeBytes,
      },
      "context",
    );
  }

  async events(options: EventsOptions = {}): Promise<Json> {
    return this.step(
      "events",
      {
        after: options.after,
        timeout_ms: options.timeoutMs,
      },
      "events",
    );
  }

  async uiStep(options: UiStepOptions): Promise<Json> {
    return this.step(
      "ui.step",
      {
        label: options.label,
        source: options.source,
        task: options.task,
        tool: options.tool,
        step_index: options.stepIndex,
        step_total: options.stepTotal,
        ttl_ms: options.ttlMs,
      },
      "ui",
    );
  }

  async uiIsland(state: "expanded" | "collapsed" | "toggle", source?: string): Promise<Json> {
    return this.step("ui.island", { state, source }, "ui");
  }

  async uiReply(options: UiReplyOptions): Promise<Json> {
    return this.step("ui.reply", { text: options.text, source: options.source, ttl_ms: options.ttlMs }, "ui");
  }

  async uiMode(mode: "headful" | "headless", source?: string): Promise<Json> {
    return this.step("ui.mode", { mode, source }, "ui");
  }

  async clipboardRead(allowSensitive = false): Promise<Json> {
    return this.step("clipboard.read", { allow_sensitive: allowSensitive }, "clipboard");
  }

  async clipboardWrite(text: string, session?: OwnerSession | string): Promise<Json> {
    return this.rpc(
      "clipboard.write",
      { schema_version: "cua.v1", text },
      session ? { sessionId: sessionIdOf(session) } : {},
    );
  }

  async pause(session: OwnerSession | string): Promise<Json> {
    return this.rpc("control.pause", {}, { sessionId: sessionIdOf(session) });
  }

  async resume(session: OwnerSession | string): Promise<Json> {
    return this.rpc("control.resume", {}, { sessionId: sessionIdOf(session) });
  }

  async killSwitch(session: OwnerSession | string): Promise<Json> {
    return this.rpc("control.kill_switch", {}, { sessionId: sessionIdOf(session) });
  }

  async dispatch(action: Json, options: DispatchOptions = {}): Promise<Json> {
    return this.rpc("input.dispatch", action, rpcSession(options));
  }

  async dispatchFrame(options: DispatchFrameOptions): Promise<Json> {
    return this.rpc(
      "input.dispatch_frame",
      {
        schema_version: "cua.v1",
        source_frame: options.sourceFrame,
        action: options.action,
      },
      rpcSession(options),
    );
  }

  async openApp(app: string, options: DispatchOptions = {}): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", action: "open_app", app }, options);
  }

  async shell(command: string, options: DispatchOptions = {}): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", action: "shell", command }, options);
  }

  async aegis(args: string[], options: DispatchOptions = {}): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", action: "aegis", args }, options);
  }

  async ctx(args: string[], options: DispatchOptions = {}): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", action: "ctx", args }, options);
  }

  private async step(action: string, fields: Record<string, Json | undefined>, saveAs: string): Promise<Json> {
    const report = await this.runSteps([{ id: saveAs, do: action, save_as: saveAs, ...fields }]);
    return readResult(report, saveAs);
  }

  private async execJson(args: string[]): Promise<Json> {
    const result = await execFile(this.bin, args, this.env);
    if (result.code !== 0) {
      throw new Error(result.stderr.trim() || result.stdout.trim() || `cua exited ${result.code}`);
    }
    try {
      return JSON.parse(result.stdout) as Json;
    } catch (error) {
      throw new Error(`cua returned non-JSON output: ${(error as Error).message}\n${result.stdout}`);
    }
  }
}

function renderRunebook(input: {
  profile: string;
  name: string;
  trace: boolean;
  onError?: "stop" | "continue" | "ask" | "rollback";
  steps: RunStep[];
}): string {
  const lines = [
    'schema = "cua.runebook.v1"',
    "",
    "[run]",
    `name = ${tomlString(input.name)}`,
    `profile = ${tomlString(input.profile)}`,
    `trace = ${input.trace ? "true" : "false"}`,
    input.onError ? `on_error = ${tomlString(input.onError)}` : "",
    "",
  ].filter(Boolean);
  for (const step of input.steps) {
    lines.push("[[steps]]");
    const { do: action, ...fields } = step;
    lines.push(`do = ${tomlString(action)}`);
    for (const [key, value] of Object.entries(fields)) {
      if (value !== undefined) {
        lines.push(`${tomlKey(key)} = ${tomlValue(value)}`);
      }
    }
    lines.push("");
  }
  return lines.join("\n");
}

function execFile(
  bin: string,
  args: string[],
  env: CuaEnv,
): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { env });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code: number | null) => resolve({ code: code ?? 1, stdout, stderr }));
  });
}

function readSessionId(value: Json): string {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const session = value.session;
    if (session && typeof session === "object" && !Array.isArray(session)) {
      const id = session.session_id;
      if (typeof id === "string") {
        return id;
      }
    }
  }
  throw new Error("cua session response did not include session.session_id");
}

function readResult(value: Json, key: string): Json {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const results = value.results;
    if (results && typeof results === "object" && !Array.isArray(results) && key in results) {
      return results[key];
    }
  }
  throw new Error(`cua runebook response did not include results.${key}`);
}

function sessionIdOf(session: OwnerSession | string): string {
  return typeof session === "string" ? session : session.sessionId;
}

function rpcSession(options: DispatchOptions): RpcOptions {
  return options.session ? { sessionId: sessionIdOf(options.session) } : {};
}

function compact(value: Record<string, Json | undefined>): Record<string, Json> {
  return Object.fromEntries(Object.entries(value).filter(([, field]) => field !== undefined && field !== null)) as Record<
    string,
    Json
  >;
}

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function tomlKey(key: string): string {
  return /^[A-Za-z_][A-Za-z0-9_-]*$/.test(key) ? key : tomlString(key);
}

function tomlValue(value: Json | undefined): string {
  if (value === undefined || value === null) {
    return "{}";
  }
  if (typeof value === "string") {
    return tomlString(value);
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(tomlValue).join(", ")}]`;
  }
  return `{ ${Object.entries(value)
    .filter(([, field]) => field !== null)
    .map(([key, field]) => `${tomlKey(key)} = ${tomlValue(field)}`)
    .join(", ")} }`;
}
