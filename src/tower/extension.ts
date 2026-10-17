// The tower of sasha, on Codex: pi with the terminals open in sasha as
// tools. `sasha tower` writes this file each time it starts, so changes
// here don't last.

const sasha = process.env.SASHA_EXE ?? "sasha";

export default function (pi: any) {
  pi.registerMcpServer("sasha", {
    command: sasha,
    args: ["mcp"],
    // It's the tower asking: what it prompts becomes one of its sessions.
    env: { ...process.env, SASHA_TOWER: "1" },
    exposure: "direct",
    // Waiting on a session can take a while.
    timeout: 3700,
    description: "The terminals open in sasha, and the Claude Code and Codex sessions in them",
  });

  // The news from its sessions goes in before each turn.
  // So does who the user called, by name ("hey Bella").
  pi.on("before_agent_start", async (event: any) => {
    const result = await pi.exec(sasha, ["inbox", "--prompt", event.prompt ?? ""], { timeout: 5000 });
    const news = (result.stdout ?? "").trim();
    if (news) {
      return { message: { customType: "sasha", content: news, display: true } };
    }
  });

  // Its answer, said aloud when voice replies are on.
  pi.on("agent_settled", async () => {
    await pi.exec(sasha, ["speak"], { timeout: 10000 });
  });

  // Not signed in yet: say how, once.
  pi.on("session_start", async (_event: any, ctx: any) => {
    if (ctx.model && !ctx.modelRegistry.hasConfiguredAuth(ctx.model)) {
      ctx.ui.notify("Sign in once: /login, then ChatGPT Plus/Pro (Codex).", "warning");
    }
  });
}
