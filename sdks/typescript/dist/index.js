import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
export class Cua {
    profile;
    bin;
    env;
    constructor(options = {}) {
        this.profile = options.profile ?? "default";
        this.bin = options.bin ?? "cua";
        this.env = { ...process.env, ...options.env };
    }
    static async connect(options = {}) {
        return new Cua(options);
    }
    async run(file, options = {}) {
        const args = ["--profile", this.profile, "run", file, "--json"];
        if (options.traceDir) {
            args.push("--trace-dir", options.traceDir);
        }
        return this.execJson(args);
    }
    async runInline(runebookToml, options = {}) {
        const dir = await mkdtemp(join(tmpdir(), "cua-runebook-"));
        const file = join(dir, `${randomUUID()}.cua.toml`);
        try {
            await writeFile(file, runebookToml, "utf8");
            return await this.run(file, options);
        }
        finally {
            await rm(dir, { force: true, recursive: true });
        }
    }
    async runSteps(steps, options = {}) {
        const runebook = renderRunebook({
            profile: this.profile,
            name: options.name ?? "typescript-sdk",
            trace: options.trace ?? false,
            onError: options.onError,
            steps,
        });
        return this.runInline(runebook, options);
    }
    async rpc(method, params = {}, options = {}) {
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
    async manifest() {
        return this.step("manifest", {}, "manifest");
    }
    async schemas() {
        return this.step("schemas", {}, "schemas");
    }
    async metrics() {
        return this.step("metrics", {}, "metrics");
    }
    async status() {
        return this.execJson(["--profile", this.profile, "status", "--json"]);
    }
    async configStatus() {
        return this.execJson(["--profile", this.profile, "config", "status", "--json"]);
    }
    async acquireOwner(clientName = "typescript sdk", ttlMs) {
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
    async cancelSession(session, targetSessionId) {
        return this.rpc("session.cancel", compact({
            schema_version: "cua.v1",
            session_id: sessionIdOf(session),
            target_session_id: targetSessionId,
        }));
    }
    async sessionStatus() {
        return this.rpc("session.status");
    }
    async profileStatus() {
        return this.step("profile.status", {}, "profile");
    }
    async createProfile(options, session) {
        if (session) {
            return this.rpc("profile.create", compact({
                name: options.name,
                mode: options.mode ?? "supervised",
                duration_ms: options.durationMs,
                capabilities: options.capabilities,
            }), { sessionId: sessionIdOf(session) });
        }
        return this.step("profile.create", {
            name: options.name,
            mode: options.mode ?? "supervised",
            duration_ms: options.durationMs,
            capabilities: options.capabilities,
        }, "profile");
    }
    async activateProfile(session) {
        if (session) {
            return this.rpc("profile.activate", {}, { sessionId: sessionIdOf(session) });
        }
        return this.step("profile.activate", {}, "profile");
    }
    async requestAccessibility() {
        return this.step("permissions.request_accessibility", {}, "permissions");
    }
    async observe() {
        return this.step("observe", {}, "desktop");
    }
    async screenshot(options = {}) {
        return this.step("screenshot", {
            max_width: options.maxWidth,
            encoding: options.encoding,
            force_fresh: options.forceFresh,
            include_bytes: options.includeBytes,
        }, "screenshot");
    }
    async windowCapture(options) {
        return this.step("window.capture", {
            window_id: options.windowId,
            max_width: options.maxWidth,
            encoding: options.encoding,
            include_bytes: options.includeBytes,
        }, "window");
    }
    async context(options = {}) {
        return this.step("context", {
            max_width: options.maxWidth,
            encoding: options.encoding,
            force_fresh: options.forceFresh,
            include_bytes: options.includeBytes,
        }, "context");
    }
    async events(options = {}) {
        return this.step("events", {
            after: options.after,
            timeout_ms: options.timeoutMs,
        }, "events");
    }
    async uiStep(options) {
        return this.step("ui.step", {
            label: options.label,
            source: options.source,
            task: options.task,
            tool: options.tool,
            step_index: options.stepIndex,
            step_total: options.stepTotal,
            ttl_ms: options.ttlMs,
        }, "ui");
    }
    async uiIsland(state, source) {
        return this.step("ui.island", { state, source }, "ui");
    }
    async uiReply(options) {
        return this.step("ui.reply", { text: options.text, source: options.source, ttl_ms: options.ttlMs }, "ui");
    }
    async uiMode(mode, source) {
        return this.step("ui.mode", { mode, source }, "ui");
    }
    async clipboardRead(allowSensitive = false) {
        return this.step("clipboard.read", { allow_sensitive: allowSensitive }, "clipboard");
    }
    async clipboardWrite(text, session) {
        return this.rpc("clipboard.write", { schema_version: "cua.v1", text }, session ? { sessionId: sessionIdOf(session) } : {});
    }
    async pause(session) {
        return this.rpc("control.pause", {}, { sessionId: sessionIdOf(session) });
    }
    async resume(session) {
        return this.rpc("control.resume", {}, { sessionId: sessionIdOf(session) });
    }
    async killSwitch(session) {
        return this.rpc("control.kill_switch", {}, { sessionId: sessionIdOf(session) });
    }
    async dispatch(action, options = {}) {
        return this.rpc("input.dispatch", action, rpcSession(options));
    }
    async dispatchFrame(options) {
        return this.rpc("input.dispatch_frame", {
            schema_version: "cua.v1",
            source_frame: options.sourceFrame,
            action: options.action,
        }, rpcSession(options));
    }
    async visualSession(options = {}) {
        const socketPath = profileSocketPath(this.profile, this.env);
        const token = await profileToken(this.profile, this.env);
        const socket = await connectUnix(socketPath);
        const session = new CuaVisualSession(socket);
        socket.write(`${JSON.stringify({
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
        })}\n`);
        return session;
    }
    async openApp(app, options = {}) {
        return this.dispatch({ schema_version: "cua.v1", action: "open_app", app }, options);
    }
    async shell(command, options = {}) {
        return this.dispatch({ schema_version: "cua.v1", action: "shell", command }, options);
    }
    async aegis(args, options = {}) {
        return this.dispatch({ schema_version: "cua.v1", action: "aegis", args }, options);
    }
    async ctx(args, options = {}) {
        return this.dispatch({ schema_version: "cua.v1", action: "ctx", args }, options);
    }
    async step(action, fields, saveAs) {
        const report = await this.runSteps([{ id: saveAs, do: action, save_as: saveAs, ...fields }]);
        return readResult(report, saveAs);
    }
    async execJson(args) {
        const result = await execFile(this.bin, args, this.env);
        if (result.code !== 0) {
            throw new Error(result.stderr.trim() || result.stdout.trim() || `cua exited ${result.code}`);
        }
        try {
            return JSON.parse(result.stdout);
        }
        catch (error) {
            throw new Error(`cua returned non-JSON output: ${error.message}\n${result.stdout}`);
        }
    }
}
export class CuaVisualSession {
    socket;
    buffer = "";
    messages = [];
    waiters = [];
    ended = false;
    closeSent = false;
    constructor(socket) {
        this.socket = socket;
        socket.setEncoding("utf8");
        socket.on("data", (chunk) => this.receive(chunk));
        socket.on("end", () => this.finish());
        socket.on("close", () => this.finish());
    }
    async nextMessage() {
        const message = this.messages.shift();
        if (message !== undefined) {
            return message;
        }
        if (this.ended) {
            return null;
        }
        return new Promise((resolve) => this.waiters.push(resolve));
    }
    async nextFrame() {
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
    async close() {
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
    async cancel() {
        this.socket.destroy();
        this.finish();
    }
    receive(chunk) {
        this.buffer += chunk;
        for (;;) {
            const newline = this.buffer.indexOf("\n");
            if (newline < 0) {
                return;
            }
            const line = this.buffer.slice(0, newline).trim();
            this.buffer = this.buffer.slice(newline + 1);
            if (line.length > 0) {
                this.push(JSON.parse(line));
            }
        }
    }
    push(message) {
        const waiter = this.waiters.shift();
        if (waiter) {
            waiter(message);
        }
        else {
            this.messages.push(message);
        }
    }
    finish() {
        if (this.ended) {
            return;
        }
        this.ended = true;
        for (const waiter of this.waiters.splice(0)) {
            waiter(null);
        }
    }
}
function renderRunebook(input) {
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
function execFile(bin, args, env) {
    return new Promise((resolve, reject) => {
        const child = spawn(bin, args, { env });
        let stdout = "";
        let stderr = "";
        child.stdout.setEncoding("utf8");
        child.stderr.setEncoding("utf8");
        child.stdout.on("data", (chunk) => {
            stdout += chunk;
        });
        child.stderr.on("data", (chunk) => {
            stderr += chunk;
        });
        child.on("error", reject);
        child.on("close", (code) => resolve({ code: code ?? 1, stdout, stderr }));
    });
}
function connectUnix(path) {
    return new Promise((resolve, reject) => {
        const socket = createConnection(path);
        socket.once("connect", () => resolve(socket));
        socket.once("error", reject);
    });
}
async function profileToken(profile, env) {
    const token = env.CUA_HTTP_TOKEN;
    if (token && token.trim().length > 0) {
        return token;
    }
    const path = profileTokenPath(profile, env);
    try {
        const existing = (await readFile(path, "utf8")).trim();
        if (existing.length > 0) {
            return existing;
        }
    }
    catch {
        // Created below for parity with the Rust client.
    }
    await mkdir(join(cuaHome(env), "profiles", profile), { recursive: true });
    const created = `cua-${randomUUID()}`;
    await writeFile(path, `${created}\n`, "utf8");
    return created;
}
function profileSocketPath(profile, env) {
    return join(cuaHome(env), "profiles", profile, "daemon.sock");
}
function profileTokenPath(profile, env) {
    return join(cuaHome(env), "profiles", profile, "http.token");
}
function cuaHome(env) {
    return env.CUA_HOME && env.CUA_HOME.length > 0 ? env.CUA_HOME : join(homedir(), ".cua");
}
function readSessionId(value) {
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
function readResult(value, key) {
    if (value && typeof value === "object" && !Array.isArray(value)) {
        const results = value.results;
        if (results && typeof results === "object" && !Array.isArray(results) && key in results) {
            return results[key];
        }
    }
    throw new Error(`cua runebook response did not include results.${key}`);
}
function sessionIdOf(session) {
    return typeof session === "string" ? session : session.sessionId;
}
function rpcSession(options) {
    return options.session ? { sessionId: sessionIdOf(options.session) } : {};
}
function isObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}
function compact(value) {
    return Object.fromEntries(Object.entries(value).filter(([, field]) => field !== undefined && field !== null));
}
function tomlString(value) {
    return JSON.stringify(value);
}
function tomlKey(key) {
    return /^[A-Za-z_][A-Za-z0-9_-]*$/.test(key) ? key : tomlString(key);
}
function tomlValue(value) {
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
