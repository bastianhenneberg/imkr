// imkr: reports pi's state (working, blocked, idle) to the imkr pane it runs
// in, as the hooks do for Claude Code. Installed by `imkr hooks --install`,
// which overwrites this file; does nothing outside imkr.
// @ts-nocheck

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";

const pane = process.env.IMKR_PANE;

// imkr on the PATH, else where its installs put it: pi may run from a
// shell whose PATH lacks them.
function imkr(): string {
  const home = process.env.HOME ?? "";
  for (const dir of (process.env.PATH ?? "").split(":")) {
    if (dir && existsSync(join(dir, "imkr"))) return join(dir, "imkr");
  }
  for (const path of [join(home, ".cargo/bin/imkr"), join(home, ".local/bin/imkr")]) {
    if (existsSync(path)) return path;
  }
  return "imkr";
}

const bin = pane ? imkr() : "";

// `imkr status STATE`, with pi's session id on stdin as a hook's JSON, so
// that imkr brings the conversation back after a restart.
function report(state: string, session?: string): void {
  try {
    const child = spawn(bin, ["status", state, "--pane", pane], {
      stdio: ["pipe", "ignore", "ignore"],
    });
    child.on("error", () => {});
    child.stdin.on("error", () => {});
    child.stdin.end(session ? JSON.stringify({ session_id: session }) : "");
    child.unref?.();
  } catch {
    // imkr is gone: nothing to tell.
  }
}

function failed(event: any): boolean {
  const messages = Array.isArray(event?.messages) ? event.messages : [];
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    if (messages[i]?.role === "assistant") return messages[i]?.stopReason === "error";
  }
  return false;
}

export default function (pi) {
  if (!pane) return;
  let session: string | undefined;
  let active = false;
  let later: ReturnType<typeof setTimeout> | undefined;

  pi.on("session_start", (_event, ctx) => {
    try {
      const id = ctx?.sessionManager?.getSessionId?.();
      session = typeof id === "string" && id ? id : undefined;
    } catch {
      session = undefined;
    }
    report("idle", session);
  });

  pi.on("agent_start", () => {
    clearTimeout(later);
    active = true;
    report("working", session);
  });

  pi.on("agent_end", (event) => {
    // Pi may end twice; only the first end of a turn counts.
    if (!active) return;
    active = false;
    // An error pi retries by itself starts the next turn soon: only one
    // that stays needs the user.
    const error = failed(event);
    later = setTimeout(() => report(error ? "blocked" : "idle", session), error ? 3000 : 250);
    later.unref?.();
  });

  pi.on("session_shutdown", () => {
    clearTimeout(later);
    report("clear");
  });
}
