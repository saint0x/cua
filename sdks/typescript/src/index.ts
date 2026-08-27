import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { createConnection, Socket } from "node:net";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type CuaEnv = Record<string, string | undefined>;

export interface CuaOptions {
  profile?: string;
  bin?: string;
  env?: CuaEnv;
  transport?: "unix" | "cli";
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

export interface VisualSessionOptions {
  maxWidth?: number;
  fps?: number;
  includeBytes?: boolean;
  durationMs?: number;
  queueDepth?: number;
  frames?: number;
  session?: OwnerSession | string;
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

export interface InboundMessageOptions {
  source?: string;
  idempotencyKey?: string;
  payload?: Json;
  replyUrl?: string;
  ttlMs?: number;
}

export interface WebhookMessageOptions extends InboundMessageOptions {
  source: string;
}

export interface WebhookSubscribeOptions {
  source: string;
  secret?: string;
  replyUrl?: string;
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
  session: OwnerSession | string;
}

export interface DispatchFrameOptions extends DispatchOptions {
  sourceFrame: Json;
  action: Json;
}

export interface ModelEvalOptions {
  live?: boolean;
  maxCalls?: number;
  maxOutputTokens?: number;
}

export class Cua {
  private readonly profile: string;
  private readonly bin: string;
  private readonly env: CuaEnv;
  private readonly transport: "unix" | "cli";

  private constructor(options: CuaOptions = {}) {
    this.profile = options.profile ?? "default";
    this.bin = options.bin ?? "cua";
    this.env = { ...process.env, ...options.env };
    this.transport = options.transport ?? "unix";
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
    if (this.transport === "unix") {
      try {
        return await this.unixRpc(method, params, options.sessionId);
      } catch (error) {
        if (!isMissingUnixSocketError(error)) {
          throw error;
        }
      }
    }
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
      `save_as = ${tomlString("rpc")}`,
      'do = "rpc"',
      `method = ${tomlString(method)}`,
      `params = ${tomlValue(params)}`,
      options.sessionId ? `session_id = ${tomlString(options.sessionId)}` : "",
    ]
      .filter(Boolean)
      .join("\n");
    const report = await this.runInline(runebook);
    return readResult(report, "rpc");
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

  async attest(audience: string, nonce: string, session?: OwnerSession | string): Promise<Json> {
    return this.rpc(
      "attestation.sign",
      {
        schema_version: "cua.v1",
        audience,
        nonce,
      },
      session ? { sessionId: sessionIdOf(session) } : {},
    );
  }

  async acquireOwner(clientName = "typescript sdk", ttlMs?: number): Promise<OwnerSession> {
    const sessionId = randomUUID();
    const args = [
      "--profile",
      this.profile,
      "session",
      "acquire",
      sessionId,
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

  async heartbeatOwner(session: OwnerSession | string, ttlMs?: number): Promise<OwnerSession> {
    const raw = await this.rpc("session.heartbeat", compact({
      schema_version: "cua.v1",
      session_id: sessionIdOf(session),
      ttl_ms: ttlMs,
    }));
    return { sessionId: readSessionId(raw), raw };
  }

  async sessionStatus(): Promise<Json> {
    return this.rpc("session.status");
  }

  async inboxPublish(text: string, options: InboundMessageOptions = {}): Promise<Json> {
    return this.rpc("inbox.publish", inboundMessageBody(text, options, "typescript-sdk"));
  }

  async inboxAfter(afterSequence = 0): Promise<Json> {
    return this.rpc("inbox.after", { after_sequence: afterSequence });
  }

  async inboxStatus(messageId: string): Promise<Json> {
    return this.rpc("inbox.status", { message_id: messageId });
  }

  async webhookPublish(text: string, options: WebhookMessageOptions): Promise<Json> {
    return this.rpc("webhook.publish", inboundMessageBody(text, options, options.source));
  }

  async webhookSubscribe(options: WebhookSubscribeOptions): Promise<Json> {
    return this.rpc(
      "webhook.subscribe",
      compact({
        schema_version: "cua.v1",
        source: options.source,
        shared_secret: options.secret,
        reply_url: options.replyUrl,
      }),
    );
  }

  async webhookStatus(source: string): Promise<Json> {
    return this.rpc("webhook.status", { source });
  }

  async profileStatus(): Promise<Json> {
    return this.step("profile.status", {}, "profile");
  }

  async createProfile(options: ProfileCreateOptions, session: OwnerSession | string): Promise<Json> {
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

  async activateProfile(session: OwnerSession | string): Promise<Json> {
    return this.rpc("profile.activate", {}, { sessionId: sessionIdOf(session) });
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
    if (options.timeoutMs !== undefined && options.after !== undefined) {
      return this.rpc("events.wait", { after_sequence: options.after, timeout_ms: options.timeoutMs });
    }
    if (options.after !== undefined) {
      return this.rpc("events.after", { after_sequence: options.after });
    }
    return this.rpc("events.snapshot");
  }

  async visualFrames(options: VisualSessionOptions = {}): Promise<Json> {
    const frames = options.frames ?? 3;
    const args = [
      "--profile",
      this.profile,
      "stream",
      "--unix",
      "--frames",
      String(frames),
      "--fps",
      String(options.fps ?? 10),
      "--max-width",
      String(options.maxWidth ?? 1280),
      "--json",
    ];
    if (options.includeBytes) {
      args.push("--include-bytes");
    }
    return this.execJson(args);
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

  async clipboardWrite(text: string, session: OwnerSession | string): Promise<Json> {
    return this.rpc(
      "clipboard.write",
      { schema_version: "cua.v1", text },
      { sessionId: sessionIdOf(session) },
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

  async dispatch(action: Json, options: DispatchOptions): Promise<Json> {
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

  async visualSession(options: VisualSessionOptions = {}): Promise<CuaVisualSession> {
    const socketPath = profileSocketPath(this.profile, this.env);
    const token = await profileToken(this.profile, this.env);
    const socket = await connectUnix(socketPath);
    const session = new CuaVisualSession(socket);
    socket.write(
      `${JSON.stringify({
        id: randomUUID(),
        token,
        session_id: options.session ? sessionIdOf(options.session) : undefined,
        method: "visual.session",
        params: compact({
          schema_version: "cua.v1",
          max_width: options.maxWidth,
          fps: options.fps,
          include_bytes: options.includeBytes ?? false,
          duration_ms: options.durationMs,
          queue_depth: options.queueDepth,
        }),
      })}\n`,
    );
    return session;
  }

  async openApp(app: string, options: DispatchOptions): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", kind: "open_app", app_name: app }, options);
  }

  async shell(command: string, options: DispatchOptions): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", kind: "shell_exec", command, timeout_ms: 5000 }, options);
  }

  async aegis(args: string[], options: DispatchOptions): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", kind: "aegis", args, timeout_ms: 15000 }, options);
  }

  async ctx(args: string[], options: DispatchOptions): Promise<Json> {
    return this.dispatch({ schema_version: "cua.v1", kind: "ctx", args, timeout_ms: 5000 }, options);
  }

  async traceVerify(dir: string): Promise<Json> {
    return this.execJson(["--profile", this.profile, "trace", "verify", dir, "--json"]);
  }

  async traceReplay(dir: string, dryRun = false): Promise<Json> {
    const args = ["--profile", this.profile, "trace", "replay", dir, "--json"];
    if (dryRun) {
      args.push("--dry-run");
    }
    return this.execJson(args);
  }

  async modelEval(options: ModelEvalOptions = {}): Promise<Json> {
    const args = ["model", "eval", "--json"];
    if (options.live) {
      args.push("--live");
    }
    if (options.maxCalls !== undefined) {
      args.push("--max-calls", String(options.maxCalls));
    }
    if (options.maxOutputTokens !== undefined) {
      args.push("--max-output-tokens", String(options.maxOutputTokens));
    }
    return this.execJson(args);
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

  private async unixRpc(method: string, params: Json, sessionId?: string): Promise<Json> {
    const token = await this.loadToken();
    const request = {
      id: randomUUID(),
      token,
      session_id: sessionId,
      method,
      params,
    };
    const line = await unixRequest(profileSocketPath(this.profile, this.env), JSON.stringify(compact(request)));
    const response = JSON.parse(line) as Json;
    if (response && typeof response === "object" && !Array.isArray(response)) {
      if (response.ok === true) {
        return response.result ?? null;
      }
      throw new CuaProtocolError(method, response.error ?? null);
    }
    throw new Error(`cua unix response for ${method} was not an object`);
  }

  private async loadToken(): Promise<string> {
    const override = this.env.CUA_HTTP_TOKEN?.trim();
    if (override) {
      return override;
    }
    return (await readFile(profileTokenPath(this.profile, this.env), "utf8")).trim();
  }
}

export class CuaProtocolError extends Error {
  readonly method: string;
  readonly error: Json;

  constructor(method: string, error: Json) {
    super(`cua ${method} failed: ${JSON.stringify(error)}`);
    this.name = "CuaProtocolError";
    this.method = method;
    this.error = error;
  }
}

export class CuaVisualSession {
  private buffer = "";
  private messages: Json[] = [];
  private waiters: Array<(message: Json | null) => void> = [];
  private ended = false;
  private closeSent = false;

  constructor(private readonly socket: Socket) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.receive(chunk));
    socket.on("end", () => this.finish());
    socket.on("close", () => this.finish());
  }

  async nextMessage(): Promise<Json | null> {
    const message = this.messages.shift();
    if (message !== undefined) {
      return message;
    }
    if (this.ended) {
      return null;
    }
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  async nextFrame(): Promise<Json | null> {
    for (;;) {
      const message = await this.nextMessage();
      if (message === null) {
        return null;
      }
      if (isObject(message) && message.type === "frame") {
        return message.frame ?? null;
      }
      if (isObject(message) && message.type === "error") {
        throw new Error(`visual session error: ${String(message.error)}`);
      }
      if (isObject(message) && message.type === "closed") {
        return null;
      }
    }
  }

  async close(): Promise<void> {
    if (this.closeSent) {
      return;
    }
    this.closeSent = true;
    this.socket.write(`${JSON.stringify({ id: randomUUID(), method: "visual.close", params: {} })}\n`);
    for (;;) {
      const message = await this.nextMessage();
      if (message === null || (isObject(message) && message.type === "closed")) {
        this.socket.end();
        return;
      }
    }
  }

  async cancel(): Promise<void> {
    this.socket.destroy();
    this.finish();
  }

  private receive(chunk: string): void {
    this.buffer += chunk;
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline < 0) {
        return;
      }
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (line.length > 0) {
        this.push(JSON.parse(line) as Json);
      }
    }
  }

  private push(message: Json): void {
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter(message);
    } else {
      this.messages.push(message);
    }
  }

  private finish(): void {
    if (this.ended) {
      return;
    }
    this.ended = true;
    for (const waiter of this.waiters.splice(0)) {
      waiter(null);
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

function connectUnix(path: string): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    socket.once("connect", () => resolve(socket));
    socket.once("error", reject);
  });
}

async function profileToken(profile: string, env: CuaEnv): Promise<string> {
  const token = env.CUA_HTTP_TOKEN?.trim();
  const override = env.CUA_DEV_HTTP_TOKEN_OVERRIDE;
  if (token && (override === "1" || override?.toLowerCase() === "true")) {
    return token;
  }
  const path = profileTokenPath(profile, env);
  try {
    const existing = (await readFile(path, "utf8")).trim();
    if (existing.length > 0) {
      return existing;
    }
  } catch {
    // Created below for parity with the Rust client.
  }
  await mkdir(join(cuaHome(env), "profiles", profile), { recursive: true });
  const created = `cua-${randomUUID()}`;
  await writeFile(path, `${created}\n`, "utf8");
  return created;
}

function profileSocketPath(profile: string, env: CuaEnv): string {
  return join(cuaHome(env), "profiles", profile, "daemon.sock");
}

function profileTokenPath(profile: string, env: CuaEnv): string {
  return join(cuaHome(env), "profiles", profile, "http.token");
}

function cuaHome(env: CuaEnv): string {
  return env.CUA_HOME && env.CUA_HOME.length > 0 ? env.CUA_HOME : join(homedir(), ".cua");
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

function inboundMessageBody(
  text: string,
  options: InboundMessageOptions,
  defaultSource: string,
): Json {
  return compact({
    schema_version: "cua.v1",
    idempotency_key: options.idempotencyKey ?? randomUUID(),
    source: options.source ?? defaultSource,
    text,
    payload: options.payload ?? {},
    reply_mode: options.replyUrl ? "webhook" : "ui",
    reply_url: options.replyUrl,
    ttl_ms: options.ttlMs,
  });
}

function rpcSession(options: DispatchOptions): RpcOptions {
  return { sessionId: sessionIdOf(options.session) };
}

function unixRequest(path: string, payload: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("connect", () => socket.write(`${payload}\n`));
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline >= 0) {
        const line = buffer.slice(0, newline);
        socket.end();
        resolve(line);
      }
    });
    socket.on("error", reject);
    socket.on("end", () => {
      if (buffer.trim().length === 0) {
        reject(new Error(`empty unix response from ${path}`));
      }
    });
  });
}

function isMissingUnixSocketError(error: unknown): boolean {
  return Boolean(
    error &&
      typeof error === "object" &&
      "code" in error &&
      ((error as { code?: string }).code === "ENOENT" || (error as { code?: string }).code === "ECONNREFUSED"),
  );
}

function isObject(value: Json): value is { [key: string]: Json } {
  return value !== null && typeof value === "object" && !Array.isArray(value);
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
