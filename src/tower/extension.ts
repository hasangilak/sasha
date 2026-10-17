// The tower of sasha: pi with the terminals open in sasha as tools, on your
// Claude or Codex subscription. `sasha tower` writes this file each time it
// starts, so changes here don't last.
import { writeFileSync } from "node:fs";
import { join } from "node:path";

const env = process.env;

const engines: Record<string, { model: string; login: string }> = {
  claude: { model: env.SASHA_TOWER_CLAUDE_MODEL ?? "", login: "Anthropic (Claude Pro/Max)" },
  codex: { model: env.SASHA_TOWER_CODEX_MODEL ?? "", login: "ChatGPT Plus/Pro (Codex)" },
};

const provider = (model: string) => model.split("/")[0];

export default function (pi: any) {
  pi.registerMcpServer("sasha", {
    command: env.SASHA_EXE,
    args: ["mcp"],
    exposure: "direct",
    // Waiting on a session can take a while.
    timeout: 3700,
    description: "The terminals open in sasha, and the Claude Code and Codex sessions in them",
  });

  // Not signed in yet: say how, once.
  pi.on("session_start", async (_event: any, ctx: any) => {
    if (ctx.model && !ctx.modelRegistry.hasConfiguredAuth(ctx.model)) {
      const engine = Object.values(engines).find((e) => provider(e.model) === ctx.model.provider);
      const login = engine?.login ?? ctx.model.provider;
      ctx.ui.notify(`Sign in once: /login, then ${login}. /claude and /codex switch subscriptions.`, "warning");
    }
  });

  for (const [name, engine] of Object.entries(engines)) {
    pi.registerCommand(name, {
      description: `Run the tower on your ${engine.login} subscription`,
      handler: async (_args: string, ctx: any) => {
        const [wanted, id] = engine.model.split("/");
        const model =
          ctx.modelRegistry.find(wanted, id) ??
          ctx.modelRegistry.getAvailable().find((m: any) => m.provider === wanted);
        if (!model || !(await pi.setModel(model))) {
          ctx.ui.notify(`Sign in first: /login, then ${engine.login}. Then /${name} again.`, "warning");
          return;
        }
        // The tower starts on it next time too.
        if (env.SASHA_TOWER_DIR) {
          writeFileSync(join(env.SASHA_TOWER_DIR, "engine"), `${name}\n`);
        }
        ctx.ui.notify(`The tower runs on ${model.name ?? model.id} now`, "info");
      },
    });
  }
}
