/**
 * grs-capture — pi extension that runs `grs watch` for the lifetime of
 * each pi session, capturing every turn into the project's grs journal.
 *
 * Design (recap of the seven grilling decisions):
 *   Q1=B  Subject: this extension is a thin wrapper; the @teach/ skill
 *           teaches the existing grs source.
 *   Q2=B+1 Trigger: the watcher fires on file changes; we don't need a
 *           turn_end hook because grs's notify-based watcher does the
 *           capture and dedupe for us.
 *   Q3=A  Integration: a small PR to grs added `grs watch` (headless
 *           watcher subcommand) — this extension spawns it.
 *   Q4=B+D Step: curriculum is concept-organized, in @teach/'s SKILL.md.
 *   Q5=A  Activation: always capture (when a .grs/ repo exists), opt-in
 *           teach via /skill:teach.
 *   Q6=C  Persistence: progress + lesson notes live at .teach/ in the
 *           project root, not in grs.
 *   Q7=B  Pedagogy: read-and-explain + small reversible exercise per
 *           topic, diff reviewed against the previous snap.
 *
 * Lifecycle:
 *   session_start    → check for .grs/, spawn `grs watch --root <cwd>`
 *                      as a child process, store the handle.
 *   session_shutdown → send SIGTERM, wait briefly, SIGKILL if needed.
 *
 * Behaviour:
 *   - If `grs` is not on PATH, notify once and skip capture.
 *   - If `.grs/` does not exist in ctx.cwd, notify once and skip.
 *     (We don't auto-init — that's a one-time opt-in the user makes by
 *     running `grs` once in the project.)
 *   - If the watcher exits unexpectedly before session_shutdown, we
 *     surface a notification. We don't restart it; the user can /reload.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";

export default function (pi: ExtensionAPI) {
  // Per-extension-instance state. The ExtensionAPI instance lives for
  // the duration of one session (torn down on session_shutdown, fresh
  // instance for the next session), so closure variables are enough —
  // no need for pi.appendEntry.
  let watcher: ChildProcess | null = null;

  pi.on("session_start", (_event, ctx) => {
    const cwd = ctx.cwd;

    // Gate 1: is there a grs repo here? If not, notify and skip.
    if (!existsSync(join(cwd, ".grs"))) {
      ctx.ui.notify(
        `grs-capture: no .grs/ in ${cwd}; run \`grs\` once to initialize, then /reload.`,
        "warning",
      );
      return;
    }

    // Gate 2: is the `grs` binary on PATH? Probe by trying to spawn
    // `grs watch --help`; an ENOENT fires the 'error' event, otherwise
    // the 'exit' event fires. The probe is best-effort: if it hangs,
    // the timeout kills it and we assume present.
    probeBinary("grs").then((ok) => {
      if (!ok) {
        ctx.ui.notify(
          "grs-capture: `grs` not found on PATH; install grs to enable capture.",
          "warning",
        );
        return;
      }

      // Spawn the headless watcher. We pass --root so the watcher
      // operates on the cwd even if the user invoked pi from a
      // subdirectory of the project. We don't pass --session-name:
      // an existing open session is reused; if none exists, the
      // watcher's default (project dir name) applies.
      const child = spawn("grs", ["watch", "--root", cwd], {
        cwd,
        // Detach from the parent stdio so the watcher's tracing output
        // (and any panics) go to its own stderr, not into pi's UI.
        // stderr is piped (not ignored) so we can surface the real
        // error message in the unexpected-exit warning. The watcher's
        // normal output is mostly quiet (a save log every few seconds
        // under load, nothing when idle) so this doesn't spam the user.
        stdio: ["ignore", "ignore", "pipe"],
        detached: false,
      });

      // Buffer the tail of stderr so we can show the user what the
      // watcher was complaining about when it dies. Bounded to ~20
      // lines to keep memory in check.
      const stderrTail: string[] = [];
      child.stderr?.on("data", (chunk: Buffer) => {
        const text = chunk.toString();
        for (const line of text.split("\n")) {
          if (line.length > 0) {
            stderrTail.push(line);
            if (stderrTail.length > 20) stderrTail.shift();
          }
        }
      });

      child.on("exit", (code, signal) => {
        const wasOurs = watcher === child;
        if (wasOurs) watcher = null;
        // If the watcher exits before session_shutdown, that's
        // unexpected (we only send SIGTERM ourselves). Surface it
        // with the watcher's last stderr lines so the user can
        // diagnose without digging through their own log files.
        if (wasOurs && code !== 0 && code !== null && signal !== "SIGTERM") {
          const tail = stderrTail.length
            ? `\n${stderrTail.slice(-5).join("\n")}`
            : "";
          ctx.ui.notify?.(
            `grs-capture: watcher exited unexpectedly (code=${code}, signal=${signal ?? "none"})${tail}`,
            "warning",
          );
          ctx.ui.setStatus?.("grs-capture", undefined);
        }
      });

      child.on("error", (err) => {
        ctx.ui.notify?.(
          `grs-capture: failed to spawn watcher: ${err.message}`,
          "error",
        );
      });

      watcher = child;
      ctx.ui.setStatus?.(
        "grs-capture",
        `watching ${cwd} (pid ${child.pid ?? "?"})`,
      );
    });
  });

  pi.on("session_shutdown", async (_event, ctx) => {
    if (!watcher) return;
    const child = watcher;
    watcher = null;

    // Tell the watcher to stop cleanly. grs watch handles SIGTERM
    // (and SIGINT) by exiting the event loop, releasing the project
    // lock, and returning.
    const exited = new Promise<void>((resolve) => {
      child.once("exit", () => resolve());
    });
    child.kill("SIGTERM");

    // Give it a moment to exit cleanly. The watcher drops the lock
    // and writes its exit log; a few seconds is plenty.
    const clean = await Promise.race([
      exited.then(() => true),
      new Promise<false>((resolve) => setTimeout(() => resolve(false), 2000)),
    ]);

    if (!clean) {
      // Watcher didn't exit on SIGTERM (shouldn't happen, but be safe).
      child.kill("SIGKILL");
      await exited;
    }

    ctx.ui.setStatus?.("grs-capture", undefined);
  });
}

/** Probe whether `binary` is on PATH by trying to spawn it. The 'error'
 *  event fires with ENOENT when the binary is missing; otherwise the
 *  'exit' event fires (regardless of exit code, since --help may exit
 *  non-zero). The timeout protects against hangs on weird inputs.
 */
function probeBinary(binary: string): Promise<boolean> {
  return new Promise((resolve) => {
    const p = spawn(binary, ["watch", "--help"], { stdio: "ignore" });
    let settled = false;
    const settle = (v: boolean) => {
      if (settled) return;
      settled = true;
      resolve(v);
    };
    p.on("error", () => settle(false));
    p.on("exit", () => settle(true));
    setTimeout(() => {
      p.kill();
      settle(true);
    }, 1000);
  });
}
