#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v aegis >/dev/null
command -v curl >/dev/null
command -v jq >/dev/null
command -v perl >/dev/null
command -v python3 >/dev/null
command -v sqlite3 >/dev/null
command -v xcrun >/dev/null

export SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
export BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:--isysroot $SDKROOT}"

RUN_ID="$(date +%s)"
SUFFIX="${RUN_ID: -5}"
PROFILE="${CUA_VOICE_LIVE_LONG_RANGE_PROFILE:-qll$SUFFIX}"
OUT_DIR="${CUA_VOICE_LIVE_LONG_RANGE_OUT_DIR:-artifacts/cua/voice-live-long-range-$RUN_ID}"
case "$OUT_DIR" in
  /*) ;;
  *) OUT_DIR="$ROOT/$OUT_DIR" ;;
esac
CUA_HOME_DIR="${CUA_VOICE_LIVE_LONG_RANGE_HOME:-}"
ENV_FILE="${CUA_ENV_FILE:-$HOME/.cua/config/env}"
PLANNER_MODEL="${CUA_VOICE_LIVE_LONG_RANGE_MODEL:-anthropic/claude-sonnet-4.6}"
BUDGET_MS="${CUA_VOICE_LIVE_LONG_RANGE_BUDGET_MS:-180000}"
WEB_ADDR="${CUA_VOICE_LIVE_LONG_RANGE_WEB_ADDR:-127.0.0.1:$((29500 + RUN_ID % 400))}"
CORPUS="$OUT_DIR/corpus"
WEB_DIR="$OUT_DIR/web"
TRACES="$OUT_DIR/traces"
EVENTS="$OUT_DIR/events"
PROOF="$OUT_DIR/proof.json"
AEGIS_PROFILE="cua-$PROFILE"

SCENARIOS=(
  cost-cutover
  incident-triage
  benchmark-analysis
  dependency-map
  contradiction-check
  config-audit
  failure-recovery
  timeout-recovery
  aegis-local-read
  aegis-link-follow
  release-risk-rank
  budget-sensitivity
  provider-cutover-readiness
  app-automation-route
  security-boundary
  telemetry-gap
  qgui-runtime-plan
  incident-remediation-plan
  aegis-cross-page-synthesis
  aegis-table-read
)

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "$ENV_FILE" 2>/dev/null && return 0
  return 1
}

env_key_value() {
  local name="$1"
  if [[ -n "${!name:-}" ]]; then
    printf '%s' "${!name}"
    return 0
  fi
  perl -MText::ParseWords=shellwords -ne '
    BEGIN { $name = shift @ARGV; }
    next unless /^\s*(?:export\s+)?\Q$name\E\s*=\s*(.+?)\s*$/;
    my @parts = shellwords($1);
    print $parts[0] // "";
    exit 0;
  ' "$name" "$ENV_FILE"
}

write_provider_credit_failure_proof() {
  local reason="$1"
  local total_credits="${2:-null}"
  local total_usage="${3:-null}"
  local remaining="${4:-null}"
  mkdir -p "$OUT_DIR"
  jq -n \
    --arg profile "$PROFILE" \
    --arg planner_model "$PLANNER_MODEL" \
    --arg reason "$reason" \
    --argjson total_credits "$total_credits" \
    --argjson total_usage "$total_usage" \
    --argjson remaining "$remaining" \
    --argjson scenario_count "${#SCENARIOS[@]}" \
    '{
      schema_version: "cua.voice_live_long_range_proof.v1",
      ok: false,
      provider_credit_failure: true,
      provider_credit_preflight: {
        ok: false,
        reason: $reason,
        total_credits: $total_credits,
        total_usage: $total_usage,
        remaining: $remaining
      },
      profile: $profile,
      planner_model: $planner_model,
      elapsed_ms: 0,
      budget_ms_per_turn: 0,
      scenario_count: $scenario_count,
      ok_count: 0,
      failed: [],
      results: []
    }' > "$PROOF"
}

require_openrouter_credit_preflight() {
  local min_remaining="${CUA_VOICE_LIVE_LONG_RANGE_MIN_OPENROUTER_CREDITS:-1}"
  local key response total usage remaining
  key="$(env_key_value OPENROUTER_API_KEY)"
  if [[ -z "$key" ]]; then
    write_provider_credit_failure_proof "OPENROUTER_API_KEY is required in the environment or $ENV_FILE"
    echo "OPENROUTER_API_KEY is required in the environment or $ENV_FILE" >&2
    exit 1
  fi
  response="$(curl -fsS https://openrouter.ai/api/v1/credits -H "Authorization: Bearer $key")" || {
    write_provider_credit_failure_proof "OpenRouter credits preflight request failed"
    echo "OpenRouter credits preflight request failed" >&2
    exit 1
  }
  total="$(jq -r '.data.total_credits // 0' <<<"$response")"
  usage="$(jq -r '.data.total_usage // 0' <<<"$response")"
  remaining="$(jq -r '.data.total_credits - .data.total_usage' <<<"$response")"
  if ! jq -n -e --argjson remaining "$remaining" --argjson min_remaining "$min_remaining" \
    '$remaining >= $min_remaining' >/dev/null
  then
    write_provider_credit_failure_proof \
      "OpenRouter credits below required minimum for live long-range proof" \
      "$total" \
      "$usage" \
      "$remaining"
    printf 'OpenRouter credits below required minimum for live long-range proof: remaining=%s required=%s\n' \
      "$remaining" \
      "$min_remaining" >&2
    exit 1
  fi
}

if ! env_key_available OPENROUTER_API_KEY; then
  echo "OPENROUTER_API_KEY is required in the environment or $ENV_FILE" >&2
  exit 1
fi
require_openrouter_credit_preflight

mkdir -p "$CORPUS" "$WEB_DIR" "$TRACES" "$EVENTS"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuall.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi

python3 - "$CORPUS" "$WEB_DIR" <<'PY'
import json
import pathlib
import sys

corpus = pathlib.Path(sys.argv[1])
web = pathlib.Path(sys.argv[2])

(corpus / "architecture.md").write_text("""# Computer Backend Architecture
The local backend is the default backend and uses the installed Mac attested route.
The cloud computer backend is provider-neutral above concrete VM providers.
The Oracle VM provider is named oracle-vm and owns OCI VM lifecycle.
QGUI belongs inside the oracle-vm guest image.
""", encoding="utf-8")

(corpus / "cost-model.md").write_text("""# Cost Model
Use rented cloud capacity while credits and early revenue prove demand.
Cut over to Quilt VM fleet when rented oracle-vm capacity costs more than self-hosted Quilt VM capacity.
""", encoding="utf-8")

(corpus / "incident.jsonl").write_text(
    "\n".join([
        json.dumps({"service": "planner", "level": "info", "latency_ms": 420, "message": "turn started"}),
        json.dumps({"service": "daemon", "level": "error", "latency_ms": 2180, "message": "socket reconnect failed"}),
        json.dumps({"service": "planner", "level": "warn", "latency_ms": 1290, "message": "retry after invalid json"}),
        json.dumps({"service": "daemon", "level": "error", "latency_ms": 2420, "message": "stale socket replaced"}),
    ]) + "\n",
    encoding="utf-8",
)

(corpus / "benchmarks.csv").write_text("""task,backend,attempts,success,seconds
research-small,local,2,true,4.2
research-small,oracle-vm,2,true,3.7
research-deep,local,7,true,21.5
research-deep,oracle-vm,5,true,13.2
broken-loop,local,480,false,42.0
""", encoding="utf-8")

(corpus / "dependencies.json").write_text(json.dumps({
    "computer_backend": ["local", "oracle-vm"],
    "oracle-vm": ["qgui", "oci-sdk", "vm-image"],
    "qgui": ["xvfb", "vnc", "wayland"],
    "agent_loop": ["planner", "dispatcher", "verification_observation", "trace_logger"],
}, indent=2), encoding="utf-8")

(corpus / "timeline.md").write_text("""2026-08-01 - local backend shipped
2026-08-12 - oracle-vm prototype booted
2026-08-18 - QGUI vendoring attached to VM image
2026-08-30 - long-range qualitative loop suite added
""", encoding="utf-8")

(corpus / "claims-a.md").write_text("Claim A: The default backend must be local. Claim B: oracle-vm is provider-specific below the generic cloud computer backend.\n", encoding="utf-8")
(corpus / "claims-b.md").write_text("Claim C: QGUI belongs inside oracle-vm images. Claim D: The planner must use OpenRouter credentials even when the model slug is google/gemini.\n", encoding="utf-8")
(corpus / "config.toml").write_text("""[voice]
planner_provider = "openrouter"
planner_model = "anthropic/claude-sonnet-4.6"
loop_budget = "n"

[backend]
default = "local"
cloud_provider = "oracle-vm"
""", encoding="utf-8")

(corpus / "release-risks.md").write_text("""# Release Risk Register
R1 | severity=high | area=agent-loop | Evidence-backed final answers can regress if action results are not reinserted into context.
R2 | severity=medium | area=hud | Background/headful label drift confuses the operator but does not break automation.
R3 | severity=high | area=daemon | Stale singleton sockets can strand HUD startup and cause persistent daemon errors.
R4 | severity=low | area=docs | Oracle VM naming needs consistent hyphenation.
""", encoding="utf-8")

(corpus / "budget-sensitivity.csv").write_text("""provider,cost_per_hour,usable_parallelism,self_host_threshold_hours
oracle-vm,0.032,64,900
aws,0.046,80,640
quilt-vm,0.018,128,0
local,0.000,1,0
""", encoding="utf-8")

(corpus / "cutover-readiness.json").write_text(json.dumps({
    "oracle-vm": {"credits_remaining": 1200, "qgui_image": "ready", "ssh_bootstrap": "ready", "autoscaler": "prototype"},
    "quilt-vm": {"kvm_runtime": "ready", "capacity": "limited", "billing": "missing", "autoscaler": "missing"},
    "decision": "keep oracle-vm for production until Quilt VM autoscaler and billing are ready"
}, indent=2), encoding="utf-8")

(corpus / "app-automation.md").write_text("""# App Automation Route
Headful app automation uses the local macOS backend and should label the target as macOS.
Headless/background automation uses the same agent loop but labels the target as Background.
Cloud app automation uses oracle-vm plus QGUI inside the guest image; the UI transport chip should show the app name when an app is being controlled.
""", encoding="utf-8")

(corpus / "security-boundary.md").write_text("""# Security Boundary
Never copy private OCI PEM keys into chat or model-visible context.
Cloud VM bootstrap may read credentials from the operator machine only through local filesystem/env handoff.
Production must not fall back to synthetic computer control when a backend is unavailable.
Action errors must propagate into the next planner turn with stderr, effect, and last attempted action.
""", encoding="utf-8")

(corpus / "telemetry.jsonl").write_text(
    "\n".join([
        json.dumps({"event": "planning_start", "present": True}),
        json.dumps({"event": "dispatch_result", "present": True}),
        json.dumps({"event": "agent_attempt_outcome", "present": True}),
        json.dumps({"event": "agent_reobserve_result", "present": True}),
        json.dumps({"event": "memory_persisted", "present": True}),
        json.dumps({"event": "gpu_frame_checksum", "present": False}),
    ]) + "\n",
    encoding="utf-8",
)

(corpus / "qgui-runtime-plan.md").write_text("""# QGUI Runtime Plan
The oracle-vm image must vendor QGUI locally.
The runtime stack is Xvfb for display, x11vnc for inspection, a window manager for app focus, and the cua-qgui-tool bridge for input/readback.
The same QGUI layer should later be reused by Quilt VM so cloud computer backends do not diverge.
""", encoding="utf-8")

(corpus / "remediation-runbook.md").write_text("""# Incident Remediation Runbook
Symptom: agent says progress was made but no action happened.
Step 1: inspect voice trace for dispatch_result and agent_attempt_outcome.
Step 2: verify action result evidence was included in the next planner request.
Step 3: reject final replies unless prior readback supports the answer token.
Step 4: if repeated visible actions produce no observed change, stop with contextual partial progress instead of looping forever.
""", encoding="utf-8")

web.joinpath("index.html").write_text("""<!doctype html><title>CUA Live Long Range Test</title>
<main>
<h1>CUA Live Long Range Test</h1>
<p>The local web fixture says the key answer is verification observation.</p>
<a href="/details.html">Architecture Details</a>
<a href="/ops.html">Operational Notes</a>
<a href="/matrix.html">Backend Matrix</a>
</main>""", encoding="utf-8")
web.joinpath("details.html").write_text("""<!doctype html><title>Architecture Details</title>
<main>
<h1>Architecture Details</h1>
<p>oracle-vm carries QGUI inside the VM image, while the upper layer remains a generic cloud computer backend.</p>
</main>""", encoding="utf-8")
web.joinpath("ops.html").write_text("""<!doctype html><title>Operational Notes</title>
<main>
<h1>Operational Notes</h1>
<p>Robust long-range automation requires dispatch evidence, reobserve evidence, and memory persistence before a final answer.</p>
<a href="/matrix.html">Backend Matrix</a>
</main>""", encoding="utf-8")
web.joinpath("matrix.html").write_text("""<!doctype html><title>Backend Matrix</title>
<main>
<h1>Backend Matrix</h1>
<table>
<tr><th>backend</th><th>surface</th><th>label</th></tr>
<tr><td>local</td><td>macOS app automation</td><td>macOS</td></tr>
<tr><td>oracle-vm</td><td>QGUI guest automation</td><td>Background</td></tr>
<tr><td>quilt-vm</td><td>future QGUI guest automation</td><td>Background</td></tr>
</table>
</main>""", encoding="utf-8")
PY

cargo build -p cua-voice

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  VOICE_BIN_PATH="target/debug/cua-voice"
else
  VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$VOICE_BIN_PATH" || ! -x "$VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

python3 -m http.server "${WEB_ADDR##*:}" --bind "${WEB_ADDR%:*}" --directory "$WEB_DIR" \
  >"$OUT_DIR/web.stdout" 2>"$OUT_DIR/web.stderr" &
WEB_PID="$!"

cleanup() {
  ps -ef \
    | grep 'aegis_cli --mode headless --profile' \
    | grep -F -- "--profile $AEGIS_PROFILE " \
    | awk '{print $2}' \
    | xargs kill >/dev/null 2>&1 || true
  kill "$WEB_PID" >/dev/null 2>&1 || true
  wait "$WEB_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  if curl -fs "http://$WEB_ADDR/index.html" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://$WEB_ADDR/index.html" >/dev/null

scenario_prompt() {
  local scenario="$1"
  local expected="$2"
  case "$scenario" in
    failure-recovery)
      printf 'Use shell_exec only. First try to read the missing file %s/missing-before-recovery.txt. After that failure is visible in your prior-attempt evidence, recover by writing and reading back the exact token `%s`. Your final reply must include exactly `%s`.' "$OUT_DIR" "$expected" "$expected"
      ;;
    timeout-recovery)
      printf 'Use shell_exec only. First run `sleep 2 && echo should-not-print` with timeout_ms 100. After that timeout is visible in your prior-attempt evidence, recover by printing the exact token `%s`. Your final reply must include exactly `%s`.' "$expected" "$expected"
      ;;
    aegis-local-read)
      printf 'Use Aegis headless only. Navigate to http://%s/index.html, read page text --scope main, verify the phrase "verification observation", then final reply must include exactly `%s`.' "$WEB_ADDR" "$expected"
      ;;
    aegis-link-follow)
      printf 'Use Aegis headless only. Navigate to http://%s/index.html, open the exact link "Architecture Details", read page text --scope main, verify QGUI/oracle-vm wording, then final reply must include exactly `%s`.' "$WEB_ADDR" "$expected"
      ;;
    aegis-cross-page-synthesis)
      printf 'Use Aegis headless only. Navigate to http://%s/index.html, read the main page, open "Operational Notes", read it, then open "Backend Matrix" and synthesize the required evidence. Final reply must include exactly `%s`.' "$WEB_ADDR" "$expected"
      ;;
    aegis-table-read)
      printf 'Use Aegis headless only. Navigate to http://%s/matrix.html, read the table, verify the oracle-vm row surface and label, then final reply must include exactly `%s`.' "$WEB_ADDR" "$expected"
      ;;
    *)
      printf 'Use shell_exec only to inspect files under %s for scenario `%s`. Do not finish from the task text alone. Run at least two separate investigation actions before the final reply: first map or inspect the relevant source files without printing the final token, then compute and read back the exact token `%s`. Only after that evidence is in prior-attempt context, final reply must include exactly `%s`.' "$CORPUS" "$scenario" "$expected" "$expected"
      ;;
  esac
}

expected_for() {
  case "$1" in
    architecture-summary) printf 'ANSWER[architecture-summary]=local default, oracle-vm provider, QGUI in VM image' ;;
    cost-cutover) printf 'ANSWER[cost-cutover]=cut over when Quilt VM self-hosting is cheaper than rented oracle-vm capacity' ;;
    incident-triage) printf 'ANSWER[incident-triage]=daemon has 2 errors and worst latency 2420ms' ;;
    benchmark-analysis) printf 'ANSWER[benchmark-analysis]=oracle-vm wins deep research by 8.3s and 2 attempts' ;;
    dependency-map) printf 'ANSWER[dependency-map]=oracle-vm depends on qgui, oci-sdk, vm-image' ;;
    timeline-order) printf 'ANSWER[timeline-order]=local backend -> oracle-vm prototype -> QGUI vendoring -> qualitative loop suite' ;;
    contradiction-check) printf 'ANSWER[contradiction-check]=no contradiction: OpenRouter credentials stay required for google/gemini model slugs' ;;
    config-audit) printf 'ANSWER[config-audit]=openrouter planner, sonnet model, unbounded loop, local default backend' ;;
    failure-recovery) printf 'ANSWER[failure-recovery]=recovered after missing-file failure' ;;
    timeout-recovery) printf 'ANSWER[timeout-recovery]=recovered after timeout with bounded command' ;;
    aegis-local-read) printf 'ANSWER[aegis-local-read]=verification observation' ;;
    aegis-link-follow) printf 'ANSWER[aegis-link-follow]=oracle-vm carries QGUI inside the VM image' ;;
    release-risk-rank) printf 'ANSWER[release-risk-rank]=top risks are agent-loop evidence regression and stale singleton daemon startup' ;;
    budget-sensitivity) printf 'ANSWER[budget-sensitivity]=oracle-vm stays rented until 900 self-host threshold hours; quilt-vm is lowest cost at 0.018' ;;
    provider-cutover-readiness) printf 'ANSWER[provider-cutover-readiness]=keep oracle-vm until Quilt VM autoscaler and billing are ready' ;;
    app-automation-route) printf 'ANSWER[app-automation-route]=headful local apps label macOS, headless automation labels Background, cloud apps use oracle-vm plus QGUI' ;;
    security-boundary) printf 'ANSWER[security-boundary]=no PEM in chat, no synthetic fallback, propagate action errors into planner context' ;;
    telemetry-gap) printf 'ANSWER[telemetry-gap]=required events are present; gpu_frame_checksum is the only missing optional signal' ;;
    qgui-runtime-plan) printf 'ANSWER[qgui-runtime-plan]=vendor QGUI in oracle-vm with Xvfb, x11vnc, window manager, and cua-qgui-tool' ;;
    incident-remediation-plan) printf 'ANSWER[incident-remediation-plan]=trace dispatch_result, feed action evidence forward, require readback, stop no-progress loops contextually' ;;
    aegis-cross-page-synthesis) printf 'ANSWER[aegis-cross-page-synthesis]=dispatch evidence, reobserve evidence, and memory persistence before a final answer' ;;
    aegis-table-read) printf 'ANSWER[aegis-table-read]=oracle-vm QGUI guest automation Background' ;;
    *) return 1 ;;
  esac
}

min_dispatches_for() {
  case "$1" in
    failure-recovery|timeout-recovery) printf '2' ;;
    aegis-local-read) printf '2' ;;
    aegis-link-follow|aegis-cross-page-synthesis) printf '3' ;;
    aegis-table-read) printf '2' ;;
    *) printf '2' ;;
  esac
}

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
for scenario in "${SCENARIOS[@]}"; do
  expected="$(expected_for "$scenario")"
  transcript="$(scenario_prompt "$scenario" "$expected")"
  set +e
  CUA_HOME="$CUA_HOME_DIR" \
  CUA_ENV_FILE="$ENV_FILE" \
  CUA_VOICE_DEBUG_TRACE=true \
  CUA_VOICE_TRACE_PATH="$TRACES/$scenario.jsonl" \
  CUA_AGENT_LOOP_MAX_ATTEMPTS="n" \
  "$VOICE_BIN_PATH" \
    --profile "$PROFILE" \
    --headless \
    --planner-model "$PLANNER_MODEL" \
    --once-agent-reply-wait-ms "$BUDGET_MS" \
    --once-transcript "$transcript" \
    >"$EVENTS/$scenario.jsonl" 2>"$EVENTS/$scenario.stderr"
  status=$?
  set -e
  printf '{"scenario":%s,"exit_code":%s,"expected":%s,"min_dispatches":%s}\n' \
    "$(jq -Rn --arg value "$scenario" '$value')" \
    "$status" \
    "$(jq -Rn --arg value "$expected" '$value')" \
    "$(jq -Rn --argjson value "$(min_dispatches_for "$scenario")" '$value')" >> "$OUT_DIR/exits.jsonl"
  if rg -q "402 Payment Required|insufficient provider credits|provider returned payment required" "$TRACES/$scenario.jsonl" "$EVENTS/$scenario.jsonl" "$EVENTS/$scenario.stderr" 2>/dev/null; then
    break
  fi
done
END_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
ELAPSED_MS="$((END_MS - START_MS))"

python3 - "$OUT_DIR" "$PROOF" "$PROFILE" "$PLANNER_MODEL" "$ELAPSED_MS" "$BUDGET_MS" "${SCENARIOS[@]}" <<'PY'
import json
import pathlib
import sys

out_dir = pathlib.Path(sys.argv[1])
proof_path = pathlib.Path(sys.argv[2])
profile = sys.argv[3]
planner_model = sys.argv[4]
elapsed_ms = int(sys.argv[5])
budget_ms = int(sys.argv[6])
scenarios = sys.argv[7:]
exits = {row["scenario"]: row for row in map(json.loads, (out_dir / "exits.jsonl").read_text().splitlines())}
results = []
for scenario in scenarios:
    if scenario not in exits:
        results.append({
            "scenario": scenario,
            "ok": False,
            "exit_code": None,
            "expected": None,
            "min_dispatches": None,
            "dispatch_count": 0,
            "planner_requests": 0,
            "mini_turn_requests": 0,
            "final_effect": None,
            "reply": "not run because an earlier live provider call failed",
            "trace_path": str(out_dir / "traces" / f"{scenario}.jsonl"),
            "events_path": str(out_dir / "events" / f"{scenario}.jsonl"),
            "stderr_path": str(out_dir / "events" / f"{scenario}.stderr"),
        })
        continue
    trace_path = out_dir / "traces" / f"{scenario}.jsonl"
    events_path = out_dir / "events" / f"{scenario}.jsonl"
    stderr_path = out_dir / "events" / f"{scenario}.stderr"
    expected = exits[scenario]["expected"]
    trace = []
    if trace_path.exists():
        trace = [json.loads(line) for line in trace_path.read_text().splitlines() if line.strip()]
    events = []
    if events_path.exists():
        events = [json.loads(line) for line in events_path.read_text().splitlines() if line.strip()]
    reply = next((event.get("data", {}).get("text", "") for event in reversed(trace) if event.get("event") == "reply"), "")
    if not reply:
        reply = next((event.get("text", "") for event in reversed(events) if event.get("event") == "reply"), "")
    stop = next((event.get("data", {}) for event in reversed(trace) if event.get("event") == "agent_loop_stop"), {})
    planner_requests = sum(1 for event in trace if event.get("event") == "planning_result")
    attempt_outcomes = [event.get("data", {}) for event in trace if event.get("event") == "agent_attempt_outcome"]
    mini_turns = sum(1 for outcome in attempt_outcomes if outcome.get("has_action") and outcome.get("should_replan"))
    dispatch_results = [event.get("data", {}).get("result", {}) for event in trace if event.get("event") == "dispatch_result"]
    dispatch_count = len(dispatch_results)
    min_dispatches = int(exits[scenario].get("min_dispatches", 1))
    dispatch_evidence = json.dumps(dispatch_results, sort_keys=True)
    has_memory = any(event.get("event") == "memory_persisted" for event in trace)
    reply_looks_evidence_free = "path is not present" in reply.lower() or "directory not found" in reply.lower() or "task contract specifies" in reply.lower()
    has_positive_evidence = expected in dispatch_evidence or scenario in {
        "failure-recovery",
        "timeout-recovery",
        "aegis-local-read",
        "aegis-link-follow",
        "aegis-cross-page-synthesis",
        "aegis-table-read",
    }
    is_recovery = scenario in {"failure-recovery", "timeout-recovery"}
    required_planner_requests = min_dispatches if is_recovery else min_dispatches + 1
    required_mini_turns = max(1, min_dispatches - 1) if is_recovery else min_dispatches
    ok = (
        exits[scenario]["exit_code"] == 0
        and expected in reply
        and stop.get("final_effect") == "confirmed"
        and planner_requests >= required_planner_requests
        and mini_turns >= required_mini_turns
        and dispatch_count >= min_dispatches
        and has_positive_evidence
        and not reply_looks_evidence_free
        and has_memory
    )
    results.append({
        "scenario": scenario,
        "ok": ok,
        "exit_code": exits[scenario]["exit_code"],
        "expected": expected,
        "min_dispatches": min_dispatches,
        "dispatch_count": dispatch_count,
        "planner_requests": planner_requests,
        "mini_turn_requests": mini_turns,
        "final_effect": stop.get("final_effect"),
        "reply": reply,
        "trace_path": str(trace_path),
        "events_path": str(events_path),
        "stderr_path": str(stderr_path),
    })

proof = {
    "schema_version": "cua.voice_live_long_range_proof.v1",
    "ok": all(result["ok"] for result in results) and len(results) >= 10 and elapsed_ms <= budget_ms * len(results),
    "provider_credit_failure": any(
        "insufficient provider credits" in result.get("reply", "").lower()
        or "payment required" in result.get("reply", "").lower()
        for result in results
    ),
    "profile": profile,
    "planner_model": planner_model,
    "elapsed_ms": elapsed_ms,
    "budget_ms_per_turn": budget_ms,
    "scenario_count": len(results),
    "ok_count": sum(1 for result in results if result["ok"]),
    "failed": [result for result in results if not result["ok"]],
    "results": results,
}
proof_path.write_text(json.dumps(proof, indent=2), encoding="utf-8")
if not proof["ok"]:
    raise SystemExit(1)
PY

jq -e '.ok == true and .scenario_count >= 10 and .ok_count == .scenario_count' "$PROOF" >/dev/null
echo "$OUT_DIR"
