#!/usr/bin/env node
/**
 * ACP billing spike — one-shot Cursor agent call via `agent acp`.
 *
 * Purpose: verify that an ACP-launched Auto model run bills against your
 * Cursor subscription (same pools as IDE/CLI print mode), not a separate meter.
 *
 * Prerequisites:
 *   - Node.js 18+
 *   - Cursor Agent CLI installed (`agent` or `%LOCALAPPDATA%\\cursor-agent\\agent.cmd`)
 *   - Authenticated: run `agent login` once, or set CURSOR_API_KEY
 *
 * Usage:
 *   node run.mjs
 *   AGENT_BIN=/path/to/agent node run.mjs
 *   CURSOR_API_KEY=cursor_... node run.mjs
 *
 * After it finishes, check Cursor Dashboard → Usage for an entry tagged around
 * the printed startedAt timestamp. Expect plan-included / Auto pool usage.
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join } from "node:path";
import readline from "node:readline";

const PROMPT =
  process.env.SPIKE_PROMPT ??
  "Reply with exactly one line: billing-spike-ok. Do not use tools.";
const MODEL_ID = process.env.SPIKE_MODEL ?? "auto";
const CWD = process.env.SPIKE_CWD ?? process.cwd();
const AUTH_TIMEOUT_MS = Number(process.env.SPIKE_AUTH_TIMEOUT_MS ?? 120_000);
const PROMPT_TIMEOUT_MS = Number(process.env.SPIKE_PROMPT_TIMEOUT_MS ?? 300_000);

function resolveAgentBin() {
  if (process.env.AGENT_BIN) return process.env.AGENT_BIN;

  const candidates = [];
  if (platform() === "win32") {
    const localAppData = process.env.LOCALAPPDATA;
    if (localAppData) {
      candidates.push(join(localAppData, "cursor-agent", "agent.cmd"));
      candidates.push(join(localAppData, "cursor-agent", "cursor-agent.cmd"));
    }
  } else {
    candidates.push(join(homedir(), ".local", "bin", "agent"));
  }
  candidates.push("agent");

  for (const candidate of candidates) {
    if (candidate === "agent" || existsSync(candidate)) return candidate;
  }
  throw new Error(
    "Cursor agent CLI not found. Install from https://cursor.com/install or set AGENT_BIN.",
  );
}

function spawnAgent(agentBin) {
  const args = ["acp"];
  const env = { ...process.env };

  if (agentBin.endsWith(".cmd") || agentBin.endsWith(".bat")) {
    return spawn(agentBin, args, {
      stdio: ["pipe", "pipe", "inherit"],
      env,
      shell: true,
    });
  }

  if (agentBin.endsWith(".ps1")) {
    return spawn(
      "powershell.exe",
      ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", agentBin, ...args],
      { stdio: ["pipe", "pipe", "inherit"], env },
    );
  }

  return spawn(agentBin, args, { stdio: ["pipe", "pipe", "inherit"], env });
}

function withTimeout(promise, ms, label) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${label} timed out after ${ms}ms`)),
      ms,
    );
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function pickModelOption(configOptions) {
  if (!Array.isArray(configOptions)) return null;
  return (
    configOptions.find((o) => o.category === "model") ??
    configOptions.find((o) => o.id === "model") ??
    configOptions.find((o) => /model/i.test(o.id ?? ""))
  );
}

function pickModeOption(configOptions) {
  if (!Array.isArray(configOptions)) return null;
  return (
    configOptions.find((o) => o.category === "mode") ??
    configOptions.find((o) => o.id === "mode")
  );
}

function hasModelValue(modelOption, modelId) {
  if (!modelOption?.options) return false;
  return modelOption.options.some((o) => o.value === modelId);
}

async function main() {
  const startedAt = new Date().toISOString();
  const agentBin = resolveAgentBin();
  console.error(`[spike] agent: ${agentBin}`);
  console.error(`[spike] cwd: ${CWD}`);
  console.error(`[spike] model: ${MODEL_ID}`);
  console.error(`[spike] startedAt: ${startedAt}`);
  console.error(`[spike] prompt: ${JSON.stringify(PROMPT)}`);
  console.error("");

  const agent = spawnAgent(agentBin);
  let nextId = 1;
  const pending = new Map();
  let sessionId = null;
  let assistantText = "";

  function send(method, params) {
    const id = nextId++;
    agent.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n",
    );
    return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  }

  function respond(id, result) {
    agent.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
  }

  const rl = readline.createInterface({ input: agent.stdout });
  rl.on("line", (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      console.error("[spike] non-json stdout:", line);
      return;
    }

    if (msg.id && (msg.result || msg.error)) {
      const waiter = pending.get(msg.id);
      if (!waiter) return;
      pending.delete(msg.id);
      msg.error ? waiter.reject(msg.error) : waiter.resolve(msg.result);
      return;
    }

    if (msg.method === "session/update") {
      const update = msg.params?.update;
      const kind = update?.sessionUpdate;

      if (kind === "agent_message_chunk" && update.content?.text) {
        assistantText += update.content.text;
        process.stdout.write(update.content.text);
      } else if (kind === "config_option_update") {
        console.error("\n[spike] config_option_update:", JSON.stringify(update.configOptions));
      } else if (kind?.includes("usage") || update?.usage || update?.cost) {
        console.error("\n[spike] usage update:", JSON.stringify(update));
      } else {
        console.error("\n[spike] session/update:", JSON.stringify(update));
      }
      return;
    }

    if (msg.method === "session/request_permission") {
      console.error("\n[spike] auto-allow permission:", msg.params?.toolCall?.title ?? "(tool)");
      respond(msg.id, {
        outcome: { outcome: "selected", optionId: "allow-once" },
      });
      return;
    }

    if (msg.method === "cursor/ask_question") {
      console.error("\n[spike] auto-skip cursor/ask_question");
      respond(msg.id, { outcome: { outcome: "skipped", reason: "billing spike" } });
      return;
    }

    if (msg.method === "cursor/create_plan") {
      console.error("\n[spike] auto-accept cursor/create_plan");
      respond(msg.id, { outcome: { outcome: "accepted" } });
      return;
    }

    if (msg.method === "cursor/update_todos" || msg.method === "cursor/task" || msg.method === "cursor/generate_image") {
      console.error(`\n[spike] notification ${msg.method}`);
      return;
    }

    if (msg.id) {
      console.error(`\n[spike] unhandled request ${msg.method ?? "(unknown)"}:`, JSON.stringify(msg.params));
      respond(msg.id, {});
    }
  });

  agent.on("exit", (code, signal) => {
    if (code !== 0 && code !== null) {
      console.error(`\n[spike] agent exited code=${code} signal=${signal ?? ""}`);
    }
  });

  try {
    const init = await send("initialize", {
      protocolVersion: 1,
      clientCapabilities: {
        fs: { readTextFile: false, writeTextFile: false },
        terminal: false,
      },
      clientInfo: { name: "acp-auto-billing-spike", version: "0.1.0" },
    });
    console.error("[spike] initialize ok");

    await withTimeout(
      send("authenticate", { methodId: "cursor_login" }),
      AUTH_TIMEOUT_MS,
      "authenticate (run `agent login` or set CURSOR_API_KEY if this hangs)",
    );
    console.error("[spike] authenticate ok");

    const session = await send("session/new", { cwd: CWD, mcpServers: [] });
    sessionId = session.sessionId;
    console.error(`[spike] session/new ok sessionId=${sessionId}`);

    const configOptions = session.configOptions ?? [];
    if (configOptions.length) {
      console.error("[spike] configOptions:", JSON.stringify(configOptions, null, 2));
    }

    const modelOption = pickModelOption(configOptions);
    if (modelOption) {
      const target = hasModelValue(modelOption, MODEL_ID)
        ? MODEL_ID
        : modelOption.currentValue;
      if (target !== modelOption.currentValue) {
        const updated = await send("session/set_config_option", {
          sessionId,
          configId: modelOption.id,
          value: target,
        });
        console.error(`[spike] model set to ${target}`);
        if (updated.configOptions) {
          const current = pickModelOption(updated.configOptions);
          console.error(`[spike] current model: ${current?.currentValue ?? "(unknown)"}`);
        }
      } else {
        console.error(`[spike] model already ${target}`);
      }
    } else {
      console.error("[spike] no model config option returned; relying on agent default");
    }

    const modeOption = pickModeOption(configOptions);
    if (modeOption) {
      const agentMode =
        modeOption.options?.find((o) => o.value === "agent")?.value ??
        modeOption.currentValue;
      if (agentMode !== modeOption.currentValue) {
        await send("session/set_config_option", {
          sessionId,
          configId: modeOption.id,
          value: agentMode,
        });
        console.error(`[spike] mode set to ${agentMode}`);
      }
    }

    console.error("\n[spike] --- assistant output ---");
    const result = await withTimeout(
      send("session/prompt", {
        sessionId,
        prompt: [{ type: "text", text: PROMPT }],
      }),
      PROMPT_TIMEOUT_MS,
      "session/prompt",
    );
    console.error("\n[spike] --- end output ---");
    console.error(`[spike] stopReason=${result.stopReason}`);

    const finishedAt = new Date().toISOString();
    console.error("");
    console.error("=== billing spike complete ===");
    console.error(`startedAt:  ${startedAt}`);
    console.error(`finishedAt: ${finishedAt}`);
    console.error(`sessionId:  ${sessionId}`);
    console.error(`stopReason: ${result.stopReason}`);
    console.error(`response:   ${assistantText.trim().slice(0, 200)}`);
    console.error("");
    console.error("Next: Cursor Dashboard → Usage. Look for activity between the timestamps above.");
    console.error("Expect subscription / Auto pool billing (not a separate on-demand-only meter).");

    if (!assistantText.includes("billing-spike-ok")) {
      console.error("\n[spike] warning: expected reply to contain 'billing-spike-ok'");
      process.exitCode = 2;
    }
  } catch (err) {
    console.error("\n[spike] failed:", err?.message ?? err);
    process.exitCode = 1;
  } finally {
    // On Windows, spawning via .cmd + shell often leaves the real agent
    // child alive after agent.kill(), which keeps this Node process open.
    try {
      rl.close();
    } catch {
      /* ignore */
    }
    try {
      agent.stdout?.destroy();
      agent.stderr?.destroy();
      agent.stdin?.end();
      agent.stdin?.destroy();
    } catch {
      /* ignore */
    }
    try {
      agent.kill("SIGTERM");
    } catch {
      /* ignore */
    }
    // Force exit — billing check is done; dangling ACP children must not hang.
    process.exit(process.exitCode ?? 0);
  }
}

main();
