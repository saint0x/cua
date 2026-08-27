import type { APIRoute } from "astro";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";

export const prerender = false;

const DEFAULT_CUA_VOICE_BIN = "/Users/deepsaint/Applications/cua.app/Contents/MacOS/cua-voice";
const DEFAULT_STT_BACKEND = "local";
const DEFAULT_STT_MODEL = "tiny.en";
const DEFAULT_PLANNER_MODEL = "google/gemini-2.5-flash-lite";

export const POST: APIRoute = async ({ request, url }) => {
  const startedAt = performance.now();
  const bytes = Buffer.from(await request.arrayBuffer());
  if (bytes.length < 44) {
    return json({ ok: false, error: "empty or invalid WAV upload" }, 400);
  }

  const tempRoot = await mkdtemp(join(tmpdir(), "cua-mic-lab-"));
  const wavPath = join(tempRoot, `run-${randomUUID()}.wav`);
  await writeFile(wavPath, bytes);

  try {
    const result = await runCuaVoice(wavPath, {
      sttBackend: url.searchParams.get("stt_backend") || undefined,
      sttModel: url.searchParams.get("stt_model") || undefined,
      plannerModel: url.searchParams.get("planner_model") || undefined,
      debugTrace: url.searchParams.get("debug_trace") === "1",
    });
    return json({
      ok: result.exitCode === 0,
      durationMs: Math.round(performance.now() - startedAt),
      wavBytes: bytes.length,
      ...result,
    });
  } finally {
    await rm(tempRoot, { force: true, recursive: true });
  }
};

function runCuaVoice(
  wavPath: string,
  options: {
    sttBackend?: string;
    sttModel?: string;
    plannerModel?: string;
    debugTrace?: boolean;
  }
): Promise<{
  command: string[];
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  events: unknown[];
  summary: Record<string, unknown>;
}> {
  const bin = process.env.CUA_VOICE_BIN || DEFAULT_CUA_VOICE_BIN;
  const args = [
    "--headless",
    "--profile",
    process.env.CUA_PROFILE || "default",
    "--stt-backend",
    options.sttBackend || process.env.CUA_STT_BACKEND || DEFAULT_STT_BACKEND,
    "--stt-model",
    options.sttModel || process.env.CUA_STT_MODEL || DEFAULT_STT_MODEL,
    "--planner-model",
    options.plannerModel || process.env.CUA_PLANNER_MODEL || DEFAULT_PLANNER_MODEL,
    "--once-wav",
    wavPath,
  ];
  if (options.debugTrace) args.unshift("--debug-trace");

  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, {
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => {
      child.kill("SIGTERM");
    }, Number(process.env.CUA_MIC_LAB_TIMEOUT_MS || 90_000));

    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.on("close", (exitCode, signal) => {
      clearTimeout(timeout);
      const events = parseJsonLines(stdout);
      resolve({
        command: [bin, ...args],
        exitCode,
        signal,
        stdout,
        stderr,
        events,
        summary: summarizeEvents(events),
      });
    });
  });
}

function parseJsonLines(stdout: string): unknown[] {
  return stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line);
      } catch {
        return { event: "raw", text: line };
      }
    });
}

function summarizeEvents(events: unknown[]): Record<string, unknown> {
  const summary: Record<string, unknown> = {
    transcript: null,
    reply: null,
    error: null,
    stt: null,
    audio: null,
    metrics: {},
    eventCount: events.length,
  };

  for (const event of events) {
    if (!event || typeof event !== "object") continue;
    const entry = event as Record<string, unknown>;
    if (entry.event === "transcript") summary.transcript = entry.text ?? null;
    if (entry.event === "reply") summary.reply = entry.text ?? null;
    if (entry.event === "error") summary.error = entry.text ?? null;
    if (entry.event === "stt_diagnostic") summary.stt = entry;
    if (entry.event === "audio_diagnostic") summary.audio = entry;
    if (entry.event === "metric" && typeof entry.name === "string") {
      (summary.metrics as Record<string, unknown>)[entry.name] = entry.ms ?? entry.value ?? entry;
    }
  }

  return summary;
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value, null, 2), {
    status,
    headers: {
      "content-type": "application/json",
    },
  });
}
