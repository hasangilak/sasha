// The tower of sasha, on Codex: pi with the terminals open in sasha as
// tools. `sasha tower` writes this file each time it starts, so changes
// here don't last.

export default function (pi: any) {
  pi.registerMcpServer("sasha", {
    command: process.env.SASHA_EXE,
    args: ["mcp"],
    exposure: "direct",
    // Waiting on a session can take a while.
    timeout: 3700,
    description: "The terminals open in sasha, and the Claude Code and Codex sessions in them",
  });

  // Not signed in yet: say how, once.
  pi.on("session_start", async (_event: any, ctx: any) => {
    if (ctx.model && !ctx.modelRegistry.hasConfiguredAuth(ctx.model)) {
      ctx.ui.notify("Sign in once: /login, then ChatGPT Plus/Pro (Codex).", "warning");
    }
  });
}
