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
      ...tomlFields(params),
      options.sessionId ? `session_id = ${tomlString(options.sessionId)}` : "",
    ]
      .filter(Boolean)
      .join("\n");
    const report = await this.runInline(runebook);
    return report;
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

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function tomlFields(value: Json): string[] {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return [`value = ${tomlJson(value)}`];
  }
  return Object.entries(value).map(([key, field]) => `${key} = ${tomlJson(field)}`);
}

function tomlJson(value: Json): string {
  if (typeof value === "string") {
    return tomlString(value);
  }
  return JSON.stringify(value);
}
